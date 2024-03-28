// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use fidl::endpoints::Proxy;
use fidl_fuchsia_io as fio;
use futures::future::Either;
use futures::FutureExt;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::Mutex;

use crate::error::{Error, Result};
use crate::interpreter::Interpreter;
use crate::value::{
    InUseHandle, PlaygroundValue, ReplayableIterator, ReplayableIteratorCursor, Value, ValueExt,
};

macro_rules! error {
    ($($data:tt)*) => { Error::from(anyhow!($($data)*)) };
}

impl Interpreter {
    /// Add built-in commands to the playground's global scope.
    pub(crate) async fn add_builtins(&self, executor_fut: &mut (impl Future<Output = ()> + Unpin)) {
        let fut = async {
            let inner_weak = Arc::downgrade(&self.inner);
            let fs_root_getter =
                self.get_runnable("$fs_root").await.expect("Could not compile fs_root getter");
            let pwd_getter = self.get_runnable("$pwd").await.expect("Could not compile pwd getter");
            self.add_command("open", move |mut args, underscore| {
                let inner_weak = inner_weak.clone();
                let fs_root_getter = fs_root_getter.clone();
                let pwd_getter = pwd_getter.clone();
                async move {
                    let Some(inner) = inner_weak.upgrade() else {
                        return Err(error!("Interpreter died"));
                    };
                    let Some(arg) = args.pop().or(underscore) else {
                        return Err(error!("open requires exactly one argument or an input"));
                    };
                    if !args.is_empty() {
                        return Err(error!("open requires at most one argument"));
                    }

                    let path = match arg {
                        Value::String(path) => path,
                        _ => return Err(error!("open argument must be a path")),
                    };
                    let fs_root = fs_root_getter().await?;
                    let pwd = pwd_getter().await?;

                    inner.open(path, fs_root, pwd).await
                }
            })
            .await
            .expect("Failed to install `open` command");

            let inner_weak = Arc::downgrade(&self.inner);
            self.add_command("req", move |mut args, under| {
                let inner_weak = inner_weak.clone();
                async move {
                    let Some(inner) = inner_weak.upgrade() else {
                        return Err(error!("Interpreter died"));
                    };

                    if args.len() != 1 {
                        return Err(error!("req takes exactly one argument"));
                    }

                    let closure = args.pop().unwrap();

                    let (server, client) = InUseHandle::new_endpoints();
                    let server = Value::OutOfLine(PlaygroundValue::InUseHandle(server));
                    let client = Value::OutOfLine(PlaygroundValue::InUseHandle(client));
                    let _ = inner.invoke_value(closure, vec![server], under).await?;
                    Ok(client)
                }
            })
            .await
            .expect("Failed to install `req` command");

            let inner_weak = Arc::downgrade(&self.inner);
            self.add_command("read", move |mut args, under| {
                let inner_weak = inner_weak.clone();
                async move {
                    let Some(inner) = inner_weak.upgrade() else {
                        return Err(error!("Interpreter died"));
                    };

                    let Some(value) = args.pop().or(under) else {
                        return Err(error!("read takes one argument or an input"));
                    };

                    if !args.is_empty() {
                        return Err(error!("read takes at most one argument"));
                    }

                    if let Ok(client) =
                        value.try_client_channel(inner.lib_namespace(), "fuchsia.io/File")
                    {
                        let proxy = fio::FileProxy::from_channel(
                            fuchsia_async::Channel::from_channel(client),
                        );
                        Ok(Value::OutOfLine(PlaygroundValue::Iterator(ReplayableIterator::from(
                            FileCursor(
                                Arc::new(FileCursorInner {
                                    proxy,
                                    cache: Mutex::new(FileCursorCache {
                                        cached: VecDeque::new(),
                                        cache_positions: [(0, 1)].into_iter().collect(),
                                    }),
                                }),
                                0,
                            ),
                        ))))
                    } else {
                        Err(error!("value cannot be read"))
                    }
                }
            })
            .await
            .expect("Failed to install `read` command");
        };

        let Either::Left(_) = futures::future::select(pin!(fut), executor_fut).await else {
            unreachable!("Executor hung up early");
        };
    }
}

/// Cached data read from a file that's being read through a ReplayableIterator
struct FileCursorCache {
    /// Contains bytes read from the file. The section of the file represented
    /// starts at the offset of the lowest key in [`cached_positions`].
    cached: VecDeque<u8>,
    /// Positions of the file that we'd like to cache. Each key in the map is an
    /// offset within the file itself where a [`FileCursor`] is currently
    /// pointed. Each value is how many such cursors are pointed there. The
    /// lowest-value key is the offset from which the data in [`cached`] was read.
    cache_positions: BTreeMap<usize, usize>,
}

/// Shared portion of [`FileCursor`]
struct FileCursorInner {
    proxy: fio::FileProxy,
    cache: Mutex<FileCursorCache>,
}

impl FileCursorInner {
    /// Amount of data to attempt to read every time we read from the file.
    const READ_BLOCK_SIZE: u64 = 64;

    /// Read data from the underlying file at the given offset. Makes use of our
    /// contained cache to avoid duplicate reads.
    async fn read(&self, pos: usize) -> Result<Option<Value>> {
        let mut bytes = Vec::new();
        loop {
            {
                let mut cache = self.cache.lock().unwrap();
                let cache_pos = *cache
                    .cache_positions
                    .first_key_value()
                    .expect("File cursor unregistered position from cache!")
                    .0;
                if !bytes.is_empty() {
                    cache.cached.extend(bytes.drain(..));
                }
                assert!(cache_pos <= pos, "Iterator cursor precedes retained state!");
                let pos = pos - cache_pos;
                if let Some(byte) = cache.cached.get(pos).copied() {
                    return Ok(Some(Value::U8(byte)));
                }
            }
            bytes = self
                .proxy
                .read(Self::READ_BLOCK_SIZE)
                .await?
                .map_err(|i| error!("read failed: {i}"))?;
            if bytes.is_empty() {
                return Ok(None);
            }
        }
    }
}

/// [`ReplayableIteratorCursor`] that yields the bytes of a file.
struct FileCursor(Arc<FileCursorInner>, usize);

impl Drop for FileCursor {
    fn drop(&mut self) {
        // Tell the cache that there is one less cursor looking at the given offset.
        let mut cache = self.0.cache.lock().unwrap();
        *cache.cache_positions.get_mut(&self.1).expect("File cursor has no cache position!") -= 1;

        // Get the offset of the leftmost byte of the file which is currently in the cache.
        let start = *cache.cache_positions.first_key_value().unwrap().0;

        // If no cursors are currently pointed to the leftmost cached position,
        // discard the entry for that position. Repeat this until we have a
        // leftmost entry that is actually used by an existing cursor.
        while let Some(entry) = cache.cache_positions.first_entry().filter(|x| *x.get() == 0) {
            entry.remove();
        }

        // If we removed the leftmost entry, discard cached data until our cache
        // starts at the new leftmost entry.
        if let Some((&end, _)) = cache.cache_positions.first_key_value().filter(|x| *x.0 != start) {
            let len = cache.cached.len();
            cache.cached.drain(..(std::cmp::min(end - start, len)));
        }
    }
}

impl ReplayableIteratorCursor for FileCursor {
    fn next(
        self: Arc<Self>,
    ) -> (
        futures::prelude::future::BoxFuture<'static, Result<Option<Value>>>,
        Arc<dyn ReplayableIteratorCursor>,
    ) {
        let next = Arc::new(FileCursor(Arc::clone(&self.0), self.1 + 1));
        self.0
            .cache
            .lock()
            .unwrap()
            .cache_positions
            .entry(self.1 + 1)
            .and_modify(|e| *e += 1)
            .or_insert(1);
        let yielder = async move { self.0.read(self.1).await }.boxed();
        (yielder, next)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test::*;
    use fidl_codec::Value as FidlValue;
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;

    #[fuchsia::test]
    async fn open() {
        Test::test("open /test")
            .with_fidl()
            .with_standard_test_dirs()
            .check_async(|value| async move {
                assert!(value.is_client("fuchsia.io/Directory"));
                let Value::ClientEnd(endpoint, _) = value else {
                    panic!();
                };
                let proxy = fidl::endpoints::ClientEnd::<fio::DirectoryMarker>::from(endpoint)
                    .into_proxy()
                    .unwrap();
                let mut dirs = fuchsia_fs::directory::readdir(&proxy).await.unwrap();
                dirs.sort_by(|x, y| x.name.cmp(&y.name));
                let [foo, neils_philosophy] = dirs.try_into().unwrap();
                assert_eq!("foo", foo.name);
                assert_eq!("neils_philosophy", neils_philosophy.name);
            })
            .await
    }

    #[fuchsia::test]
    async fn req() {
        Test::test(format!(
            "open /test | req \\i _ @Clone {{ flags: {}, object: $i }}",
            fio::OpenFlags::DESCRIBE.bits()
        ))
        .with_fidl()
        .with_standard_test_dirs()
        .check_async(|value| async move {
            let Value::OutOfLine(PlaygroundValue::InUseHandle(i)) = value else {
                panic!();
            };
            let endpoint = i.take_client("fuchsia.io/Node").unwrap();
            let FidlValue::ClientEnd(endpoint, _) = endpoint else { unreachable!() };
            let proxy =
                fidl::endpoints::ClientEnd::<fio::NodeMarker>::from(endpoint).into_proxy().unwrap();
            let event = proxy.take_event_stream().next().await.unwrap().unwrap();
            let fio::NodeEvent::OnOpen_ { s: _, info } = event else { panic!() };
            let info = *info.unwrap();
            let fio::NodeInfoDeprecated::Directory(fio::DirectoryObject) = info else { panic!() };
        })
        .await
    }

    #[fuchsia::test]
    async fn read() {
        Test::test("open /test/neils_philosophy | read")
            .with_fidl()
            .with_standard_test_dirs()
            .check_async(|value| async move {
                let Value::OutOfLine(PlaygroundValue::Iterator(mut i)) = value else {
                    panic!();
                };
                let mut bytes = Vec::new();
                while let Some(byte) = i.next().await.unwrap() {
                    let Value::U8(byte) = byte else { panic!() };
                    bytes.push(byte);
                }

                assert_eq!(crate::test::NEILS_PHILOSOPHY, bytes.as_slice());
            })
            .await
    }

    #[fuchsia::test]
    async fn read_no_pipe() {
        Test::test("read {open /test/neils_philosophy}")
            .with_fidl()
            .with_standard_test_dirs()
            .check_async(|value| async move {
                let Value::OutOfLine(PlaygroundValue::Iterator(mut i)) = value else {
                    panic!();
                };
                let mut bytes = Vec::new();
                while let Some(byte) = i.next().await.unwrap() {
                    let Value::U8(byte) = byte else { panic!() };
                    bytes.push(byte);
                }

                assert_eq!(crate::test::NEILS_PHILOSOPHY, bytes.as_slice());
            })
            .await
    }
}
