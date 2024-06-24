// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::dict::Key;
use crate::fidl::registry;
use crate::{Capability, ConversionError, Dict, RemotableCapability};
use fidl::endpoints::{self, create_request_stream, ClientEnd};
use fidl::handle::AsHandleRef;
use fuchsia_zircon::Koid;
use futures::TryStreamExt;
use std::sync::Arc;
use tracing::warn;
use vfs::directory::entry::DirectoryEntry;
use vfs::directory::helper::{AlreadyExists, DirectlyMutable};
use vfs::directory::immutable::simple as pfs;
use vfs::name::Name;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

impl Dict {
    /// Serve the `fuchsia.component.sandbox.Dictionary` protocol for this `Dict`.
    pub async fn serve_dict(
        &mut self,
        mut stream: fsandbox::DictionaryRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fsandbox::DictionaryRequest::Insert { key, value, responder, .. } => {
                    let result = (|| {
                        let key = key.parse().map_err(|_| fsandbox::DictionaryError::InvalidKey)?;
                        let cap = Capability::try_from(value)
                            .map_err(|_| fsandbox::DictionaryError::BadCapability)?;
                        self.insert(key, cap)
                    })();
                    responder.send(result)?;
                }
                fsandbox::DictionaryRequest::Get { key, responder } => {
                    let result = (|| {
                        let key =
                            Key::new(key).map_err(|_| fsandbox::DictionaryError::InvalidKey)?;
                        match self.get(&key) {
                            Some(cap) => Ok(cap.into()),
                            None => {
                                (self.not_found)(key.as_str());
                                Err(fsandbox::DictionaryError::NotFound)
                            }
                        }
                    })();
                    responder.send(result)?;
                }
                fsandbox::DictionaryRequest::Remove { key, responder } => {
                    let result = (|| {
                        let key =
                            Key::new(key).map_err(|_| fsandbox::DictionaryError::InvalidKey)?;
                        match self.remove(&key) {
                            Some(cap) => Ok(cap.into()),
                            None => {
                                (self.not_found)(key.as_str());
                                Err(fsandbox::DictionaryError::NotFound)
                            }
                        }
                    })();
                    responder.send(result)?;
                }
                fsandbox::DictionaryRequest::Clone { responder } => {
                    let (client_end, server_end) =
                        endpoints::create_endpoints::<fsandbox::DictionaryMarker>();
                    // The clone is registered under the koid of the client end.
                    let koid = client_end.basic_info().unwrap().koid;
                    let stream = server_end.into_stream().unwrap();
                    self.clone().serve_and_register(stream, koid);
                    responder.send(client_end)?;
                }
                fsandbox::DictionaryRequest::Copy { responder } => {
                    let (client_end, server_end) =
                        endpoints::create_endpoints::<fsandbox::DictionaryMarker>();
                    // The copy is registered under the koid of the client end.
                    let koid = client_end.basic_info().unwrap().koid;
                    let stream = server_end.into_stream().unwrap();
                    self.shallow_copy().serve_and_register(stream, koid);
                    responder.send(client_end)?;
                }
                fsandbox::DictionaryRequest::Enumerate { iterator: server_end, .. } => {
                    let items = self.enumerate().collect();
                    let stream = server_end.into_stream().unwrap();
                    let task = fasync::Task::spawn(serve_dict_item_iterator(items, stream));
                    self.iterator_tasks.add(task);
                }
                fsandbox::DictionaryRequest::Keys { iterator: server_end, .. } => {
                    let keys = self.keys().collect();
                    let stream = server_end.into_stream().unwrap();
                    let task = fasync::Task::spawn(serve_dict_key_iterator(keys, stream));
                    self.iterator_tasks.add(task);
                }
                fsandbox::DictionaryRequest::Drain { iterator: server_end, .. } => {
                    // Take out entries, replacing with an empty BTreeMap.
                    // They are dropped if the caller does not request an iterator.
                    if let Some(server_end) = server_end {
                        let items = self.drain().collect();
                        let stream = server_end.into_stream().unwrap();
                        let task = fasync::Task::spawn(serve_dict_item_iterator(items, stream));
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
    pub fn serve_and_register(self, stream: fsandbox::DictionaryRequestStream, koid: Koid) {
        let mut dict = self.clone();

        // Move this capability into the registry.
        registry::insert(self.into(), koid, async move {
            dict.serve_dict(stream).await.expect("failed to serve Dict");
        });
    }
}

impl From<Dict> for ClientEnd<fsandbox::DictionaryMarker> {
    fn from(dict: Dict) -> Self {
        let (client_end, dict_stream) =
            create_request_stream::<fsandbox::DictionaryMarker>().unwrap();
        dict.serve_and_register(dict_stream, client_end.get_koid().unwrap());
        client_end
    }
}

impl From<Dict> for fsandbox::Capability {
    fn from(dict: Dict) -> Self {
        Self::Dictionary(dict.into())
    }
}

impl RemotableCapability for Dict {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        let dir = pfs::simple();
        for (key, value) in self.enumerate() {
            let remote: Arc<dyn DirectoryEntry> = match value {
                Capability::Directory(d) => d.into_remote(),
                value => value.try_into_directory_entry().map_err(|err| {
                    ConversionError::Nested { key: key.to_string(), err: Box::new(err) }
                })?,
            };
            let key: Name = key.to_string().try_into()?;
            match dir.add_entry_impl(key, remote, false) {
                Ok(()) => {}
                Err(AlreadyExists) => {
                    unreachable!("Dict items should be unique");
                }
            }
        }
        let not_found = self.not_found.clone();
        dir.clone().set_not_found_handler(Box::new(move |path| {
            not_found(path);
        }));
        Ok(dir)
    }
}

async fn serve_dict_item_iterator(
    items: Vec<(Key, Capability)>,
    mut stream: fsandbox::DictionaryItemIteratorRequestStream,
) {
    let mut chunks = items
        .chunks(fsandbox::MAX_DICTIONARY_ITEMS_CHUNK as usize)
        .map(|chunk: &[(Key, Capability)]| chunk.to_vec())
        .collect::<Vec<_>>()
        .into_iter();

    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::DictionaryItemIteratorRequest::GetNext { responder } => match chunks.next() {
                Some(chunk) => {
                    let items = chunk
                        .into_iter()
                        .map(|(key, value)| fsandbox::DictionaryItem {
                            key: key.to_string(),
                            value: value.into(),
                        })
                        .collect();
                    if let Err(_) = responder.send(items) {
                        return;
                    }
                }
                None => {
                    let _ = responder.send(vec![]);
                    return;
                }
            },
            fsandbox::DictionaryItemIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryItemIterator request");
            }
        }
    }
}

async fn serve_dict_key_iterator(
    keys: Vec<Key>,
    mut stream: fsandbox::DictionaryKeyIteratorRequestStream,
) {
    let mut chunks = keys
        .chunks(fsandbox::MAX_DICTIONARY_KEYS_CHUNK as usize)
        .map(|chunk: &[Key]| chunk.to_vec())
        .collect::<Vec<_>>()
        .into_iter();

    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::DictionaryKeyIteratorRequest::GetNext { responder } => match chunks.next() {
                Some(chunk) => {
                    let keys: Vec<_> = chunk.into_iter().map(|k| k.to_string()).collect();
                    if let Err(_) = responder.send(&keys) {
                        return;
                    }
                }
                None => {
                    let _ = responder.send(&[]);
                    return;
                }
            },
            fsandbox::DictionaryKeyIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryKeyIterator request");
            }
        }
    }
}

// These tests only run on target because the vfs library is not generally available on host.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Data, Dict, DirEntry, Directory, Unit};
    use anyhow::{Error, Result};
    use assert_matches::assert_matches;
    use fidl::endpoints::{
        create_endpoints, create_proxy, create_proxy_and_stream, Proxy, ServerEnd,
    };
    use fidl::handle::{Channel, Status};
    use fidl_fuchsia_io as fio;
    use fuchsia_fs::directory;
    use futures::try_join;
    use lazy_static::lazy_static;
    use test_util::Counter;
    use vfs::directory::entry::{serve_directory, DirectoryEntry, EntryInfo, OpenRequest, SubNode};
    use vfs::directory::entry_container::Directory as VfsDirectory;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::pseudo_directory;
    use vfs::remote::RemoteLike;
    use vfs::service::endpoint;

    lazy_static! {
        static ref CAP_KEY: Key = "cap".parse().unwrap();
    }

    /// Tests that the `Dict` contains an entry for a capability inserted via `Dict.Insert`,
    /// and that the value is the same capability.
    #[fuchsia::test]
    async fn serve_insert() -> Result<()> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let value = Unit::default().into();
            dict_proxy
                .insert(CAP_KEY.as_str(), value)
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
        let cap = entries.remove(&*CAP_KEY).expect("not in entries after insert");
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
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict.lock_entries().len(), 1);

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let cap = dict_proxy
                .remove(CAP_KEY.as_str())
                .await
                .expect("failed to call Remove")
                .expect("failed to remove");

            // The value should be the same one that was previously inserted.
            assert_eq!(cap, Unit::default().into());

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
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict.lock_entries().len(), 1);

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let cap = dict_proxy
                .get(CAP_KEY.as_str())
                .await
                .expect("failed to call Get")
                .expect("failed to get");

            // The value should be the same one that was previously inserted.
            assert_eq!(cap, Unit::default().into());

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
                .insert(CAP_KEY.as_str(), Unit::default().into())
                .await
                .expect("failed to call Insert")
                .expect("failed to insert");

            // Inserting again should return an error.
            let result = dict_proxy
                .insert(CAP_KEY.as_str(), Unit::default().into())
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
            let result = dict_proxy.remove(CAP_KEY.as_str()).await.expect("failed to call Remove");
            assert_matches!(result, Err(fsandbox::DictionaryError::NotFound));

            Ok(())
        };

        try_join!(client, server).unwrap();
        Ok(())
    }

    /// Tests that `copy` produces a new Dict with cloned entries.
    #[fuchsia::test]
    async fn copy() -> Result<()> {
        // Create a Dict with a Unit inside, and copy the Dict.
        let dict = Dict::new();
        dict.insert("unit1".parse().unwrap(), Capability::Unit(Unit::default())).unwrap();

        let copy = dict.shallow_copy();

        // Insert a Unit into the copy.
        copy.insert("unit2".parse().unwrap(), Capability::Unit(Unit::default())).unwrap();

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
        dict_clone.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict_clone.lock_entries().len(), 1);

        // The original dict should now have an entry because it shares entries with the clone.
        let entries = dict.lock_entries();
        assert_eq!(entries.len(), 1);

        Ok(())
    }

    /// Tests that a Dict can be cloned via `fuchsia.unknown/Cloneable.Clone2`
    #[fuchsia::test]
    async fn fidl_clone() -> Result<()> {
        let dict = Dict::new();
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();

        let client_end: ClientEnd<fsandbox::DictionaryMarker> = dict.into();
        let dict_proxy = client_end.into_proxy().unwrap();

        // Clone the dict with `Clone`
        let clone_client_end = dict_proxy.clone().await.unwrap();
        let clone_client_end: ClientEnd<fsandbox::DictionaryMarker> =
            clone_client_end.into_channel().into();
        let clone_proxy = clone_client_end.into_proxy().unwrap();

        // Remove the `Unit` from the clone.
        let cap = clone_proxy
            .remove(CAP_KEY.as_str())
            .await
            .expect("failed to call Remove")
            .expect("failed to remove");

        // The value should be the Unit that was previously inserted.
        assert_eq!(cap, Unit::default().into());

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

    /// Tests basic functionality of Enumerate and Keys APIs.
    #[fuchsia::test]
    async fn read() {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) =
            create_proxy_and_stream::<fsandbox::DictionaryMarker>().unwrap();
        let _server = fasync::Task::spawn(async move { dict.serve_dict(dict_stream).await });

        // Create two Data capabilities.
        let mut data_caps: Vec<_> = (1..3).map(|i| Data::Int64(i)).collect();

        // Add the Data capabilities to the dict.
        dict_proxy.insert("cap1", data_caps.remove(0).into()).await.unwrap().unwrap();
        dict_proxy.insert("cap2", data_caps.remove(0).into()).await.unwrap().unwrap();

        // Now read the entries back.
        let (iterator, server_end) = endpoints::create_proxy().unwrap();
        dict_proxy.enumerate(server_end).unwrap();
        let mut items = iterator.get_next().await.unwrap();
        assert!(iterator.get_next().await.unwrap().is_empty());
        assert_eq!(items.len(), 2);
        assert_matches!(
            items.remove(0),
            fsandbox::DictionaryItem {
                key,
                value: fsandbox::Capability::Data(fsandbox::Data::Int64(num))
            }
            if key == "cap1"
            && num == 1
        );
        assert_matches!(
            items.remove(0),
            fsandbox::DictionaryItem {
                key,
                value: fsandbox::Capability::Data(fsandbox::Data::Int64(num))
            }
            if key == "cap2"
            && num == 2
        );

        let (iterator, server_end) = endpoints::create_proxy().unwrap();
        dict_proxy.keys(server_end).unwrap();
        let keys = iterator.get_next().await.unwrap();
        assert!(iterator.get_next().await.unwrap().is_empty());
        assert_eq!(keys, ["cap1", "cap2"]);
    }

    /// Tests batching for Enumerate and Keys iterators.
    #[fuchsia::test]
    async fn read_batches() {
        // Number of entries in the Dict that will be enumerated.
        //
        // This value was chosen such that that GetNext returns multiple chunks of different sizes.
        const NUM_ENTRIES: u32 = fsandbox::MAX_DICTIONARY_ITEMS_CHUNK * 2 + 1;

        // Number of items we expect in each chunk, for every chunk we expect to get.
        const EXPECTED_CHUNK_LENGTHS: &[u32] =
            &[fsandbox::MAX_DICTIONARY_ITEMS_CHUNK, fsandbox::MAX_DICTIONARY_ITEMS_CHUNK, 1];

        // Create a Dict with [NUM_ENTRIES] entries that have Unit values.
        let dict = Dict::new();
        for i in 0..NUM_ENTRIES {
            dict.insert(format!("{}", i).parse().unwrap(), Capability::Unit(Unit::default()))
                .unwrap();
        }

        let client_end: ClientEnd<fsandbox::DictionaryMarker> = dict.into();
        let dict_proxy = client_end.into_proxy().unwrap();

        let (item_iterator, server_end) = create_proxy().unwrap();
        dict_proxy.enumerate(server_end).unwrap();
        let (key_iterator, server_end) = create_proxy().unwrap();
        dict_proxy.keys(server_end).unwrap();

        // Get all the entries from the Dict with `GetNext`.
        let mut num_got_items: u32 = 0;
        for expected_len in EXPECTED_CHUNK_LENGTHS {
            let items = item_iterator.get_next().await.unwrap();
            let keys = key_iterator.get_next().await.unwrap();
            if items.is_empty() && keys.is_empty() {
                break;
            }
            assert_eq!(*expected_len, items.len() as u32);
            assert_eq!(*expected_len, keys.len() as u32);
            num_got_items += items.len() as u32;
            for item in items {
                assert_eq!(item.value, Unit::default().into());
            }
        }

        // GetNext should return no items once all items have been returned.
        assert!(item_iterator.get_next().await.unwrap().is_empty());
        assert!(key_iterator.get_next().await.unwrap().is_empty());

        assert_eq!(num_got_items, NUM_ENTRIES);
    }

    #[fuchsia::test]
    async fn try_into_open_error_not_supported() {
        let dict = Dict::new();
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default()))
            .expect("dict entry already exists");
        assert_matches!(
            dict.try_into_directory_entry().err(),
            Some(ConversionError::Nested { .. })
        );
    }

    struct MockDir(Counter);
    impl DirectoryEntry for MockDir {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }

        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
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

    #[fuchsia::test]
    async fn try_into_open_success() {
        let dict = Dict::new();
        let mock_dir = Arc::new(MockDir(Counter::new(0)));
        dict.insert(CAP_KEY.clone(), Capability::DirEntry(DirEntry::new(mock_dir.clone())))
            .expect("dict entry already exists");
        let remote = dict.try_into_directory_entry().unwrap();
        let scope = ExecutionScope::new();

        let dir_client_end =
            serve_directory(remote.clone(), &scope, fio::OpenFlags::DIRECTORY).unwrap();

        assert_eq!(mock_dir.0.get(), 0);
        let (client_end, server_end) = Channel::create();
        let dir = dir_client_end.channel();
        fdio::service_connect_at(dir, &format!("{}/bar", *CAP_KEY), server_end).unwrap();
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();
        assert_eq!(mock_dir.0.get(), 1);
    }

    #[fuchsia::test]
    async fn try_into_open_success_nested() {
        let inner_dict = Dict::new();
        let mock_dir = Arc::new(MockDir(Counter::new(0)));
        inner_dict
            .insert(CAP_KEY.clone(), Capability::DirEntry(DirEntry::new(mock_dir.clone())))
            .expect("dict entry already exists");
        let dict = Dict::new();
        dict.insert(CAP_KEY.clone(), Capability::Dictionary(inner_dict)).unwrap();

        let remote = dict.try_into_directory_entry().expect("convert dict into Open capability");
        let scope = ExecutionScope::new();

        let dir_client_end =
            serve_directory(remote.clone(), &scope, fio::OpenFlags::DIRECTORY).unwrap();

        // List the outer directory and verify the contents.
        let dir = dir_client_end.into_proxy().unwrap();
        assert_eq!(
            fuchsia_fs::directory::readdir(&dir).await.unwrap(),
            vec![directory::DirEntry {
                name: CAP_KEY.to_string(),
                kind: fio::DirentType::Directory
            },]
        );

        // Open the inner most capability.
        assert_eq!(mock_dir.0.get(), 0);
        let (client_end, server_end) = Channel::create();
        let dir = dir.into_channel().unwrap().into_zx_channel();
        fdio::service_connect_at(&dir, &format!("{}/{}/bar", *CAP_KEY, *CAP_KEY), server_end)
            .unwrap();
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
        let dir_entry = DirEntry::new(endpoint(|_scope, _channel| {}));
        let fs = pseudo_directory! {
            "a" => dir_entry.clone().try_into_directory_entry().unwrap(),
            "b" => dir_entry.clone().try_into_directory_entry().unwrap(),
            "c" => dir_entry.try_into_directory_entry().unwrap(),
        };
        let directory = Directory::from(serve_vfs_dir(fs));
        let dict = Dict::new();
        dict.insert(CAP_KEY.clone(), Capability::Directory(directory))
            .expect("dict entry already exists");

        let remote = dict.try_into_directory_entry().unwrap();

        // List the inner directory and verify its contents.
        let scope = ExecutionScope::new();
        {
            let dir_proxy = serve_directory(remote.clone(), &scope, fio::OpenFlags::DIRECTORY)
                .unwrap()
                .into_proxy()
                .unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&dir_proxy).await.unwrap(),
                vec![directory::DirEntry {
                    name: CAP_KEY.to_string(),
                    kind: fio::DirentType::Directory
                },]
            );
        }
        {
            let dir_proxy = serve_directory(
                Arc::new(SubNode::new(
                    remote,
                    CAP_KEY.to_string().try_into().unwrap(),
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
                    directory::DirEntry { name: "a".to_string(), kind: fio::DirentType::Service },
                    directory::DirEntry { name: "b".to_string(), kind: fio::DirentType::Service },
                    directory::DirEntry { name: "c".to_string(), kind: fio::DirentType::Service },
                ]
            );
        }
    }
}
