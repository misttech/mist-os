// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        cap::{
            AnyCapability, AnyCloneCapability, Capability, Remote, TryIntoOpen, TryIntoOpenError,
        },
        open::Open,
    },
    anyhow::{Context, Error},
    fidl::endpoints::create_request_stream,
    fidl_fuchsia_component_bedrock as fbedrock, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon as zx,
    fuchsia_zircon::HandleBased,
    futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        future::BoxFuture,
        FutureExt, SinkExt, TryStreamExt,
    },
    std::collections::hash_map::Entry,
    std::collections::HashMap,
    vfs::{
        directory::{
            entry::DirectoryEntry,
            helper::{AlreadyExists, DirectlyMutable},
            immutable::simple as pfs,
        },
        execution_scope::ExecutionScope,
        name::{Name, ParseNameError},
        path::Path,
    },
};

pub type Key = String;

/// A capability that represents a dictionary of capabilities.
#[derive(Debug, Clone)]
pub struct Dict<T> {
    pub entries: HashMap<Key, T>,

    /// When an external request tries to access a non-existent entry,
    /// the name of the entry will be sent using `not_found`.
    not_found: UnboundedSender<Key>,
}

impl<T> Dict<T> {
    /// Creates an empty dictionary.
    pub fn new() -> Self {
        Dict { entries: HashMap::new(), not_found: unbounded().0 }
    }

    /// Creates an empty dictionary. When an external request tries to access a non-existent entry,
    /// the name of the entry will be sent using `not_found`.
    pub fn new_with_not_found(not_found: UnboundedSender<Key>) -> Self {
        Dict { entries: HashMap::new(), not_found }
    }
}

impl<T: ?Sized + Capability> Dict<Box<T>>
where
    Box<T>: TryFrom<zx::Handle>,
{
    /// Serve the `fuchsia.component.bedrock.Dict` protocol for this `Dict`.
    pub async fn serve_dict(
        &mut self,
        mut stream: fbedrock::DictRequestStream,
    ) -> Result<(), Error> {
        // Tasks that serve the zx handles for entries removed from this dict.
        let mut entry_tasks = vec![];

        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fbedrock::DictRequest::Insert { key, value, responder, .. } => {
                    let result = match self.entries.entry(key) {
                        Entry::Occupied(_) => Err(fbedrock::DictError::AlreadyExists),
                        Entry::Vacant(entry) => match Box::<T>::try_from(value) {
                            Ok(cap) => {
                                entry.insert(cap);
                                Ok(())
                            }
                            Err(_) => Err(fbedrock::DictError::BadHandle),
                        },
                    };
                    responder.send(result).context("failed to send response")?;
                }
                fbedrock::DictRequest::Remove { key, responder } => {
                    let cap = self.entries.remove(&key);
                    let result = match cap {
                        Some(cap) => {
                            let (handle, fut) = cap.to_zx_handle();
                            if let Some(fut) = fut {
                                entry_tasks.push(fasync::Task::spawn(fut));
                            }
                            Ok(handle)
                        }
                        None => {
                            // Ignore the result of sending. The receiver is free to break away to
                            // ignore all the not-found errors.
                            let _ = self.not_found.send(key);
                            Err(fbedrock::DictError::NotFound)
                        }
                    };
                    responder.send(result).context("failed to send response")?;
                }
            }
        }

        Ok(())
    }

    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (dict_client_end, dict_stream) =
            create_request_stream::<fbedrock::DictMarker>().unwrap();

        let fut = async move {
            let mut dict = *self;
            dict.serve_dict(dict_stream).await.expect("failed to serve Dict");
        };

        (dict_client_end.into_handle(), Some(fut.boxed()))
    }

    fn try_into_open(self: Box<Self>) -> Result<Open, TryIntoOpenError> {
        let dir = pfs::simple();
        for (key, value) in self.entries.into_iter() {
            let key: Name =
                key.try_into().map_err(|e: ParseNameError| TryIntoOpenError::ParseNameError(e))?;
            match dir.add_entry_impl(key, value.try_into_open()?.into_remote(), false) {
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

impl Remote for Dict<AnyCapability> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        self.to_zx_handle()
    }
}

impl Remote for Dict<AnyCloneCapability> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        self.to_zx_handle()
    }
}

impl TryIntoOpen for Dict<AnyCapability> {
    fn try_into_open(self: Box<Self>) -> Result<Open, TryIntoOpenError> {
        self.try_into_open()
    }
}

impl TryIntoOpen for Dict<AnyCloneCapability> {
    fn try_into_open(self: Box<Self>) -> Result<Open, TryIntoOpenError> {
        self.try_into_open()
    }
}

impl Capability for Dict<AnyCapability> {}
impl Capability for Dict<AnyCloneCapability> {}

/// The trait implemented by all dictionary types.
pub trait SomeDict {
    fn list(&self) -> Vec<&Key>;
    fn remove(&mut self, k: &Key) -> Option<AnyCapability>;
}

impl SomeDict for Dict<AnyCapability> {
    fn list(&self) -> Vec<&Key> {
        self.entries.keys().collect()
    }

    fn remove(&mut self, key: &Key) -> Option<AnyCapability> {
        self.entries.remove(key)
    }
}

impl SomeDict for Dict<AnyCloneCapability> {
    fn list(&self) -> Vec<&Key> {
        self.entries.keys().collect()
    }

    fn remove(&mut self, key: &Key) -> Option<AnyCapability> {
        self.entries.remove(key).map(|c| c.into_any_capability())
    }
}

impl TryFrom<AnyCapability> for Box<dyn SomeDict> {
    type Error = AnyCapability;

    fn try_from(value: AnyCapability) -> Result<Self, Self::Error> {
        if value.as_any().is::<Dict<AnyCapability>>() {
            return Ok(value.into_any().downcast::<Dict<AnyCapability>>().unwrap());
        }
        if value.as_any().is::<Dict<AnyCloneCapability>>() {
            return Ok(value.into_any().downcast::<Dict<AnyCloneCapability>>().unwrap());
        }
        Err(value)
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            dict::*,
            handle::{CloneHandle, Handle},
        },
        anyhow::{anyhow, Error},
        assert_matches::assert_matches,
        fidl::endpoints::{create_endpoints, create_proxy_and_stream, Proxy},
        fidl_fuchsia_component_bedrock as fbedrock,
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
        let mut dict = Dict::<AnyCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
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
        let mut dict = Dict::<AnyCapability>::new();

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        // Put the event into the dict as a `Handle`.
        dict.entries.insert(CAP_KEY.to_string(), Box::new(Handle::from(event.into_handle())));
        assert_eq!(dict.entries.len(), 1);

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
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
        let mut dict = Dict::<AnyCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
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
            assert_matches!(result, Err(fbedrock::DictError::AlreadyExists));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that the `Dict.Remove` returns `NOT_FOUND` when there is no item with the given key.
    #[fuchsia::test]
    async fn remove_not_found() -> Result<(), Error> {
        let mut dict = Dict::<AnyCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            // Removing an item from an empty dict should fail.
            let result = dict_proxy.remove(CAP_KEY).await.context("failed to call Remove")?;
            assert_matches!(result, Err(fbedrock::DictError::NotFound));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that `Dict<AnyCloneCapability>.Insert` let the client to insert a cloneable handle.
    #[fuchsia::test]
    async fn clone_insert() -> Result<(), Error> {
        let mut dict = Dict::<AnyCloneCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();
            let handle = event.into_handle();

            let result =
                dict_proxy.insert(CAP_KEY, handle).await.context("failed to call Insert")?;
            assert_matches!(result, Ok(()));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that `Dict<AnyCloneCapability>.Insert` returns `BAD_HANDLE` when the handle
    /// does not have ZX_RIGHT_DUPLICATE.
    #[fuchsia::test]
    async fn clone_insert_bad_handle() -> Result<(), Error> {
        let mut dict = Dict::<AnyCloneCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();

            // Create a handle without ZX_RIGHT_DUPLICATE.
            let handle =
                event.as_handle_ref().duplicate(zx::Rights::BASIC - zx::Rights::DUPLICATE).unwrap();

            let result =
                dict_proxy.insert(CAP_KEY, handle).await.context("failed to call Insert")?;
            assert_matches!(result, Err(fbedrock::DictError::BadHandle));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that cloning a `Dict<AnyCloneCapability>` with an item results in a Dict that
    /// contains a handle to the same object.
    #[fuchsia::test]
    async fn clone_contains_same_object() -> Result<(), Error> {
        let mut dict = Dict::<AnyCloneCapability>::new();

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        // Put the event into the dict as a `CloneHandle`.
        dict.entries.insert(
            CAP_KEY.to_string(),
            Box::new(CloneHandle::try_from(event.into_handle()).unwrap()),
        );
        assert_eq!(dict.entries.len(), 1);

        let dict_clone = dict.clone();

        assert_eq!(dict_clone.entries.len(), 1);
        let event_clone = dict.entries.remove(CAP_KEY).unwrap();

        let (handle, _) = event_clone.to_zx_handle();
        let got_koid = handle.as_handle_ref().get_koid().unwrap();

        assert_eq!(got_koid, expected_koid);

        Ok(())
    }

    #[fuchsia::test]
    async fn try_into_open_error_not_supported() {
        let event = zx::Event::create();
        let mut dict = Dict::<AnyCapability>::new();
        dict.entries.insert(CAP_KEY.to_string(), Box::new(Handle::from(event.into_handle())));
        assert_matches!(Box::new(dict).try_into_open(), Err(TryIntoOpenError::DoesNotSupportOpen));
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
        let mut dict = Dict::<AnyCapability>::new();
        // This string is too long to be a valid fuchsia.io name.
        let bad_name = "a".repeat(10000);
        dict.entries.insert(bad_name, Box::new(placeholder_open));
        assert_matches!(Box::new(dict).try_into_open(), Err(TryIntoOpenError::ParseNameError(_)));
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
        let mut dict = Dict::<AnyCapability>::new();
        dict.entries.insert(CAP_KEY.to_string(), Box::new(open));
        let dict_open = Box::new(dict).try_into_open().expect("convert dict into Open capability");

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
        let mut inner_dict = Dict::<AnyCapability>::new();
        inner_dict.entries.insert(CAP_KEY.to_string(), Box::new(open));
        let mut dict = Dict::<AnyCapability>::new();
        dict.entries.insert(CAP_KEY.to_string(), Box::new(inner_dict));

        let dict_open = Box::new(dict).try_into_open().expect("convert dict into Open capability");

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
