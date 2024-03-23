// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use derivative::Derivative;
use fidl::endpoints::{create_request_stream, ClientEnd, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::TryStreamExt;
use std::{
    collections::{btree_map::Entry, BTreeMap},
    sync::{Arc, Mutex, MutexGuard},
};
use tracing::warn;
use vfs::{
    directory::{
        entry::DirectoryEntry,
        helper::{AlreadyExists, DirectlyMutable},
        immutable::simple as pfs,
    },
    name::Name,
};

use crate::{registry, Capability, CapabilityTrait, ConversionError, Open};

pub type Key = String;

/// A capability that represents a dictionary of capabilities.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Dict {
    entries: Arc<Mutex<BTreeMap<Key, Capability>>>,

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

    pub fn lock_entries(&self) -> MutexGuard<'_, BTreeMap<Key, Capability>> {
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
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fsandbox::DictionaryRequest::Insert { key, value, responder, .. } => {
                    let result = match self.lock_entries().entry(key) {
                        Entry::Occupied(_) => Err(fsandbox::DictionaryError::AlreadyExists),
                        Entry::Vacant(entry) => match Capability::try_from(value) {
                            Ok(cap) => {
                                entry.insert(cap);
                                Ok(())
                            }
                            Err(_) => Err(fsandbox::DictionaryError::BadCapability),
                        },
                    };
                    responder.send(result)?;
                }
                fsandbox::DictionaryRequest::Get { key, responder } => {
                    let result = match self.entries.lock().unwrap().get(&key) {
                        Some(cap) => Ok(cap.clone().into_fidl()),
                        None => {
                            (self.not_found)(key);
                            Err(fsandbox::DictionaryError::NotFound)
                        }
                    };
                    responder.send(result)?;
                }
                fsandbox::DictionaryRequest::Remove { key, responder } => {
                    let result = match self.entries.lock().unwrap().remove(&key) {
                        Some(cap) => Ok(cap.into_fidl()),
                        None => {
                            (self.not_found)(key);
                            Err(fsandbox::DictionaryError::NotFound)
                        }
                    };
                    responder.send(result)?;
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
                    responder.send(items)?;
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

        // Move this capability into the registry.
        registry::spawn_task(self.into(), koid, async move {
            dict.serve_dict(stream).await.expect("failed to serve Dict");
        });
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

impl CapabilityTrait for Dict {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        let dir = pfs::simple();
        for (key, value) in self.lock_entries().iter() {
            let remote: Arc<dyn DirectoryEntry> = match value {
                Capability::Directory(d) => d.clone().into_remote(),
                value => {
                    let open: Open = value.clone().try_into_open().map_err(|err| {
                        ConversionError::Nested { key: key.clone(), err: Box::new(err) }
                    })?;
                    open.into_remote()
                }
            };
            let key: Name = key.clone().try_into()?;
            match dir.add_entry_impl(key, remote, false) {
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
        Ok(dir)
    }
}

/// Serves the `fuchsia.sandbox.DictionaryIterator` protocol, providing items from the given iterator.
async fn serve_dict_iterator(
    items: Vec<(Key, Capability)>,
    mut stream: fsandbox::DictionaryIteratorRequestStream,
) {
    let mut chunks = items
        .chunks(fsandbox::MAX_DICTIONARY_ITEMS_CHUNK as usize)
        .map(|chunk: &[(Key, Capability)]| chunk.to_vec())
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
    use crate::{Data, Directory, Unit};
    use anyhow::{Error, Result};
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, create_proxy, create_proxy_and_stream, Proxy};
    use fidl_fuchsia_io as fio;
    use fidl_fuchsia_unknown as funknown;
    use fuchsia_fs::directory::DirEntry;
    use futures::try_join;
    use test_util::Counter;
    use vfs::{
        directory::{
            entry::{serve_directory, EntryInfo, OpenRequest, SubNode},
            entry_container::Directory as VfsDirectory,
        },
        execution_scope::ExecutionScope,
        path::Path,
        pseudo_directory,
        remote::{remote_dir, RemoteLike},
        service::endpoint,
    };

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

        try_join!(client, server).unwrap();

        let mut entries = dict.lock_entries();

        // Inserting adds the entry to `entries`.
        assert_eq!(entries.len(), 1);

        // The entry that was inserted should now be in `entries`.
        let cap = entries.remove(CAP_KEY).expect("not in entries after insert");
        let Capability::Unit(unit) = cap else { panic!("Bad capability type: {:#?}", cap) };
        assert_eq!(unit, Unit::default());

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
            entries.insert(CAP_KEY.to_string(), Capability::Unit(Unit::default()));
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

        try_join!(client, server).unwrap();

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
            entries.insert(CAP_KEY.to_string(), Capability::Unit(Unit::default()));
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

        try_join!(client, server).unwrap();

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

        try_join!(client, server).unwrap();
        Ok(())
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

        try_join!(client, server).unwrap();
        Ok(())
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
        dict.lock_entries().insert("unit1".to_string(), Capability::Unit(Unit::default()));

        let copy = dict.copy();

        // Insert a Unit into the copy.
        copy.lock_entries().insert("unit2".to_string(), Capability::Unit(Unit::default()));

        // The copy should have two Units.
        let copy_entries = copy.lock_entries();
        assert_eq!(copy_entries.len(), 2);
        assert!(copy_entries.values().all(|value| matches!(value, Capability::Unit(_))));

        // The original Dict should have only one Unit.
        let entries = dict.lock_entries();
        assert_eq!(entries.len(), 1);
        assert!(entries.values().all(|value| matches!(value, Capability::Unit(_))));

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
            clone_entries.insert(CAP_KEY.to_string(), Capability::Unit(Unit::default()));
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
        dict.lock_entries().insert(CAP_KEY.to_string(), Capability::Unit(Unit::default()));

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
        let any: Capability = fidl_capability.try_into().unwrap();
        let dict = assert_matches!(any, Capability::Dictionary(c) => c);

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
                entries.insert(format!("{}", i), Capability::Unit(Unit::default()));
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
        dict.lock_entries().insert(CAP_KEY.to_string(), Capability::Unit(Unit::default()));
        assert_matches!(dict.try_into_open(), Err(ConversionError::Nested { .. }));
    }

    #[fuchsia::test]
    async fn try_into_open_error_invalid_name() {
        let (proxy, _server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let placeholder_open = Open::new(remote_dir(proxy));
        let dict = Dict::new();
        // This string is too long to be a valid fuchsia.io name.
        let bad_name = "a".repeat(10000);
        dict.lock_entries().insert(bad_name, Capability::Open(placeholder_open));
        assert_matches!(dict.try_into_open(), Err(ConversionError::ParseName(_)));
    }

    struct MockDir(Counter);
    impl DirectoryEntry for MockDir {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }

        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
            request.open_remote(self)
        }
    }
    impl RemoteLike for MockDir {
        fn open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _flags: fio::OpenFlags,
            relative_path: Path,
            _server_end: ServerEnd<fio::NodeMarker>,
        ) {
            assert_eq!(relative_path.as_ref(), "bar");
            self.0.inc();
        }
    }

    /// Convert a dict `{ CAP_KEY: open }` to [Open].
    #[fuchsia::test]
    async fn try_into_open_success() {
        let dict = Dict::new();
        let mock_dir = Arc::new(MockDir(Counter::new(0)));
        dict.lock_entries()
            .insert(CAP_KEY.to_string(), Capability::Open(Open::new(mock_dir.clone())));
        let dict_open = dict.try_into_open().expect("convert dict into Open capability");

        let remote = dict_open.into_remote();
        let scope = ExecutionScope::new();

        let dir_client_end =
            serve_directory(remote.clone(), &scope, fio::OpenFlags::DIRECTORY).unwrap();

        assert_eq!(mock_dir.0.get(), 0);
        let (client_end, server_end) = zx::Channel::create();
        let dir = dir_client_end.channel();
        fdio::service_connect_at(dir, &format!("{CAP_KEY}/bar"), server_end).unwrap();
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();
        assert_eq!(mock_dir.0.get(), 1);
    }

    /// Convert a dict `{ CAP_KEY: { CAP_KEY: open } }` to [Open].
    #[fuchsia::test]
    async fn try_into_open_success_nested() {
        let inner_dict = Dict::new();
        let mock_dir = Arc::new(MockDir(Counter::new(0)));
        inner_dict
            .lock_entries()
            .insert(CAP_KEY.to_string(), Capability::Open(Open::new(mock_dir.clone())));
        let dict = Dict::new();
        dict.lock_entries().insert(CAP_KEY.to_string(), Capability::Dictionary(inner_dict));

        let dict_open = dict.try_into_open().expect("convert dict into Open capability");

        let remote = dict_open.into_remote();
        let scope = ExecutionScope::new();

        let dir_client_end =
            serve_directory(remote.clone(), &scope, fio::OpenFlags::DIRECTORY).unwrap();

        // List the outer directory and verify the contents.
        let dir = dir_client_end.into_proxy().unwrap();
        assert_eq!(
            fuchsia_fs::directory::readdir(&dir).await.unwrap(),
            vec![DirEntry { name: CAP_KEY.to_string(), kind: fio::DirentType::Directory },]
        );

        // Open the inner most capability.
        assert_eq!(mock_dir.0.get(), 0);
        let (client_end, server_end) = zx::Channel::create();
        let dir = dir.into_channel().unwrap().into_zx_channel();
        fdio::service_connect_at(&dir, &format!("{CAP_KEY}/{CAP_KEY}/bar"), server_end).unwrap();
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();
        assert_eq!(mock_dir.0.get(), 1)
    }

    fn serve_vfs_dir(root: Arc<impl VfsDirectory>) -> ClientEnd<fio::DirectoryMarker> {
        let scope = ExecutionScope::new();
        let (client, server) = create_endpoints::<fio::DirectoryMarker>();
        root.open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            vfs::path::Path::dot(),
            ServerEnd::new(server.into_channel()),
        );
        client
    }

    #[fuchsia::test]
    async fn try_into_open_with_directory() {
        let open = Open::new(endpoint(|_scope, _channel| {}));
        let fs = pseudo_directory! {
            "a" => open.clone().into_remote(),
            "b" => open.clone().into_remote(),
            "c" => open.into_remote(),
        };
        let directory = Directory::from(serve_vfs_dir(fs));
        let dict = Dict::new();
        dict.lock_entries().insert(CAP_KEY.to_string(), Capability::Directory(directory));

        let dict_open = dict.try_into_open().unwrap();
        let remote = dict_open.into_remote();

        // List the inner directory and verify its contents.
        let scope = ExecutionScope::new();
        {
            let dir_proxy = serve_directory(remote.clone(), &scope, fio::OpenFlags::DIRECTORY)
                .unwrap()
                .into_proxy()
                .unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap(),
                vec![DirEntry { name: CAP_KEY.to_string(), kind: fio::DirentType::Directory },]
            );
        }
        {
            let dir_proxy = serve_directory(
                Arc::new(SubNode::new(
                    remote,
                    CAP_KEY.try_into().unwrap(),
                    fio::DirentType::Directory,
                )),
                &scope,
                fio::OpenFlags::DIRECTORY,
            )
            .unwrap()
            .into_proxy()
            .unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap(),
                vec![
                    DirEntry { name: "a".to_string(), kind: fio::DirentType::Service },
                    DirEntry { name: "b".to_string(), kind: fio::DirentType::Service },
                    DirEntry { name: "c".to_string(), kind: fio::DirentType::Service },
                ]
            );
        }
    }
}
