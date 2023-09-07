// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCapability, AnyCast, Capability, Convert, Directory, Open, Remote, TryClone},
    anyhow::{Context, Error},
    fidl::endpoints::create_request_stream,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon as zx,
    fuchsia_zircon::HandleBased,
    futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        future::BoxFuture,
        FutureExt, SinkExt, TryStreamExt,
    },
    std::collections::btree_map::Entry,
    std::collections::BTreeMap,
    std::fmt::Debug,
    thiserror::Error,
    vfs::{
        directory::{
            entry::DirectoryEntry,
            helper::{AlreadyExists, DirectlyMutable},
            immutable::simple as pfs,
        },
        execution_scope::ExecutionScope,
        name::Name,
        path::Path,
    },
};

pub type Key = String;

/// A capability that represents a dictionary of capabilities.
#[derive(Capability, Debug)]
pub struct Dict {
    pub entries: BTreeMap<Key, AnyCapability>,

    /// When an external request tries to access a non-existent entry,
    /// the name of the entry will be sent using `not_found`.
    not_found: UnboundedSender<Key>,
}

impl Default for Dict {
    fn default() -> Self {
        Self::new()
    }
}

impl Dict {
    /// Creates an empty dictionary.
    pub fn new() -> Self {
        Dict { entries: BTreeMap::new(), not_found: unbounded().0 }
    }

    /// Creates an empty dictionary. When an external request tries to access a non-existent entry,
    /// the name of the entry will be sent using `not_found`.
    pub fn new_with_not_found(not_found: UnboundedSender<Key>) -> Self {
        Dict { entries: BTreeMap::new(), not_found }
    }

    /// Serve the `fuchsia.component.Dict` protocol for this `Dict`.
    pub async fn serve_dict(
        &mut self,
        mut stream: fsandbox::DictRequestStream,
    ) -> Result<(), Error> {
        // Tasks that serve the zx handles for entries removed from this dict.
        let mut entry_tasks = fasync::TaskGroup::new();

        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fsandbox::DictRequest::Insert { key, value, responder, .. } => {
                    let result = match self.entries.entry(key) {
                        Entry::Occupied(_) => Err(fsandbox::DictError::AlreadyExists),
                        Entry::Vacant(entry) => match AnyCapability::try_from(value) {
                            Ok(cap) => {
                                entry.insert(cap);
                                Ok(())
                            }
                            Err(_) => Err(fsandbox::DictError::BadHandle),
                        },
                    };
                    responder.send(result).context("failed to send response")?;
                }
                fsandbox::DictRequest::Remove { key, responder } => {
                    let cap = self.entries.remove(&key);
                    let result = match cap {
                        Some(cap) => {
                            let (handle, fut) = Box::new(cap).to_zx_handle();
                            if let Some(fut) = fut {
                                entry_tasks.spawn(fut);
                            }
                            Ok(handle)
                        }
                        None => {
                            // Ignore the result of sending. The receiver is free to break away to
                            // ignore all the not-found errors.
                            let _ = self.not_found.send(key);
                            Err(fsandbox::DictError::NotFound)
                        }
                    };
                    responder.send(result).context("failed to send response")?;
                }
                fsandbox::DictRequest::Read { responder } => {
                    let result = (|| {
                        let items = self
                            .entries
                            .iter()
                            .map(|(key, value)| {
                                let (handle, fut) = value.try_clone()?.to_zx_handle();
                                Ok((fsandbox::DictItem { key: key.clone(), value: handle }, fut))
                            })
                            .collect::<Result<Vec<_>, ()>>()
                            .map_err(|_| fsandbox::DictError::NotCloneable)?;
                        let items: Vec<_> = items
                            .into_iter()
                            .map(|(item, fut)| {
                                if let Some(fut) = fut {
                                    entry_tasks.spawn(fut)
                                }
                                item
                            })
                            .collect();
                        Ok(items)
                    })();
                    responder.send(result).context("failed to send response")?;
                }
            }
        }

        Ok(())
    }
}

impl Remote for Dict {
    fn to_zx_handle(mut self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (dict_client_end, dict_stream) =
            create_request_stream::<fsandbox::DictMarker>().unwrap();

        let fut = async move {
            self.serve_dict(dict_stream).await.expect("failed to serve Dict");
        };

        (dict_client_end.into_handle(), Some(fut.boxed()))
    }
}

impl TryInto<Open> for Dict {
    type Error = TryIntoOpenError;

    /// Convert this [Dict] capability into [Open] by recursively converting the entries
    /// to [Open], then building a VFS directory where each entry is a remote VFS node.
    /// The resulting [Open] capability will speak `fuchsia.io/Directory` when remoted.
    fn try_into(self: Self) -> Result<Open, Self::Error> {
        let dir = pfs::simple();
        for (key, value) in self.entries.into_iter() {
            let key: Name = key.try_into().map_err(TryIntoOpenError::ParseNameError)?;
            let open: Open =
                value.try_into().map_err(|_| TryIntoOpenError::ValueDoesNotSupportOpen)?;

            match dir.add_entry_impl(key, open.into_remote(), false) {
                Ok(()) => {}
                Err(AlreadyExists) => {
                    unreachable!("Dict items should be unique");
                }
            }
        }
        let not_found = self.not_found.clone();
        dir.clone().set_not_found_handler(Box::new(move |path| {
            // Ignore the result of sending. The receiver is free to break away to ignore all the
            // not-found errors.
            let _ = not_found.unbounded_send(path.to_owned());
        }));
        Ok(Open::new(
            move |scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                dir.clone().open(scope.clone(), flags, relative_path, server_end.into())
            },
            fio::DirentType::Directory,
            // TODO(https://fxbug.dev/129636): Remove RIGHT_READABLE when `opendir` no longer
            // requires READABLE.
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
        ))
    }
}

/// This error is returned when a [Dict] cannot be converted into an [Open] capability.
#[derive(Error, Debug)]
pub enum TryIntoOpenError {
    /// A key is not a valid `fuchsia.io` node name.
    #[error("key is not a valid `fuchsia.io` node name")]
    ParseNameError(#[from] vfs::name::ParseNameError),

    /// A value does not support converting into an [Open] capability.
    #[error("value does not support converting into an Open capability")]
    ValueDoesNotSupportOpen,
}

impl TryClone for Dict {
    fn try_clone(&self) -> Result<Self, ()> {
        let entries: BTreeMap<Key, AnyCapability> = self
            .entries
            .iter()
            .map(|(key, value)| Ok((key.clone(), value.try_clone()?)))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect();
        Ok(Self { entries, not_found: self.not_found.clone() })
    }
}

impl Convert for Dict {
    fn try_into_capability(self, type_id: std::any::TypeId) -> Result<Box<dyn std::any::Any>, ()> {
        if type_id == std::any::TypeId::of::<Self>() {
            return Ok(Box::new(self).into_any());
        } else if type_id == std::any::TypeId::of::<Open>() {
            let open: Open = self.try_into().map_err(|_| ())?;
            return Ok(Box::new(open));
        } else if type_id == std::any::TypeId::of::<Directory>() {
            let open: Open = self.try_into().map_err(|_| ())?;
            let directory: Directory = open.try_into().map_err(|_| ())?;
            return Ok(Box::new(directory));
        }
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{dict::*, Handle},
        anyhow::{anyhow, Error},
        assert_matches::assert_matches,
        fidl::endpoints::{create_endpoints, create_proxy_and_stream, Proxy},
        fidl_fuchsia_component_sandbox as fsandbox,
        fuchsia_fs::directory::DirEntry,
        fuchsia_zircon::{self as zx, AsHandleRef},
        futures::try_join,
        lazy_static::lazy_static,
        test_util::Counter,
    };

    const CAP_KEY: &str = "cap";

    /// Tests that the `Dict` contains an entry for a capability inserted via `Dict.Insert`,
    /// and that the value is the same capability.
    #[fuchsia::test]
    async fn serve_insert() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let client = async move {
            dict_proxy
                .insert(CAP_KEY, event.into_handle())
                .await
                .context("failed to call Insert")?
                .map_err(|err| anyhow!("failed to insert: {:?}", err))?;

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        // Inserting adds the entry to `entries`.
        assert_eq!(dict.entries.len(), 1);

        // The entry that was inserted should now be in `entries`.
        let (handle, _fut) = dict
            .entries
            .remove(CAP_KEY)
            .ok_or_else(|| anyhow!("not in entries after insert"))?
            .to_zx_handle();
        let got_koid = handle.get_koid().unwrap();
        assert_eq!(got_koid, expected_koid);

        Ok(())
    }

    /// Tests that removing an entry from the `Dict` via `Dict.Remove` yields the same capability
    /// that was previously inserted.
    #[fuchsia::test]
    async fn serve_remove() -> Result<(), Error> {
        let mut dict = Dict::new();

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        // Put the event into the dict as a `Handle`.
        dict.entries.insert(CAP_KEY.to_string(), Box::new(Handle::from(event.into_handle())));
        assert_eq!(dict.entries.len(), 1);

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let cap = dict_proxy
                .remove(CAP_KEY)
                .await
                .context("failed to call Remove")?
                .map_err(|err| anyhow!("failed to remove: {:?}", err))?;

            // The entry should be the same one that was previously inserted.
            let got_koid = cap.get_koid().unwrap();
            assert_eq!(got_koid, expected_koid);

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        // Removing the entry with Remove should remove it from `entries`.
        assert!(dict.entries.is_empty());

        Ok(())
    }

    /// Tests that `Dict.Insert` returns `ALREADY_EXISTS` when there is already an item with
    /// the same key.
    #[fuchsia::test]
    async fn insert_already_exists() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();

            // Insert an entry.
            dict_proxy
                .insert(CAP_KEY, event.into_handle())
                .await
                .context("failed to call Insert")?
                .map_err(|err| anyhow!("failed to insert: {:?}", err))?;

            let event = zx::Event::create();

            // Inserting again should return an error.
            let result = dict_proxy
                .insert(CAP_KEY, event.into_handle())
                .await
                .context("failed to call Insert")?;
            assert_matches!(result, Err(fsandbox::DictError::AlreadyExists));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that the `Dict.Remove` returns `NOT_FOUND` when there is no item with the given key.
    #[fuchsia::test]
    async fn remove_not_found() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            // Removing an item from an empty dict should fail.
            let result = dict_proxy.remove(CAP_KEY).await.context("failed to call Remove")?;
            assert_matches!(result, Err(fsandbox::DictError::NotFound));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    #[fuchsia::test]
    async fn serve_read() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictMarker>()?;
        let _server = fasync::Task::spawn(async move { dict.serve_dict(dict_stream).await });

        // Create two events and get the koids.
        let mut events: Vec<_> = (0..2).map(|_| zx::Event::create()).collect();
        let mut expected_koids: Vec<_> = (0..2).map(|i| events[i].get_koid()).collect();

        // Add the events to the dict.
        dict_proxy
            .insert("cap1", events.remove(0).into_handle())
            .await
            .context("failed to call Insert")?
            .map_err(|err| anyhow!("failed to insert: {:?}", err))?;
        dict_proxy
            .insert("cap2", events.remove(0).into_handle())
            .await
            .context("failed to call Insert")?
            .map_err(|err| anyhow!("failed to insert: {:?}", err))?;

        // Now read the entries back.
        let mut items = dict_proxy.read().await.unwrap().unwrap();
        assert_eq!(items.len(), 2);
        assert_matches!(
            items.remove(0),
            fsandbox::DictItem {
                key,
                value,
            }
            if key == "cap1" && value.get_koid() == expected_koids.remove(0)
        );
        assert_matches!(
            items.remove(0),
            fsandbox::DictItem {
                key,
                value,
            }
            if key == "cap2" && value.get_koid() == expected_koids.remove(0)
        );

        Ok(())
    }

    /// Tests that a Dict can be cloned after a client inserts a cloneable handle.
    #[fuchsia::test]
    async fn clone_insert() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();
            let handle = event.into_handle();

            let result =
                dict_proxy.insert(CAP_KEY, handle).await.context("failed to call Insert")?;
            assert_matches!(result, Ok(()));

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        let dict_clone = dict.try_clone().expect("failed to clone");
        assert_eq!(dict_clone.entries.len(), 1);

        Ok(())
    }

    /// Tests that a Dict cannot be cloned after a client inserts a handle that does not
    /// have ZX_RIGHT_DUPLICATE.
    #[fuchsia::test]
    async fn clone_insert_non_duplicate_handle() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();

            // Create a handle without ZX_RIGHT_DUPLICATE.
            let handle =
                event.as_handle_ref().duplicate(zx::Rights::BASIC - zx::Rights::DUPLICATE).unwrap();

            let result =
                dict_proxy.insert(CAP_KEY, handle).await.context("failed to call Insert")?;
            assert_matches!(result, Ok(()));

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        assert!(dict.try_clone().is_err());

        Ok(())
    }

    /// Tests that cloning a Dict with an item results in a Dict that contains a handle to
    /// the same object.
    #[fuchsia::test]
    async fn clone_contains_same_object() -> Result<(), Error> {
        let mut dict = Dict::new();

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        // Put the event into the dict as a `Handle`.
        dict.entries
            .insert(CAP_KEY.to_string(), Box::new(Handle::try_from(event.into_handle()).unwrap()));
        assert_eq!(dict.entries.len(), 1);

        let dict_clone = dict.try_clone().unwrap();

        assert_eq!(dict_clone.entries.len(), 1);
        let event_clone = dict.entries.remove(CAP_KEY).unwrap();

        let (handle, _) = event_clone.to_zx_handle();
        let got_koid = handle.as_handle_ref().get_koid().unwrap();

        assert_eq!(got_koid, expected_koid);

        Ok(())
    }

    #[fuchsia::test]
    fn try_into_open_error_not_supported() {
        let event = zx::Event::create();
        let mut dict = Dict::new();
        dict.entries.insert(CAP_KEY.to_string(), Box::new(Handle::from(event.into_handle())));
        assert_matches!(
            TryInto::<Open>::try_into(dict),
            Err(TryIntoOpenError::ValueDoesNotSupportOpen)
        );
    }

    #[fuchsia::test]
    async fn try_into_open_error_invalid_name() {
        let placeholder_open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  _relative_path: Path,
                  _server_end: zx::Channel| {
                unreachable!();
            },
            fio::DirentType::Directory,
            fio::OpenFlags::DIRECTORY,
        );
        let mut dict = Dict::new();
        // This string is too long to be a valid fuchsia.io name.
        let bad_name = "a".repeat(10000);
        dict.entries.insert(bad_name, Box::new(placeholder_open));
        assert_matches!(TryInto::<Open>::try_into(dict), Err(TryIntoOpenError::ParseNameError(_)));
    }

    /// Convert a dict `{ CAP_KEY: open }` to [Open].
    #[fuchsia::test]
    async fn try_into_open_success() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "bar");
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
            fio::OpenFlags::DIRECTORY,
        );
        let mut dict = Dict::new();
        dict.entries.insert(CAP_KEY.to_string(), Box::new(open));
        let dict_open: Open = dict.try_into().expect("convert dict into Open capability");

        let remote = dict_open.into_remote();
        let scope = ExecutionScope::new();
        let (dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        remote.clone().open(
            scope.clone(),
            fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            dir_server_end.into_channel().into(),
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, server_end) = zx::Channel::create();
        let dir = dir_client_end.channel();
        fdio::service_connect_at(dir, &format!("{CAP_KEY}/bar"), server_end).unwrap();
        fasync::Channel::from_channel(client_end).unwrap().on_closed().await.unwrap();
        assert_eq!(OPEN_COUNT.get(), 1);
    }

    /// Convert a dict `{ CAP_KEY: { CAP_KEY: open } }` to [Open].
    #[fuchsia::test]
    async fn try_into_open_success_nested() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "bar");
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
            fio::OpenFlags::DIRECTORY,
        );
        let mut inner_dict = Dict::new();
        inner_dict.entries.insert(CAP_KEY.to_string(), Box::new(open));
        let mut dict = Dict::new();
        dict.entries.insert(CAP_KEY.to_string(), Box::new(inner_dict));

        let dict_open: Open = dict.try_into().expect("convert dict into Open capability");

        let remote = dict_open.into_remote();
        let scope = ExecutionScope::new();
        let (dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        remote.clone().open(
            scope.clone(),
            fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            dir_server_end.into_channel().into(),
        );

        // List the outer directory and verify the contents.
        let dir = dir_client_end.into_proxy().unwrap();
        assert_eq!(
            fuchsia_fs::directory::readdir(&dir).await.unwrap(),
            vec![DirEntry { name: CAP_KEY.to_string(), kind: fio::DirentType::Directory },]
        );

        // Open the inner most capability.
        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, server_end) = zx::Channel::create();
        let dir = dir.into_channel().unwrap().into_zx_channel();
        fdio::service_connect_at(&dir, &format!("{CAP_KEY}/{CAP_KEY}/bar"), server_end).unwrap();
        fasync::Channel::from_channel(client_end).unwrap().on_closed().await.unwrap();
        assert_eq!(OPEN_COUNT.get(), 1)
    }
}
