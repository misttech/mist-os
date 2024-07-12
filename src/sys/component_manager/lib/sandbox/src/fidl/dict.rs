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
                            Ok(Some(cap)) => Ok(cap.into()),
                            Ok(None) => {
                                (self.lock().not_found)(key.as_str());
                                Err(fsandbox::DictionaryError::NotFound)
                            }
                            Err(()) => Err(fsandbox::DictionaryError::NotCloneable),
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
                                (self.lock().not_found)(key.as_str());
                                Err(fsandbox::DictionaryError::NotFound)
                            }
                        }
                    })();
                    responder.send(result)?;
                }
                fsandbox::DictionaryRequest::Copy { responder } => {
                    let result = self
                        .shallow_copy()
                        .map(fsandbox::DictionaryRef::from)
                        .map_err(|_| fsandbox::DictionaryError::NotCloneable);
                    responder.send(result)?;
                }
                fsandbox::DictionaryRequest::Enumerate { iterator: server_end, .. } => {
                    let items = self.enumerate();
                    let stream = server_end.into_stream().unwrap();
                    let mut this = self.lock();
                    this.tasks().spawn(serve_enumerate_iterator(items, stream));
                }
                fsandbox::DictionaryRequest::Keys { iterator: server_end, .. } => {
                    let keys = self.keys().collect();
                    let stream = server_end.into_stream().unwrap();
                    let mut this = self.lock();
                    this.tasks().spawn(serve_keys_iterator(keys, stream));
                }
                fsandbox::DictionaryRequest::Drain { iterator: server_end, .. } => {
                    // Take out entries, replacing with an empty BTreeMap.
                    // They are dropped if the caller does not request an iterator.
                    if let Some(server_end) = server_end {
                        let items = self.drain();
                        let stream = server_end.into_stream().unwrap();
                        let mut this = self.lock();
                        this.tasks().spawn(serve_drain_iterator(items, stream));
                    }
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

async fn serve_drain_iterator(
    mut items: impl Iterator<Item = (Key, Capability)>,
    mut stream: fsandbox::DictionaryDrainIteratorRequestStream,
) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::DictionaryDrainIteratorRequest::GetNext { responder } => {
                let mut chunk = vec![];
                for _ in 0..fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK {
                    match items.next() {
                        Some((key, value)) => {
                            chunk.push(fsandbox::DictionaryItem {
                                key: key.into(),
                                value: value.into(),
                            });
                        }
                        None => break,
                    }
                }
                let _ = responder.send(chunk);
            }
            fsandbox::DictionaryDrainIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryDrainIterator request");
            }
        }
    }
}

async fn serve_keys_iterator(
    keys: Vec<Key>,
    mut stream: fsandbox::DictionaryKeysIteratorRequestStream,
) {
    let mut keys = keys.into_iter();
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
    use crate::{Data, Dict, DirEntry, Directory, Handle, Unit};
    use anyhow::{Error, Result};
    use assert_matches::assert_matches;
    use fidl::endpoints::{
        create_endpoints, create_proxy, create_proxy_and_stream, ClientEnd, Proxy, ServerEnd,
    };
    use fidl::handle::{Channel, HandleBased, Status};
    use fuchsia_fs::directory;
    use futures::try_join;
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

    /// Tests that the `Dict` contains an entry for a capability inserted via `Dict.Insert`,
    /// and that the value is the same capability.
    #[fuchsia::test]
    async fn serve_insert() -> Result<()> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.do_serve(dict_stream);

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
        let mut dict = Dict::new();

        // Insert a Unit into the Dict.
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict.lock().entries.len(), 1);

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.do_serve(dict_stream);

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
        assert!(dict.lock().entries.is_empty());

        Ok(())
    }

    /// Tests that `Dict.Get` yields the same capability that was previously inserted.
    #[fuchsia::test]
    async fn serve_get() {
        let mut dict = Dict::new();

        // Insert a Unit and a Handle into the Dict.
        dict.insert(CAP_KEY.clone(), Capability::Unit(Unit::default())).unwrap();
        assert_eq!(dict.lock().entries.len(), 1);
        let (ch, _) = fidl::Channel::create();
        let handle = Handle::from(ch.into_handle());
        dict.insert("h".parse().unwrap(), Capability::Handle(handle)).unwrap();

        let (dict_proxy, dict_stream) =
            create_proxy_and_stream::<fsandbox::DictionaryMarker>().unwrap();
        let server = dict.do_serve(dict_stream);

        let client = async move {
            let cap = dict_proxy.get(CAP_KEY.as_str()).await.unwrap().unwrap();

            // The value should be the same one that was previously inserted.
            assert_eq!(cap, Unit::default().into());

            // Trying to get the handle capability should return NOT_CLONEABLE.
            assert_matches!(
                dict_proxy.get("h").await.unwrap(),
                Err(fsandbox::DictionaryError::NotCloneable)
            );

            Ok(())
        };

        try_join!(client, server).unwrap();

        // The capability should remain in the Dict.
        assert_eq!(dict.lock().entries.len(), 2);
    }

    /// Tests that `Dict.Insert` returns `ALREADY_EXISTS` when there is already an item with
    /// the same key.
    #[fuchsia::test]
    async fn insert_already_exists() -> Result<(), Error> {
        let mut dict = Dict::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>()?;
        let server = dict.do_serve(dict_stream);

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
        let server = dict.do_serve(dict_stream);

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

    /// Tests basic functionality of Enumerate and Keys APIs.
    #[fuchsia::test]
    async fn read() {
        let dict = Dict::new();

        let (dict_proxy, dict_stream) =
            create_proxy_and_stream::<fsandbox::DictionaryMarker>().unwrap();
        dict.serve(dict_stream);

        // Create two Data capabilities.
        let mut data_caps: Vec<_> = (1..3).map(|i| Data::Int64(i)).collect();

        // Add the Data capabilities to the dict.
        dict_proxy.insert("cap1", data_caps.remove(0).into()).await.unwrap().unwrap();
        dict_proxy.insert("cap2", data_caps.remove(0).into()).await.unwrap().unwrap();
        // This item is not cloneable so only the key will appear in the results.
        let (ch, _) = fidl::Channel::create();
        let handle = ch.into_handle();
        dict_proxy.insert("cap3", fsandbox::Capability::Handle(handle)).await.unwrap().unwrap();

        // Now read the entries back.
        let (iterator, server_end) = create_proxy().unwrap();
        dict_proxy.enumerate(server_end).unwrap();
        let mut items = iterator.get_next().await.unwrap();
        assert!(iterator.get_next().await.unwrap().is_empty());
        assert_eq!(items.len(), 3);
        assert_matches!(
            items.remove(0),
            fsandbox::DictionaryFallibleItem {
                key,
                value: fsandbox::DictionaryValueResult::Ok(value),
            }
            if key == "cap1" &&
            value == fsandbox::Capability::Data(fsandbox::Data::Int64(1))
        );
        assert_matches!(
            items.remove(0),
            fsandbox::DictionaryFallibleItem {
                key,
                value: fsandbox::DictionaryValueResult::Ok(value),
            }
            if key == "cap2" &&
            value == fsandbox::Capability::Data(fsandbox::Data::Int64(2))
        );
        assert_matches!(
            items.remove(0),
            fsandbox::DictionaryFallibleItem {
                key,
                value: fsandbox::DictionaryValueResult::Error(e),
            }
            if key == "cap3" &&
            e == fsandbox::DictionaryError::NotCloneable
        );

        let (iterator, server_end) = create_proxy().unwrap();
        dict_proxy.keys(server_end).unwrap();
        let keys = iterator.get_next().await.unwrap();
        assert!(iterator.get_next().await.unwrap().is_empty());
        assert_eq!(keys, ["cap1", "cap2", "cap3"]);
    }

    /// Tests batching for Enumerate and Keys iterators.
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

        let (dict_proxy, stream) = create_proxy_and_stream::<fsandbox::DictionaryMarker>().unwrap();
        dict.serve(stream);

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
                assert_eq!(item.value, fsandbox::DictionaryValueResult::Ok(Unit::default().into()));
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
