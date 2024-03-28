// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use futures::future::Either;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;

use crate::error::Error;
use crate::interpreter::Interpreter;
use crate::value::{InUseHandle, PlaygroundValue, Value};

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
        };

        let Either::Left(_) = futures::future::select(pin!(fut), executor_fut).await else {
            unreachable!("Executor hung up early");
        };
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test::*;
    use crate::value::ValueExt;
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
                let dirs = fuchsia_fs::directory::readdir(&proxy).await.unwrap();
                let [subdir] = dirs.try_into().unwrap();
                assert_eq!("foo", subdir.name);
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
}
