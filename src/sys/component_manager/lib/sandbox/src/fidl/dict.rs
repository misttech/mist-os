// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::dict::Key;
use crate::fidl::registry::{self, try_from_handle_in_registry};
use crate::{Capability, ConversionError, Dict, RemotableCapability, RemoteError};
use fidl::AsHandleRef;
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::TryStreamExt;
use std::sync::Arc;
use tracing::warn;
use vfs::directory::entry::DirectoryEntry;
use vfs::directory::helper::{AlreadyExists, DirectlyMutable};
use vfs::directory::immutable::simple as pfs;
use vfs::name::Name;

impl Dict {
    /// Serve the `fuchsia.component.sandbox/Dictionary` protocol for this [Dict].
    pub fn serve(&self, stream: fsandbox::DictionaryRequestStream) {
        let mut clone = self.clone();
        let mut this = self.lock();
        this.tasks().spawn(async move {
            let _ = clone.do_serve(stream).await;
        });
    }

    async fn do_serve(
        &mut self,
        mut stream: fsandbox::DictionaryRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fsandbox::DictionaryRequest::Enumerate { iterator: server_end, .. } => {
                    let items = self.enumerate();
                    let stream = server_end.into_stream().unwrap();
                    let mut this = self.lock();
                    this.tasks().spawn(serve_enumerate_iterator(items, stream));
                }
                fsandbox::DictionaryRequest::_UnknownMethod { ordinal, .. } => {
                    warn!("Received unknown Dict request with ordinal {ordinal}");
                }
            }
        }
        Ok(())
    }
}

impl From<Dict> for fsandbox::DictionaryRef {
    fn from(dict: Dict) -> Self {
        fsandbox::DictionaryRef { token: registry::insert_token(dict.into()) }
    }
}

impl From<Dict> for fsandbox::Capability {
    fn from(dict: Dict) -> Self {
        Self::Dictionary(dict.into())
    }
}

impl TryFrom<fsandbox::DictionaryRef> for Dict {
    type Error = RemoteError;

    fn try_from(dict: fsandbox::DictionaryRef) -> Result<Self, Self::Error> {
        let any = try_from_handle_in_registry(dict.token.as_handle_ref())?;
        let Capability::Dictionary(dict) = any else {
            panic!("BUG: registry has a non-Dict capability under a Dict koid");
        };
        Ok(dict)
    }
}

impl RemotableCapability for Dict {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        let dir = pfs::simple();
        for (key, value) in self.enumerate() {
            let Ok(value) = value else {
                continue;
            };
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
        let not_found = self.lock().not_found.clone();
        dir.clone().set_not_found_handler(Box::new(move |path| {
            not_found(path);
        }));
        Ok(dir)
    }
}

async fn serve_enumerate_iterator(
    mut items: impl Iterator<Item = (Key, Result<Capability, ()>)>,
    mut stream: fsandbox::DictionaryEnumerateIteratorRequestStream,
) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::DictionaryEnumerateIteratorRequest::GetNext { responder } => {
                let mut chunk = vec![];
                for _ in 0..fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK {
                    match items.next() {
                        Some((key, value)) => {
                            chunk.push(fsandbox::DictionaryFallibleItem {
                                key: key.into(),
                                value: match value {
                                    Ok(v) => fsandbox::DictionaryValueResult::Ok(v.into()),
                                    Err(()) => fsandbox::DictionaryValueResult::Error(
                                        fsandbox::DictionaryError::NotCloneable,
                                    ),
                                },
                            });
                        }
                        None => break,
                    }
                }
                let _ = responder.send(chunk);
            }
            fsandbox::DictionaryEnumerateIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryEnumerateIterator request");
            }
        }
    }
}

pub(super) async fn serve_keys_iterator(
    mut keys: impl Iterator<Item = Key>,
    mut stream: fsandbox::DictionaryKeysIteratorRequestStream,
) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::DictionaryKeysIteratorRequest::GetNext { responder } => {
                let mut chunk = vec![];
                for _ in 0..fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK {
                    match keys.next() {
                        Some(key) => {
                            chunk.push(key.into());
                        }
                        None => break,
                    }
                }
                let _ = responder.send(&chunk);
            }
            fsandbox::DictionaryKeysIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryKeysIterator request");
            }
        }
    }
}
// These tests only run on target because the vfs library is not generally available on host.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::Key;
    use crate::{serve_capability_store, Data, Dict, DirEntry, Directory, Handle, Unit};
    use anyhow::{Error, Result};
    use assert_matches::assert_matches;
    use fidl::endpoints::{
        create_endpoints, create_proxy, create_proxy_and_stream, ClientEnd, Proxy, ServerEnd,
    };
    use fidl::handle::{Channel, HandleBased, Status};
    use fuchsia_fs::directory;
    use lazy_static::lazy_static;
    use test_util::Counter;
    use vfs::directory::entry::{
        serve_directory, DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest, SubNode,
    };
    use vfs::directory::entry_container::Directory as VfsDirectory;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::pseudo_directory;
    use vfs::remote::RemoteLike;
    use vfs::service::endpoint;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    lazy_static! {
        static ref CAP_KEY: Key = "cap".parse().unwrap();
    }

    #[fuchsia::test]
    async fn create_and_open_dictionary() {
        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        let dict_id = 42;
        assert_matches!(store.dictionary_create(dict_id).await.unwrap(), Ok(()));
        assert_matches!(
            store.dictionary_create(dict_id).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::IdAlreadyExists)
        );
        let (dict, server) = create_proxy().unwrap();
        store.dictionary_open(dict_id, server).unwrap();

        // The dictionary is empty.
        let (iterator, server_end) = create_proxy().unwrap();
        dict.enumerate(server_end).unwrap();
        let items = iterator.get_next().await.unwrap();
        assert_eq!(items.len(), 0);
    }

    /// Tests that the `Dict` contains an entry for a capability inserted via `Dict.Insert`,
    /// and that the value is the same capability.
    #[fuchsia::test]
    async fn serve_insert() -> Result<()> {
        let dict = Dict::new();

        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));
        let dict_ref = Capability::Dictionary(dict.clone()).into();
        let dict_id = 1;
        store.import(dict_id, dict_ref).await.unwrap().unwrap();

        let unit = Unit::default().into();
        let value = 2;
        store.import(value, unit).await.unwrap().unwrap();
        store
            .dictionary_insert(
                dict_id,
                &fsandbox::DictionaryItem { key: CAP_KEY.to_string(), value },
            )
            .await
            .unwrap()
            .unwrap();

        let mut dict = dict.lock();

        // Inserting adds the entry to `entries`.
        assert_eq!(dict.entries.len(), 1);

        // The entry that was inserted should now be in `entries`.
        let cap = dict.entries.remove(&*CAP_KEY).expect("not in entries after insert");
        let Capability::Unit(unit) = cap else { panic!("Bad capability type: {:#?}", cap) };
        assert_eq!(unit, Unit::default());

        Ok(())
    }

    /// Tests that removing an entry from the `Dict` via `Dict.Remove` yields the same capability
    /// that was previously inserted.
    #[fuchsia::test]
    async fn serve_remove() -> Result<(), Error> {
        let dict = Dict::new();

        // Insert a Unit into the Dict.
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict.lock().entries.len(), 1);

        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));
        let dict_ref = Capability::Dictionary(dict.clone()).into();
        let dict_id = 1;
        store.import(dict_id, dict_ref).await.unwrap().unwrap();

        let dest_id = 2;
        store
            .dictionary_remove(
                dict_id,
                CAP_KEY.as_str(),
                Some(&fsandbox::WrappedNewCapabilityId { value: dest_id }),
            )
            .await
            .unwrap()
            .unwrap();
        let cap = store.export(dest_id).await.unwrap().unwrap();
        // The value should be the same one that was previously inserted.
        assert_eq!(cap, Unit::default().into());

        // Removing the entry with Remove should remove it from `entries`.
        assert!(dict.lock().entries.is_empty());

        Ok(())
    }

    /// Tests that `Dict.Get` yields the same capability that was previously inserted.
    #[fuchsia::test]
    async fn serve_get() {
        let dict = Dict::new();

        // Insert a Unit and a Handle into the Dict.
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict.lock().entries.len(), 1);
        let (ch, _) = fidl::Channel::create();
        let handle = Handle::from(ch.into_handle());
        dict.insert("h".parse().unwrap(), Capability::Handle(handle)).unwrap();

        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));
        let dict_ref = Capability::Dictionary(dict.clone()).into();
        let dict_id = 1;
        store.import(dict_id, dict_ref).await.unwrap().unwrap();

        let dest_id = 2;
        store.dictionary_get(dict_id, CAP_KEY.as_str(), dest_id).await.unwrap().unwrap();
        let cap = store.export(dest_id).await.unwrap().unwrap();
        // The value should be the same one that was previously inserted.
        assert_eq!(cap, Unit::default().into());

        // Trying to get the handle capability should return NOT_CLONEABLE.
        let dest_id = 3;
        assert_matches!(
            store.dictionary_get(dict_id, "h", dest_id).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::NotDuplicatable)
        );

        // The capability should remain in the Dict.
        assert_eq!(dict.lock().entries.len(), 2);
    }

    /// Tests that `Dict.Insert` returns `ALREADY_EXISTS` when there is already an item with
    /// the same key.
    #[fuchsia::test]
    async fn insert_already_exists() -> Result<(), Error> {
        let dict = Dict::new();

        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));
        let dict_ref = Capability::Dictionary(dict.clone()).into();
        let dict_id = 1;
        store.import(dict_id, dict_ref).await.unwrap().unwrap();

        // Insert an entry.
        let unit = Unit::default().into();
        let value = 2;
        store.import(value, unit).await.unwrap().unwrap();
        store
            .dictionary_insert(
                dict_id,
                &fsandbox::DictionaryItem { key: CAP_KEY.to_string(), value },
            )
            .await
            .unwrap()
            .unwrap();

        // Inserting again should return an error.
        let unit = Unit::default().into();
        let value = 3;
        store.import(value, unit).await.unwrap().unwrap();
        let result = store
            .dictionary_insert(
                dict_id,
                &fsandbox::DictionaryItem { key: CAP_KEY.to_string(), value },
            )
            .await
            .unwrap();
        assert_matches!(result, Err(fsandbox::CapabilityStoreError::ItemAlreadyExists));

        Ok(())
    }

    /// Tests that the `Dict.Remove` returns `NOT_FOUND` when there is no item with the given key.
    #[fuchsia::test]
    async fn remove_not_found() -> Result<(), Error> {
        let dict = Dict::new();

        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));
        let dict_ref = Capability::Dictionary(dict.clone()).into();
        let dict_id = 1;
        store.import(dict_id, dict_ref).await.unwrap().unwrap();

        // Removing an item from an empty dict should fail.
        let dest_id = 2;
        let result = store
            .dictionary_remove(
                dict_id,
                CAP_KEY.as_str(),
                Some(&fsandbox::WrappedCapabilityId { value: dest_id }),
            )
            .await
            .unwrap();
        assert_matches!(result, Err(fsandbox::CapabilityStoreError::ItemNotFound));

        Ok(())
    }

    /// Tests that `copy` produces a new Dict with cloned entries.
    #[fuchsia::test]
    async fn copy() {
        // Create a Dict with a Unit inside, and copy the Dict.
        let dict = Dict::new();
        dict.insert("unit1".parse().unwrap(), Capability::Unit(Unit::default())).unwrap();

        let copy = dict.shallow_copy().unwrap();

        // Insert a Unit into the copy.
        copy.insert("unit2".parse().unwrap(), Capability::Unit(Unit::default())).unwrap();

        // The copy should have two Units.
        let copy = copy.lock();
        assert_eq!(copy.entries.len(), 2);
        assert!(copy.entries.values().all(|value| matches!(value, Capability::Unit(_))));

        // The original Dict should have only one Unit.
        let this = dict.lock();
        assert_eq!(this.entries.len(), 1);
        assert!(this.entries.values().all(|value| matches!(value, Capability::Unit(_))));
        drop(this);

        // Non-cloneable handle results in error
        let (ch, _) = fidl::Channel::create();
        let handle = Handle::from(ch.into_handle());
        dict.insert("h".parse().unwrap(), Capability::Handle(handle)).unwrap();
        assert_matches!(dict.shallow_copy(), Err(()));
    }

    /// Tests that cloning a Dict results in a Dict that shares the same entries.
    #[fuchsia::test]
    async fn clone_by_reference() -> Result<()> {
        let dict = Dict::new();
        let dict_clone = dict.clone();

        // Add a Unit into the clone.
        dict_clone.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict_clone.lock().entries.len(), 1);

        // The original dict should now have an entry because it shares entries with the clone.
        assert_eq!(dict.lock().entries.len(), 1);

        Ok(())
    }

    /// Tests basic functionality of Keys APIs.
    #[fuchsia::test]
    async fn read() {
        let dict = Dict::new();

        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));
        let dict_ref = Capability::Dictionary(dict).into();
        let dict_id = 1;
        store.import(dict_id, dict_ref).await.unwrap().unwrap();

        // Create two Data capabilities.
        let mut data_caps = vec![];
        for i in 1..3 {
            let value = 10 + i;
            store.import(value, Data::Int64(i.try_into().unwrap()).into()).await.unwrap().unwrap();
            data_caps.push(value);
        }

        // Add the Data capabilities to the dict.
        store
            .dictionary_insert(
                dict_id,
                &fsandbox::DictionaryItem { key: "cap1".into(), value: data_caps.remove(0) },
            )
            .await
            .unwrap()
            .unwrap();
        store
            .dictionary_insert(
                dict_id,
                &fsandbox::DictionaryItem { key: "cap2".into(), value: data_caps.remove(0) },
            )
            .await
            .unwrap()
            .unwrap();
        let (ch, _) = fidl::Channel::create();
        let handle = ch.into_handle();
        let value = 20;
        store.import(value, fsandbox::Capability::Handle(handle)).await.unwrap().unwrap();
        store
            .dictionary_insert(dict_id, &fsandbox::DictionaryItem { key: "cap3".into(), value })
            .await
            .unwrap()
            .unwrap();

        // Now read the entries back.
        let (iterator, server_end) = create_proxy().unwrap();
        store.dictionary_keys(dict_id, server_end).await.unwrap().unwrap();
        let keys = iterator.get_next().await.unwrap();
        assert!(iterator.get_next().await.unwrap().is_empty());
        assert_eq!(keys, ["cap1", "cap2", "cap3"]);
    }

    /// Tests batching for Keys iterator.
    #[fuchsia::test]
    async fn read_batches() {
        // Number of entries in the Dict that will be enumerated.
        //
        // This value was chosen such that that GetNext returns multiple chunks of different sizes.
        const NUM_ENTRIES: u32 = fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK * 2 + 1;

        // Number of items we expect in each chunk, for every chunk we expect to get.
        const EXPECTED_CHUNK_LENGTHS: &[u32] =
            &[fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK, fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK, 1];

        // Create a Dict with [NUM_ENTRIES] entries that have Unit values.
        let dict = Dict::new();
        for i in 0..NUM_ENTRIES {
            dict.insert(format!("{}", i).parse().unwrap(), Capability::Unit(Unit::default()))
                .unwrap();
        }

        let (store, stream) = create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));
        let dict_ref = Capability::Dictionary(dict.clone()).into();
        let dict_id = 1;
        store.import(dict_id, dict_ref).await.unwrap().unwrap();

        let (key_iterator, server_end) = create_proxy().unwrap();
        store.dictionary_keys(dict_id, server_end).await.unwrap().unwrap();

        // Get all the entries from the Dict with `GetNext`.
        let mut num_got_items: u32 = 0;
        for expected_len in EXPECTED_CHUNK_LENGTHS {
            let keys = key_iterator.get_next().await.unwrap();
            if keys.is_empty() {
                break;
            }
            assert_eq!(*expected_len, keys.len() as u32);
            num_got_items += keys.len() as u32;
        }

        // GetNext should return no items once all items have been returned.
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
        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
            request.open_remote(self)
        }
    }
    impl GetEntryInfo for MockDir {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
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
