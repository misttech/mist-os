// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{Context, Error};
use clonable_error::ClonableError;
use derivative::Derivative;
use fidl::endpoints::{create_request_stream, ClientEnd, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::TryStreamExt;
use std::{
    collections::{btree_map::Entry, BTreeMap},
    fmt::Debug,
    sync::{Arc, Mutex, MutexGuard},
};
use thiserror::Error;
use tracing::warn;
use vfs::{
    directory::{
        entry::DirectoryEntry,
        helper::{AlreadyExists, DirectlyMutable},
        immutable::simple as pfs,
    },
    execution_scope::ExecutionScope,
    name::{Name, ParseNameError},
    path::Path,
};

use crate::{registry, AnyCapability, Capability, ConversionError, Open};

pub type Key = String;

/// A capability that represents a dictionary of capabilities.
#[derive(Capability, Derivative)]
#[derivative(Debug)]
pub struct Dict {
    entries: Arc<Mutex<BTreeMap<Key, AnyCapability>>>,

    /// When an external request tries to access a non-existent entry,
    /// this closure will be invoked with the name of the entry.
    #[derivative(Debug = "ignore")]
    not_found: Arc<dyn Fn(Key) -> () + 'static + Send + Sync>,

    /// Tasks that serve [DictionaryIterator]s.
    #[derivative(Debug = "ignore")]
    iterator_tasks: fasync::TaskGroup,

    /// The FIDL representation of this `Dict`.
    ///
    /// This will be `Some` if was previously converted into a `ClientEnd`, such as by calling
    /// [into_fidl], and the capability is not currently in the registry.
    client_end: Option<ClientEnd<fsandbox::DictionaryMarker>>,
}

impl Default for Dict {
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            not_found: Arc::new(|_key: Key| {}),
            iterator_tasks: fasync::TaskGroup::new(),
            client_end: None,
        }
    }
}

impl Clone for Dict {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            not_found: self.not_found.clone(),
            iterator_tasks: fasync::TaskGroup::new(),
            client_end: None,
        }
    }
}

impl Dict {
    /// Creates an empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty dictionary. When an external request tries to access a non-existent entry,
    /// the name of the entry will be sent using `not_found`.
    pub fn new_with_not_found(not_found: impl Fn(Key) -> () + 'static + Send + Sync) -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            not_found: Arc::new(not_found),
            iterator_tasks: fasync::TaskGroup::new(),
            client_end: None,
        }
    }

    pub fn lock_entries(&self) -> MutexGuard<'_, BTreeMap<Key, AnyCapability>> {
        self.entries.lock().unwrap()
    }

    /// Creates a new Dict with entries cloned from this Dict.
    ///
    /// This is a shallow copy. Values are cloned, not copied, so are new references to the same
    /// underlying data.
    pub fn copy(&self) -> Self {
        let copy = Dict::new();
        copy.lock_entries().clone_from(&self.lock_entries());
        copy
    }

    /// Serve the `fuchsia.component.sandbox.Dictionary` protocol for this `Dict`.
    pub async fn serve_dict(
        &mut self,
        mut stream: fsandbox::DictionaryRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fsandbox::DictionaryRequest::Insert { key, value, responder, .. } => {
                    let result = match self.lock_entries().entry(key) {
                        Entry::Occupied(_) => Err(fsandbox::DictionaryError::AlreadyExists),
                        Entry::Vacant(entry) => match AnyCapability::try_from(value) {
                            Ok(cap) => {
                                entry.insert(cap);
                                Ok(())
                            }
                            Err(_) => Err(fsandbox::DictionaryError::BadCapability),
                        },
                    };
                    responder.send(result).context("failed to send response")?;
                }
                fsandbox::DictionaryRequest::Get { key, responder } => {
                    let result = match self.entries.lock().unwrap().get(&key) {
                        Some(cap) => Ok(cap.clone().into_fidl()),
                        None => {
                            (self.not_found)(key);
                            Err(fsandbox::DictionaryError::NotFound)
                        }
                    };
                    responder.send(result).context("failed to send response")?;
                }
                fsandbox::DictionaryRequest::Remove { key, responder } => {
                    let result = match self.entries.lock().unwrap().remove(&key) {
                        Some(cap) => Ok(cap.into_fidl()),
                        None => {
                            (self.not_found)(key);
                            Err(fsandbox::DictionaryError::NotFound)
                        }
                    };
                    responder.send(result).context("failed to send response")?;
                }
                fsandbox::DictionaryRequest::Read { responder } => {
                    let items = self
                        .lock_entries()
                        .iter()
                        .map(|(key, value)| {
                            let value = value.clone().into_fidl();
                            fsandbox::DictionaryItem { key: key.clone(), value }
                        })
                        .collect();
                    responder.send(items).context("failed to send response")?;
                }
                fsandbox::DictionaryRequest::Clone2 { request, control_handle: _ } => {
                    // The clone is registered under the koid of the client end.
                    let koid = request.basic_info().unwrap().related_koid;
                    let server_end: ServerEnd<fsandbox::DictionaryMarker> =
                        request.into_channel().into();
                    let stream = server_end.into_stream().unwrap();
                    self.clone().serve_and_register(stream, koid);
                }
                fsandbox::DictionaryRequest::Copy { request, .. } => {
                    // The copy is registered under the koid of the client end.
                    let koid = request.basic_info().unwrap().related_koid;
                    let server_end: ServerEnd<fsandbox::DictionaryMarker> =
                        request.into_channel().into();
                    let stream = server_end.into_stream().unwrap();
                    self.copy().serve_and_register(stream, koid);
                }
                fsandbox::DictionaryRequest::Enumerate { contents: server_end, .. } => {
                    let items = self
                        .lock_entries()
                        .iter()
                        .map(|(key, cap)| (key.clone(), cap.clone()))
                        .collect();
                    let stream = server_end.into_stream().unwrap();
                    let task = fasync::Task::spawn(serve_dict_iterator(items, stream));
                    self.iterator_tasks.add(task);
                }
                fsandbox::DictionaryRequest::Drain { contents: server_end, .. } => {
                    // Take out entries, replacing with an empty BTreeMap.
                    // They are dropped if the caller does not request an iterator.
                    let entries = {
                        let mut entries = self.lock_entries();
                        std::mem::replace(&mut *entries, BTreeMap::new())
                    };
                    if let Some(server_end) = server_end {
                        let items = entries.into_iter().collect();
                        let stream = server_end.into_stream().unwrap();
                        let task = fasync::Task::spawn(serve_dict_iterator(items, stream));
                        self.iterator_tasks.add(task);
                    }
                }
                fsandbox::DictionaryRequest::_UnknownMethod { ordinal, .. } => {
                    warn!("Received unknown Dict request with ordinal {ordinal}");
                }
            }
        }

        Ok(())
    }

    /// Serves the `fuchsia.sandbox.Dictionary` protocol for this Open and moves it into the registry.
    pub fn serve_and_register(self, stream: fsandbox::DictionaryRequestStream, koid: zx::Koid) {
        let mut dict = self.clone();
        let fut = async move {
            dict.serve_dict(stream).await.expect("failed to serve Dict");
        };

        // Move this capability into the registry.
        let task = fasync::Task::spawn(fut);
        registry::insert_with_task(Box::new(self), koid, task);
    }

    /// Sets this Dict's client end to the provided one.
    ///
    /// This should only be used to put a remoted client end back into the Dict after it is removed
    /// from the registry.
    pub(crate) fn set_client_end(&mut self, client_end: ClientEnd<fsandbox::DictionaryMarker>) {
        self.client_end = Some(client_end)
    }
}

impl From<Dict> for ClientEnd<fsandbox::DictionaryMarker> {
    fn from(mut dict: Dict) -> Self {
        dict.client_end.take().unwrap_or_else(|| {
            let (client_end, dict_stream) =
                create_request_stream::<fsandbox::DictionaryMarker>().unwrap();
            dict.serve_and_register(dict_stream, client_end.get_koid().unwrap());
            client_end
        })
    }
}

impl From<Dict> for fsandbox::Capability {
    fn from(dict: Dict) -> Self {
        Self::Dictionary(dict.into())
    }
}

/// This error is returned when a [Dict] cannot be converted into an [Open] capability.
#[derive(Error, Debug)]
enum TryIntoOpenError {
    /// A key is not a valid `fuchsia.io` node name.
    #[error("key is not a valid `fuchsia.io` node name")]
    ParseNameError(#[from] vfs::name::ParseNameError),

    /// A value could not be converted into an [Open] capability.
    #[error("value at '{key}' could not be converted into an Open capability")]
    ConvertIntoOpen {
        key: String,
        #[source]
        err: ConversionError,
    },
}

impl Capability for Dict {
    /// Convert this [Dict] capability into [Open] by recursively converting the entries
    /// to [Open], then building a VFS directory where each entry is a remote VFS node.
    /// The resulting [Open] capability will speak `fuchsia.io/Directory` when remoted.
    fn try_into_open(self: Self) -> Result<Open, ConversionError> {
        let dir = pfs::simple();
        for (key, value) in self.lock_entries().iter() {
            let open: Open = value.clone().try_into_open().map_err(|err| {
                ClonableError::from(anyhow::Error::from(TryIntoOpenError::ConvertIntoOpen {
                    key: key.clone(),
                    err,
                }))
            })?;
            let key: Name = key.clone().try_into().map_err(|err: ParseNameError| {
                ClonableError::from(anyhow::Error::from(TryIntoOpenError::from(err)))
            })?;

            match dir.add_entry_impl(key, open.into_remote(), false) {
                Ok(()) => {}
                Err(AlreadyExists) => {
                    unreachable!("Dict items should be unique");
                }
            }
        }
        let not_found = self.not_found.clone();
        dir.clone().set_not_found_handler(Box::new(move |path| {
            not_found(path.to_owned());
        }));
        Ok(Open::new(
            move |scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                dir.clone().open(scope.clone(), flags, relative_path, server_end.into())
            },
            fio::DirentType::Directory,
        ))
    }
}

/// Serves the `fuchsia.sandbox.DictionaryIterator` protocol, providing items from the given iterator.
async fn serve_dict_iterator(
    items: Vec<(Key, AnyCapability)>,
    mut stream: fsandbox::DictionaryIteratorRequestStream,
) {
    let mut chunks = items
        .chunks(fsandbox::MAX_DICTIONARY_ITEMS_CHUNK as usize)
        .map(|chunk: &[(Key, AnyCapability)]| chunk.to_vec())
        .collect::<Vec<_>>()
        .into_iter();

    while let Some(request) = stream.try_next().await.expect("failed to read request from stream") {
        match request {
            fsandbox::DictionaryIteratorRequest::GetNext { responder } => match chunks.next() {
                Some(chunk) => {
                    let items = chunk
                        .into_iter()
                        .map(|(key, value)| fsandbox::DictionaryItem {
                            key: key.to_string(),
                            value: value.into_fidl(),
                        })
                        .collect();
                    responder.send(items).expect("failed to send response");
                }
                None => {
                    responder.send(vec![]).expect("failed to send response");
                    return;
                }
            },
            fsandbox::DictionaryIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!("Received unknown DictionaryIterator request with ordinal {ordinal}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Data, Unit};
    use anyhow::Result;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, create_proxy, create_proxy_and_stream, Proxy};
    use fidl_fuchsia_unknown as funknown;
    use fuchsia_fs::directory::DirEntry;
    use futures::try_join;
    use lazy_static::lazy_static;
    use test_util::Counter;

    const CAP_KEY: &str = "cap";

    /// Tests that the `Dict` contains an entry for a capability inserted via `Dict.Insert`,
    /// and that the value is the same capability.
    #[fuchsia::test]
    async fn serve_insert() -> Result<()> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let value = Unit::default().into_fidl();
            dict_proxy
                .insert(CAP_KEY, value)
                .await
                .expect("failed to call Insert")
                .expect("failed to insert");
            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        let mut entries = dict.lock_entries();

        // Inserting adds the entry to `entries`.
        assert_eq!(entries.len(), 1);

        // The entry that was inserted should now be in `entries`.
        let any = entries.remove(CAP_KEY).expect("not in entries after insert");
        let cap: Unit = any.try_into().unwrap();
        assert_eq!(cap, Unit::default());

        Ok(())
    }

    /// Tests that removing an entry from the `Dict` via `Dict.Remove` yields the same capability
    /// that was previously inserted.
    #[fuchsia::test]
    async fn serve_remove() -> Result<(), Error> {
        let mut dict = Dict::new();

        // Insert a Unit into the Dict.
        {
            let mut entries = dict.lock_entries();
            entries.insert(CAP_KEY.to_string(), Box::new(Unit::default()));
            assert_eq!(entries.len(), 1);
        }

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let cap = dict_proxy
                .remove(CAP_KEY)
                .await
                .expect("failed to call Remove")
                .expect("failed to remove");

            // The value should be the same one that was previously inserted.
            assert_eq!(cap, Unit::default().into_fidl());

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        // Removing the entry with Remove should remove it from `entries`.
        assert!(dict.lock_entries().is_empty());

        Ok(())
    }

    /// Tests that `Dict.Get` yields the same capability that was previously inserted.
    #[fuchsia::test]
    async fn serve_get() -> Result<(), Error> {
        let mut dict = Dict::new();

        // Insert a Unit into the Dict.
        {
            let mut entries = dict.lock_entries();
            entries.insert(CAP_KEY.to_string(), Box::new(Unit::default()));
            assert_eq!(entries.len(), 1);
        }

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let cap =
                dict_proxy.get(CAP_KEY).await.expect("failed to call Get").expect("failed to get");

            // The value should be the same one that was previously inserted.
            assert_eq!(cap, Unit::default().into_fidl());

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        // The capability should remain in the Dict.
        assert_eq!(dict.lock_entries().len(), 1);

        Ok(())
    }

    /// Tests that `Dict.Insert` returns `ALREADY_EXISTS` when there is already an item with
    /// the same key.
    #[fuchsia::test]
    async fn insert_already_exists() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            // Insert an entry.
            dict_proxy
                .insert(CAP_KEY, Unit::default().into_fidl())
                .await
                .expect("failed to call Insert")
                .expect("failed to insert");

            // Inserting again should return an error.
            let result = dict_proxy
                .insert(CAP_KEY, Unit::default().into_fidl())
                .await
                .expect("failed to call Insert");
            assert_matches!(result, Err(fsandbox::DictionaryError::AlreadyExists));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that the `Dict.Remove` returns `NOT_FOUND` when there is no item with the given key.
    #[fuchsia::test]
    async fn remove_not_found() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            // Removing an item from an empty dict should fail.
            let result = dict_proxy.remove(CAP_KEY).await.expect("failed to call Remove");
            assert_matches!(result, Err(fsandbox::DictionaryError::NotFound));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    #[fuchsia::test]
    async fn serve_read() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let _server = fasync::Task::spawn(async move { dict.serve_dict(dict_stream).await });

        // Create two Data capabilities.
        let mut data_caps: Vec<_> = (1..3).map(|i| Data::Int64(i)).collect();

        // Add the Data capabilities to the dict.
        dict_proxy
            .insert("cap1", data_caps.remove(0).into_fidl())
            .await
            .expect("failed to call Insert")
            .expect("failed to insert");
        dict_proxy
            .insert("cap2", data_caps.remove(0).into_fidl())
            .await
            .expect("failed to call Insert")
            .expect("failed to insert");

        // Now read the entries back.
        let mut items = dict_proxy.read().await.unwrap();
        assert_eq!(items.len(), 2);
        assert_matches!(
            items.remove(0),
            fsandbox::DictionaryItem {
                key,
                value: fsandbox::Capability::Data(fsandbox::DataCapability::Int64(num))
            }
            if key == "cap1"
            && num == 1
        );
        assert_matches!(
            items.remove(0),
            fsandbox::DictionaryItem {
                key,
                value: fsandbox::Capability::Data(fsandbox::DataCapability::Int64(num))
            }
            if key == "cap2"
            && num == 2
        );

        Ok(())
    }

    /// Tests that `copy` produces a new Dict with cloned entries.
    #[fuchsia::test]
    async fn copy() -> Result<()> {
        // Create a Dict with a Unit inside, and copy the Dict.
        let dict = Dict::new();
        dict.lock_entries().insert("unit1".to_string(), Box::new(Unit::default()));

        let copy = dict.copy();

        // Insert a Unit into the copy.
        copy.lock_entries().insert("unit2".to_string(), Box::new(Unit::default()));

        // The copy should have two Units.
        let copy_entries = copy.lock_entries();
        assert_eq!(copy_entries.len(), 2);
        assert!(copy_entries.values().all(|value| (**value).as_any().is::<Unit>()));

        // The original Dict should have only one Unit.
        let entries = dict.lock_entries();
        assert_eq!(entries.len(), 1);
        assert!(entries.values().all(|value| (**value).as_any().is::<Unit>()));

        Ok(())
    }

    /// Tests that cloning a Dict results in a Dict that shares the same entries.
    #[fuchsia::test]
    async fn clone_by_reference() -> Result<()> {
        let dict = Dict::new();
        let dict_clone = dict.clone();

        // Add a Unit into the clone.
        {
            let mut clone_entries = dict_clone.lock_entries();
            clone_entries.insert(CAP_KEY.to_string(), Box::new(Unit::default()));
            assert_eq!(clone_entries.len(), 1);
        }

        // The original dict should now have an entry because it shares entries with the clone.
        let entries = dict.lock_entries();
        assert_eq!(entries.len(), 1);

        Ok(())
    }

    /// Tests that a Dict can be cloned via `fuchsia.unknown/Cloneable.Clone2`
    #[fuchsia::test]
    async fn fidl_clone() -> Result<()> {
        let dict = Dict::new();
        dict.lock_entries().insert(CAP_KEY.to_string(), Box::new(Unit::default()));

        let client_end: ClientEnd<fsandbox::DictionaryMarker> = dict.into();
        let dict_proxy = client_end.into_proxy().unwrap();

        // Clone the dict with `Clone2`
        let (clone_client_end, clone_server_end) = create_endpoints::<funknown::CloneableMarker>();
        let _ = dict_proxy.clone2(clone_server_end);
        let clone_client_end: ClientEnd<fsandbox::DictionaryMarker> =
            clone_client_end.into_channel().into();
        let clone_proxy = clone_client_end.into_proxy().unwrap();

        // Remove the `Unit` from the clone.
        let cap = clone_proxy
            .remove(CAP_KEY)
            .await
            .expect("failed to call Remove")
            .expect("failed to remove");

        // The value should be the Unit that was previously inserted.
        assert_eq!(cap, Unit::default().into_fidl());

        // Convert the original Dict back to a Rust object.
        let fidl_capability =
            fsandbox::Capability::Dictionary(ClientEnd::<fsandbox::DictionaryMarker>::new(
                dict_proxy.into_channel().unwrap().into_zx_channel(),
            ));
        let any: AnyCapability = fidl_capability.try_into().unwrap();
        let dict: Dict = any.try_into().unwrap();

        // The original dict should now have zero entries because the Unit was removed.
        let entries = dict.lock_entries();
        assert!(entries.is_empty());

        Ok(())
    }

    /// Tests that `Dict.Enumerate` creates a [DictionaryIterator] that returns entries.
    #[fuchsia::test]
    async fn enumerate() -> Result<()> {
        // Number of entries in the Dict that will be enumerated.
        //
        // This value was chosen such that that GetNext returns multiple chunks of different sizes.
        const NUM_ENTRIES: u32 = fsandbox::MAX_DICTIONARY_ITEMS_CHUNK * 2 + 1;

        // Number of items we expect in each chunk, for every chunk we expect to get.
        const EXPECTED_CHUNK_LENGTHS: &[u32] =
            &[fsandbox::MAX_DICTIONARY_ITEMS_CHUNK, fsandbox::MAX_DICTIONARY_ITEMS_CHUNK, 1];

        // Create a Dict with [NUM_ENTRIES] entries that have Unit values.
        let dict = Dict::new();
        {
            let mut entries = dict.lock_entries();
            for i in 0..NUM_ENTRIES {
                entries.insert(format!("{}", i), Box::new(Unit::default()));
            }
        }

        let client_end: ClientEnd<fsandbox::DictionaryMarker> = dict.into();
        let dict_proxy = client_end.into_proxy().unwrap();

        let (iter_proxy, iter_server_end) =
            create_proxy::<fsandbox::DictionaryIteratorMarker>().unwrap();
        dict_proxy.enumerate(iter_server_end).expect("failed to call Enumerate");

        // Get all the entries from the Dict with `GetNext`.
        let mut num_got_items: u32 = 0;
        for expected_len in EXPECTED_CHUNK_LENGTHS {
            let items = iter_proxy.get_next().await.expect("failed to call GetNext");
            if items.is_empty() {
                break;
            }
            assert_eq!(*expected_len, items.len() as u32);
            num_got_items += items.len() as u32;
            for item in items {
                assert_eq!(item.value, Unit::default().into_fidl());
            }
        }

        // GetNext should return no items once all items have been returned.
        let items = iter_proxy.get_next().await.expect("failed to call GetNext");
        assert!(items.is_empty());

        assert_eq!(num_got_items, NUM_ENTRIES);

        Ok(())
    }

    #[fuchsia::test]
    async fn try_into_open_error_not_supported() {
        let dict = Dict::new();
        dict.lock_entries().insert(CAP_KEY.to_string(), Box::new(Unit::default()));
        assert_matches!(dict.try_into_open(), Err(ConversionError::Other { .. }));
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
        );
        let dict = Dict::new();
        // This string is too long to be a valid fuchsia.io name.
        let bad_name = "a".repeat(10000);
        dict.lock_entries().insert(bad_name, Box::new(placeholder_open));
        assert_matches!(dict.try_into_open(), Err(ConversionError::Other { .. }));
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
        );
        let dict = Dict::new();
        dict.lock_entries().insert(CAP_KEY.to_string(), Box::new(open));
        let dict_open = dict.try_into_open().expect("convert dict into Open capability");

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
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();
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
        );
        let inner_dict = Dict::new();
        inner_dict.lock_entries().insert(CAP_KEY.to_string(), Box::new(open));
        let dict = Dict::new();
        dict.lock_entries().insert(CAP_KEY.to_string(), Box::new(inner_dict));

        let dict_open = dict.try_into_open().expect("convert dict into Open capability");

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
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();
        assert_eq!(OPEN_COUNT.get(), 1)
    }
}
