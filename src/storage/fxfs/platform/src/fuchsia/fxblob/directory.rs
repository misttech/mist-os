// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`BlobDirectory`] node type used to represent a directory of immutable
//! content-addressable blobs.

use {
    crate::fuchsia::{
        directory::FxDirectory,
        errors::map_to_status,
        fxblob::{
            blob::FxBlob,
            writer::{BlobWriterProtocol as _, FxUnsealedBlob},
        },
        node::{FxNode, GetResult, OpenedNode},
        volume::{FxVolume, RootDir},
    },
    anyhow::{bail, ensure, Error},
    async_trait::async_trait,
    fidl::endpoints::{create_proxy, ClientEnd, Proxy as _, ServerEnd},
    fidl_fuchsia_fxfs::{
        BlobWriterMarker, BlobWriterRequest, CreateBlobError, WriteBlobRequest,
        WriteBlobRequestStream,
    },
    fidl_fuchsia_io::{
        self as fio, FilesystemInfo, NodeAttributeFlags, NodeAttributes, NodeMarker, WatchMask,
    },
    fuchsia_async as fasync,
    fuchsia_hash::Hash,
    fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
    fuchsia_zircon::{self as zx, Status},
    futures::{lock::Mutex as AsyncMutex, FutureExt, TryStreamExt},
    fxfs::{
        errors::FxfsError,
        log::*,
        object_handle::{ObjectHandle, ObjectProperties},
        object_store::{
            self,
            directory::ObjectDescriptor,
            transaction::{LockKey, Options},
            HandleOptions, ObjectStore, BLOB_MERKLE_ATTRIBUTE_ID,
        },
        serialized_types::BlobMetadata,
    },
    std::{collections::HashMap, str::FromStr, sync::Arc},
    vfs::{
        directory::{
            dirents_sink::{self, Sink},
            entry::{DirectoryEntry, EntryInfo},
            entry_container::{DirectoryWatcher, MutableDirectory},
            mutable::connection::io1::MutableConnection,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        file::FidlIoConnection,
        path::Path,
        ToObjectRequest,
    },
};

/// A flat directory containing content-addressable blobs (names are their hashes).
/// It is not possible to create sub-directories.
/// It is not possible to write to an existing blob.
/// It is not possible to open or read a blob until it is written and verified.
pub struct BlobDirectory {
    directory: Arc<FxDirectory>,
}

#[async_trait]
impl RootDir for BlobDirectory {
    fn as_directory_entry(self: Arc<Self>) -> Arc<dyn DirectoryEntry> {
        self as Arc<dyn DirectoryEntry>
    }

    fn as_node(self: Arc<Self>) -> Arc<dyn FxNode> {
        self as Arc<dyn FxNode>
    }

    async fn handle_blob_requests(
        self: Arc<Self>,
        mut requests: WriteBlobRequestStream,
    ) -> Result<(), Error> {
        let blob_state: AsyncMutex<HashMap<Hash, fasync::Task<()>>> =
            AsyncMutex::new(HashMap::default());
        while let Some(request) = requests.try_next().await? {
            match request {
                WriteBlobRequest::Create { responder, hash, .. } => {
                    let mut blob_state = blob_state.lock().await;
                    let hash = Hash::from(hash);
                    let res = if blob_state.contains_key(&hash) {
                        Err(CreateBlobError::AlreadyExists)
                    } else {
                        match self.create_blob(&hash).await {
                            Ok((task, client_end)) => {
                                blob_state.insert(hash, task);
                                Ok(client_end)
                            }
                            Err(e) => {
                                tracing::error!("blob service: create failed: {:?}", e);
                                Err(e)
                            }
                        }
                    };
                    responder.send(res).unwrap_or_else(|e| {
                        tracing::error!("failed to send Create response. error: {:?}", e);
                    });
                }
            }
        }
        for (_, task) in std::mem::take(&mut *blob_state.lock().await) {
            task.await;
        }
        Ok(())
    }
}

impl BlobDirectory {
    fn new(directory: FxDirectory) -> Self {
        Self { directory: Arc::new(directory) }
    }

    pub fn directory(&self) -> &Arc<FxDirectory> {
        &self.directory
    }

    pub fn volume(&self) -> &Arc<FxVolume> {
        self.directory.volume()
    }

    fn store(&self) -> &ObjectStore {
        self.directory.store()
    }

    async fn lookup(
        self: &Arc<Self>,
        flags: fio::OpenFlags,
        mut path: Path,
    ) -> Result<OpenedNode<dyn FxNode>, Error> {
        if path.is_empty() {
            return Ok(OpenedNode::new(self.clone()));
        }
        if !path.is_single_component() {
            bail!(FxfsError::NotFound);
        }
        let store = self.store();
        let fs = store.filesystem();
        let name = path.next().unwrap();

        // TODO(fxbug.dev/122125): Create the transaction here if we might need to create the object
        // so that we have a lock in place.
        let keys = [LockKey::object(store.store_object_id(), self.directory.object_id())];

        // A lock needs to be held over searching the directory and incrementing the open count.
        let guard = fs.read_lock(&keys).await;

        match self.directory.directory().lookup(name).await? {
            Some((object_id, object_descriptor)) => {
                ensure!(!flags.contains(fio::OpenFlags::RIGHT_WRITABLE), FxfsError::AccessDenied);
                ensure!(
                    !flags.contains(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT),
                    FxfsError::AlreadyExists
                );
                match object_descriptor {
                    ObjectDescriptor::File => {
                        ensure!(!flags.contains(fio::OpenFlags::DIRECTORY), FxfsError::NotDir)
                    }
                    _ => bail!(FxfsError::Inconsistent),
                }
                // TODO(fxbug.dev/122125): Test that we can't open a blob while still writing it.
                Ok(OpenedNode::new(self.get_or_load_node(object_id, name).await?))
            }
            None => {
                std::mem::drop(guard);

                ensure!(flags.contains(fio::OpenFlags::CREATE), FxfsError::NotFound);

                let mut transaction = fs.clone().new_transaction(&keys, Options::default()).await?;

                let volume = self.volume();

                let node = OpenedNode::new(FxUnsealedBlob::new(
                    self.clone(),
                    Hash::from_str(name).map_err(|_| FxfsError::InvalidArgs)?,
                    ObjectStore::create_object(
                        volume,
                        &mut transaction,
                        HandleOptions::default(),
                        store.crypt().as_deref(),
                        None,
                    )
                    .await?,
                ) as Arc<dyn FxNode>);

                // Add the object to the graveyard so that it's cleaned up if we crash.
                store.add_to_graveyard(&mut transaction, node.object_id());

                // Note that we don't bother notifying watchers yet.  Nothing else should be able to
                // see this object yet.
                transaction.commit().await?;

                Ok(node)
            }
        }
    }

    // Attempts to get a node from the node cache. If the node wasn't present in the cache, loads
    // the object from the object store, installing the returned node into the cache and returns the
    // newly created FxNode backed by the loaded object.
    async fn get_or_load_node(
        self: &Arc<Self>,
        object_id: u64,
        name: &str,
    ) -> Result<Arc<dyn FxNode>, Error> {
        let volume = self.volume();
        match volume.cache().get_or_reserve(object_id).await {
            GetResult::Node(node) => Ok(node),
            GetResult::Placeholder(placeholder) => {
                let hash = Hash::from_str(name).map_err(|_| FxfsError::Inconsistent)?;
                let object = ObjectStore::open_object(
                    volume,
                    object_id,
                    HandleOptions::default(),
                    volume.store().crypt(),
                )
                .await?;
                let (tree, metadata) = match object.read_attr(BLOB_MERKLE_ATTRIBUTE_ID).await? {
                    None => {
                        // If the file is uncompressed and is small enough, it may not have any
                        // metadata stored on disk.
                        (
                            MerkleTree::from_levels(vec![vec![hash]]),
                            BlobMetadata {
                                hashes: vec![],
                                chunk_size: 0,
                                compressed_offsets: vec![],
                                uncompressed_size: object.get_size(),
                            },
                        )
                    }
                    Some(data) => {
                        let mut metadata: BlobMetadata = bincode::deserialize_from(&*data)?;
                        let tree = if metadata.hashes.is_empty() {
                            MerkleTree::from_levels(vec![vec![hash]])
                        } else {
                            let mut builder = MerkleTreeBuilder::new();
                            for hash in std::mem::take(&mut metadata.hashes) {
                                builder.push_data_hash(hash.into());
                            }
                            let tree = builder.finish();
                            ensure!(tree.root() == hash, FxfsError::Inconsistent);
                            tree
                        };
                        (tree, metadata)
                    }
                };

                let node = FxBlob::new(
                    object,
                    tree,
                    metadata.chunk_size,
                    metadata.compressed_offsets,
                    metadata.uncompressed_size,
                ) as Arc<dyn FxNode>;
                placeholder.commit(&node);
                Ok(node)
            }
        }
    }

    async fn create_blob(
        self: &Arc<Self>,
        hash: &Hash,
    ) -> Result<(fasync::Task<()>, ClientEnd<BlobWriterMarker>), CreateBlobError> {
        let flags = fio::OpenFlags::CREATE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_READABLE;
        let path = Path::validate_and_split(hash.to_string()).map_err(|e| {
            tracing::error!("failed to validate path: {:?}", e);
            CreateBlobError::Internal
        })?;
        let node = self.lookup(flags, path).await.map_err(|e| {
            tracing::error!("lookup failed: {:?}", e);
            CreateBlobError::Internal
        })?;
        if !node.is::<FxUnsealedBlob>() {
            return Err(CreateBlobError::AlreadyExists);
        }
        let unsealed_blob = node.downcast::<FxUnsealedBlob>().unwrap_or_else(|_| unreachable!());
        let (client, server_end) = create_proxy::<BlobWriterMarker>().map_err(|e| {
            tracing::error!("create_proxy failed for the BlobWriter protocol: {:?}", e);
            CreateBlobError::Internal
        })?;
        let client_channel = client.into_channel().map_err(|_| {
            tracing::error!("failed to create client channel");
            CreateBlobError::Internal
        })?;
        let client_end = ClientEnd::new(client_channel.into());
        let this = self.clone();
        let task = fasync::Task::spawn(async move {
            if let Err(e) = this.handle_blob_writer_requests(unsealed_blob, server_end).await {
                tracing::error!("Failed to handle blob writer requests: {}", e);
            }
        });
        return Ok((task, client_end));
    }

    async fn handle_blob_writer_requests(
        self: &Arc<Self>,
        blob: OpenedNode<FxUnsealedBlob>,
        server_end: ServerEnd<BlobWriterMarker>,
    ) -> Result<(), Error> {
        let mut stream = server_end.into_stream()?;
        while let Some(request) = stream.try_next().await? {
            match request {
                BlobWriterRequest::GetVmo { size, responder } => {
                    let res = match blob.as_ref().get_vmo(size).await {
                        Ok(vmo) => Ok(vmo),
                        Err(e) => {
                            tracing::error!("blob service: get_vmo failed: {:?}", e);
                            Err(zx::Status::INTERNAL.into_raw())
                        }
                    };
                    responder.send(res).unwrap_or_else(|e| {
                        tracing::error!("failed to send GetVmo response. error: {:?}", e);
                    });
                }
                BlobWriterRequest::BytesReady { bytes_written, responder } => {
                    let res = match blob.as_ref().bytes_ready(bytes_written).await {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            tracing::error!("blob service: bytes_ready failed: {:?}", e);
                            Err(zx::Status::INTERNAL.into_raw())
                        }
                    };
                    responder.send(res).unwrap_or_else(|e| {
                        tracing::error!("failed to send BytesReady response. error: {:?}", e);
                    });
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl FxNode for BlobDirectory {
    fn object_id(&self) -> u64 {
        self.directory.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        self.directory.parent()
    }

    fn set_parent(&self, _parent: Arc<FxDirectory>) {
        // This directory can't be renamed.
        unreachable!();
    }

    async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        self.directory.get_properties().await
    }

    fn open_count_add_one(&self) {}
    fn open_count_sub_one(self: Arc<Self>) {}
}

#[async_trait]
impl MutableDirectory for BlobDirectory {
    async fn unlink(self: Arc<Self>, name: &str, must_be_directory: bool) -> Result<(), Status> {
        if must_be_directory {
            return Err(Status::INVALID_ARGS);
        }
        self.directory.clone().unlink(name, must_be_directory).await
    }

    async fn set_attrs(
        &self,
        flags: NodeAttributeFlags,
        attrs: NodeAttributes,
    ) -> Result<(), Status> {
        self.directory.set_attrs(flags, attrs).await
    }

    async fn sync(&self) -> Result<(), Status> {
        self.directory.sync().await
    }

    async fn rename(
        self: Arc<Self>,
        _src_dir: Arc<dyn vfs::directory::entry_container::MutableDirectory + 'static>,
        _src_name: Path,
        _dst_name: Path,
    ) -> Result<(), Status> {
        // Files in a blob directory can't be renamed.
        Err(Status::NOT_SUPPORTED)
    }
}

/// Implementation of VFS pseudo-directory for blobs. Forks a task per connection.
impl DirectoryEntry for BlobDirectory {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<NodeMarker>,
    ) {
        flags.to_object_request(server_end).spawn(&scope.clone(), move |object_request| {
            async move {
                let node = self.lookup(flags, path).await.map_err(|e| {
                    debug!(?e, "lookup failed");
                    map_to_status(e)
                })?;
                if node.is::<BlobDirectory>() {
                    object_request.create_connection(
                        scope,
                        node.downcast::<BlobDirectory>().unwrap_or_else(|_| unreachable!()).take(),
                        flags,
                        MutableConnection::create,
                    )
                } else if node.is::<FxBlob>() {
                    let node = node.downcast::<FxBlob>().unwrap_or_else(|_| unreachable!());
                    FxBlob::create_connection_async(node, scope, flags, object_request)
                } else if node.is::<FxUnsealedBlob>() {
                    let node = node.downcast::<FxUnsealedBlob>().unwrap_or_else(|_| unreachable!());
                    object_request.create_connection(
                        scope,
                        node.take(),
                        flags,
                        FidlIoConnection::create,
                    )
                } else {
                    unreachable!();
                }
            }
            .boxed()
        });
    }

    fn entry_info(&self) -> EntryInfo {
        self.directory.entry_info()
    }
}

#[async_trait]
impl vfs::node::Node for BlobDirectory {
    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        self.directory.get_attrs().await
    }
}

/// Implements VFS entry container trait for directories, allowing manipulation of their contents.
#[async_trait]
impl vfs::directory::entry_container::Directory for BlobDirectory {
    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<dyn Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), Status> {
        self.directory.read_dirents(pos, sink).await
    }

    fn register_watcher(
        self: Arc<Self>,
        scope: ExecutionScope,
        mask: WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), Status> {
        self.directory.clone().register_watcher(scope, mask, watcher)
    }

    fn unregister_watcher(self: Arc<Self>, key: usize) {
        self.directory.clone().unregister_watcher(key)
    }

    fn query_filesystem(&self) -> Result<FilesystemInfo, Status> {
        self.directory.query_filesystem()
    }
}

impl From<object_store::Directory<FxVolume>> for BlobDirectory {
    fn from(dir: object_store::Directory<FxVolume>) -> Self {
        Self::new(dir.into())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::fuchsia::{
            fxblob::testing::{new_blob_fixture, BlobFixture},
            testing::open_file_checked,
        },
        assert_matches::assert_matches,
        fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _},
        fuchsia_fs::directory::{
            readdir_inclusive, DirEntry, DirentKind, WatchEvent, WatchMessage, Watcher,
        },
        fuchsia_zircon::DurationNum as _,
        futures::StreamExt as _,
        std::path::PathBuf,
    };

    #[fasync::run(10, test)]
    async fn test_unlink() {
        let fixture = new_blob_fixture().await;

        let data = [1; 1000];

        let hash = fixture.write_blob(&data).await;

        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);

        fixture
            .root()
            .unlink(&format!("{}", hash), &fio::UnlinkOptions::default())
            .await
            .expect("FIDL failed")
            .expect("unlink failed");

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_readdir() {
        let fixture = new_blob_fixture().await;

        let data = [0xab; 2];
        let hash;
        {
            let mut builder = MerkleTreeBuilder::new();
            builder.write(&data);
            hash = builder.finish().root();

            let blob = open_file_checked(
                fixture.root(),
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE,
                &format!("{}", hash),
            )
            .await;
            blob.resize(data.len() as u64)
                .await
                .expect("FIDL call failed")
                .expect("truncate failed");
            assert_eq!(
                blob.write(&data[..1]).await.expect("FIDL call failed").expect("write failed"),
                1u64
            );
            // Before the blob is finished writing, it shouldn't appear in the directory.
            assert_eq!(
                readdir_inclusive(fixture.root()).await.ok(),
                Some(vec![DirEntry { name: ".".to_string(), kind: DirentKind::Directory }])
            );

            assert_eq!(
                blob.write(&data[1..]).await.expect("FIDL call failed").expect("write failed"),
                1u64
            );
        }

        assert_eq!(
            readdir_inclusive(fixture.root()).await.ok(),
            Some(vec![
                DirEntry { name: ".".to_string(), kind: DirentKind::Directory },
                DirEntry { name: format! {"{}", hash}, kind: DirentKind::File },
            ])
        );

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_watchers() {
        let fixture = new_blob_fixture().await;

        let mut watcher = Watcher::new(fixture.root()).await.unwrap();
        assert_eq!(
            watcher.next().await,
            Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
        );
        assert_matches!(
            watcher.next().await,
            Some(Ok(WatchMessage { event: WatchEvent::IDLE, .. }))
        );

        let data = vec![vec![0xab; 2], vec![0xcd; 65_536]];
        let mut hashes = vec![];
        let mut filenames = vec![];
        for datum in data {
            let mut builder = MerkleTreeBuilder::new();
            builder.write(&datum);
            let hash = builder.finish().root();
            let filename = PathBuf::from(format!("{}", hash));
            hashes.push(hash.clone());
            filenames.push(filename.clone());

            let blob = open_file_checked(
                fixture.root(),
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE,
                &format!("{}", hash),
            )
            .await;
            blob.resize(datum.len() as u64)
                .await
                .expect("FIDL call failed")
                .expect("truncate failed");
            let len = datum.len();
            for chunk in datum[..len - 1].chunks(fio::MAX_TRANSFER_SIZE as usize) {
                assert_eq!(
                    blob.write(chunk).await.expect("FIDL call failed").expect("write failed"),
                    chunk.len() as u64
                );
            }
            // Before the blob is finished writing, we shouldn't see any watch events for it.
            assert_matches!(
                watcher.next().on_timeout(500.millis().after_now(), || None).await,
                None
            );

            assert_eq!(
                blob.write(&datum[len - 1..])
                    .await
                    .expect("FIDL call failed")
                    .expect("write failed"),
                1u64
            );
            assert_eq!(
                watcher.next().await,
                Some(Ok(WatchMessage { event: WatchEvent::ADD_FILE, filename }))
            );
        }

        for (hash, filename) in hashes.iter().zip(filenames) {
            fixture
                .root()
                .unlink(&format!("{}", hash), &fio::UnlinkOptions::default())
                .await
                .expect("FIDL call failed")
                .expect("unlink failed");
            assert_eq!(
                watcher.next().await,
                Some(Ok(WatchMessage { event: WatchEvent::REMOVE_FILE, filename }))
            );
        }

        std::mem::drop(watcher);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_rename_fails() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data).await;

        let (status, token) = fixture.root().get_token().await.expect("FIDL failed");
        Status::ok(status).unwrap();
        fixture
            .root()
            .rename(&format!("{}", hash), token.unwrap().into(), "foo")
            .await
            .expect("FIDL failed")
            .expect_err("rename should fail");

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_link_fails() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data).await;

        let (status, token) = fixture.root().get_token().await.expect("FIDL failed");
        Status::ok(status).unwrap();
        let status = fixture
            .root()
            .link(&format!("{}", hash), token.unwrap().into(), "foo")
            .await
            .expect("FIDL failed");
        assert_eq!(Status::from_raw(status), Status::NOT_SUPPORTED);

        fixture.close().await;
    }
}
