// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`BlobDirectory`] node type used to represent a directory of immutable
//! content-addressable blobs.

use crate::fuchsia::component::map_to_raw_status;
use crate::fuchsia::directory::FxDirectory;
use crate::fuchsia::fxblob::blob::FxBlob;
use crate::fuchsia::fxblob::writer::DeliveryBlobWriter;
use crate::fuchsia::node::{FxNode, GetResult, OpenedNode};
use crate::fuchsia::volume::{FxVolume, RootDir};
use anyhow::{anyhow, ensure, Context as _, Error};
use async_trait::async_trait;
use fidl::endpoints::{create_request_stream, ClientEnd, ServerEnd};
use fidl_fuchsia_fxfs::{
    BlobCreatorRequest, BlobCreatorRequestStream, BlobReaderRequest, BlobReaderRequestStream,
    BlobWriterMarker, CreateBlobError,
};
use fidl_fuchsia_io::{self as fio, FilesystemInfo, NodeMarker, WatchMask};
use fuchsia_hash::Hash;
use fuchsia_merkle::{MerkleTree, MerkleTreeBuilder};
use futures::TryStreamExt;
use fxfs::errors::FxfsError;
use fxfs::object_handle::ReadObjectHandle;
use fxfs::object_store::transaction::{lock_keys, LockKey};
use fxfs::object_store::{
    self, HandleOptions, ObjectDescriptor, ObjectStore, BLOB_MERKLE_ATTRIBUTE_ID,
};
use fxfs::serialized_types::BlobMetadata;
use fxfs_macros::ToWeakNode;
use std::str::FromStr;
use std::sync::Arc;
use vfs::directory::dirents_sink::{self, Sink};
use vfs::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
use vfs::directory::entry_container::{
    Directory as VfsDirectory, DirectoryWatcher, MutableDirectory,
};
use vfs::directory::mutable::connection::MutableConnection;
use vfs::directory::traversal_position::TraversalPosition;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::{ObjectRequestRef, ToObjectRequest};
use zx::Status;

/// A flat directory containing content-addressable blobs (names are their hashes).
/// It is not possible to create sub-directories.
/// It is not possible to write to an existing blob.
/// It is not possible to open or read a blob until it is written and verified.
#[derive(ToWeakNode)]
pub struct BlobDirectory {
    directory: Arc<FxDirectory>,
}

/// Instead of constantly switching back and forth between strings and hashes. Do it once and then
/// just pass around a reference to that.
pub(crate) struct Identifier {
    pub string: String,
    pub hash: Hash,
}

impl TryFrom<&str> for Identifier {
    type Error = FxfsError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Ok(Self {
            string: value.to_owned(),
            hash: Hash::from_str(value).map_err(|_| FxfsError::InvalidArgs)?,
        })
    }
}

impl From<Hash> for Identifier {
    fn from(hash: Hash) -> Self {
        Self { string: hash.to_string(), hash }
    }
}

#[async_trait]
impl RootDir for BlobDirectory {
    fn as_directory_entry(self: Arc<Self>) -> Arc<dyn DirectoryEntry> {
        self
    }

    fn as_directory(self: Arc<Self>) -> Arc<dyn VfsDirectory> {
        self
    }

    fn as_node(self: Arc<Self>) -> Arc<dyn FxNode> {
        self as Arc<dyn FxNode>
    }

    fn on_open(self: Arc<Self>) {
        // It's safe for this task to be dropped when the volume is shut down.
        let scope = self.volume().scope().clone();
        scope.spawn(async move {
            if let Err(e) = self.prefetch_blobs().await {
                tracing::warn!("Failed to prefetch blobs: {:?}", e);
            }
        });
    }

    /// Handle fuchsia.fxfs/BlobCreator requests for this [`BlobDirectory`].
    async fn handle_blob_creator_requests(self: Arc<Self>, mut requests: BlobCreatorRequestStream) {
        while let Ok(Some(request)) = requests.try_next().await {
            match request {
                BlobCreatorRequest::Create { responder, hash, .. } => {
                    responder.send(self.create_blob_writer(Hash::from(hash)).await).unwrap_or_else(
                        |error| {
                            tracing::error!(?error, "failed to send Create response");
                        },
                    );
                }
            }
        }
    }

    /// Handle fuchsia.fxfs/BlobReader requests for this [`BlobDirectory`].
    async fn handle_blob_reader_requests(self: Arc<Self>, mut requests: BlobReaderRequestStream) {
        while let Ok(Some(request)) = requests.try_next().await {
            match request {
                BlobReaderRequest::GetVmo { blob_hash, responder } => {
                    responder
                        .send(self.get_blob_vmo(blob_hash.into()).await.map_err(map_to_raw_status))
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to send GetVmo response");
                        });
                }
            };
        }
    }
}

impl BlobDirectory {
    fn new(directory: FxDirectory) -> Self {
        fuchsia_merkle::crypto_library_init();
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

    async fn prefetch_blobs(self: &Arc<Self>) -> Result<(), Error> {
        let store = self.store();
        let fs = store.filesystem();

        let dirents = {
            let _guard = fs
                .lock_manager()
                .read_lock(lock_keys![LockKey::object(
                    store.store_object_id(),
                    self.directory.object_id()
                )])
                .await;
            let mut dirents = vec![];
            let layer_set = store.tree().layer_set();
            let mut merger = layer_set.merger();
            let mut iter = self.directory.directory().iter(&mut merger).await?;
            let mut num = 0;
            let limit = self.directory.directory().owner().dirent_cache().limit();
            while let Some((name, object_id, _)) = iter.get() {
                dirents.push((name.try_into()?, object_id));
                iter.advance().await?;
                num += 1;
                if num >= limit {
                    break;
                }
            }
            dirents
        };

        for (identifier, object_id) in dirents {
            if let Ok(node) = self.get_or_load_node(object_id, &identifier).await {
                self.directory.directory().owner().dirent_cache().insert(
                    self.directory.object_id(),
                    identifier.string,
                    node,
                );
            }
        }
        Ok(())
    }

    /// Attempt to lookup and cache the blob with `id` in this directory.
    ///
    /// *WARNING*: Use caution when performing operations on the returned node handle. Unlike
    /// [`Self::open_blob`], this function doesn't modify the node's open count, so the underlying
    /// node can still be purged/unlinked even if there are still references to the node.
    pub(crate) async fn lookup_blob(self: &Arc<Self>, hash: Hash) -> Result<Arc<FxBlob>, Error> {
        // For simplify lookup logic, we re-use `open_blob` just decrement the open count before
        // returning the node handle.
        self.open_blob(&hash.into()).await?.ok_or_else(|| FxfsError::NotFound.into()).map(|blob| {
            let node = blob.take();
            node.clone().open_count_sub_one();
            node
        })
    }

    /// Attempt to open and cache the blob with `id` in this directory. Returns `Ok(None)` if no
    /// blob matching `id` was found.
    pub(crate) async fn open_blob(
        self: &Arc<Self>,
        id: &Identifier,
    ) -> Result<Option<OpenedNode<FxBlob>>, Error> {
        let store = self.store();
        let fs = store.filesystem();
        let keys = lock_keys![LockKey::object(store.store_object_id(), self.directory.object_id())];
        // A lock needs to be held over searching the directory and incrementing the open count.
        let _guard = fs.lock_manager().read_lock(keys.clone()).await;

        let node = match self
            .directory
            .directory()
            .owner()
            .dirent_cache()
            .lookup(&(self.directory.object_id(), &id.string))
        {
            Some(node) => Some(node),
            None => {
                if let Some((object_id, _)) = self.directory.directory().lookup(&id.string).await? {
                    let node = self.get_or_load_node(object_id, &id).await?;
                    self.directory.directory().owner().dirent_cache().insert(
                        self.directory.object_id(),
                        id.string.clone(),
                        node.clone(),
                    );
                    Some(node)
                } else {
                    None
                }
            }
        };
        let Some(node) = node else {
            return Ok(None);
        };
        if node.object_descriptor() != ObjectDescriptor::File {
            return Err(FxfsError::Inconsistent)
                .with_context(|| format!("Blob {} has invalid object descriptor!", id.string));
        }
        node.into_any()
            .downcast::<FxBlob>()
            .map(|node| Some(OpenedNode::new(node)))
            .map_err(|_| FxfsError::Inconsistent)
            .with_context(|| format!("Blob {} has incorrect node type!", id.string))
    }

    // Attempts to get a node from the node cache. If the node wasn't present in the cache, loads
    // the object from the object store, installing the returned node into the cache and returns the
    // newly created FxNode backed by the loaded object.
    async fn get_or_load_node(
        self: &Arc<Self>,
        object_id: u64,
        id: &Identifier,
    ) -> Result<Arc<dyn FxNode>, Error> {
        let volume = self.volume();
        match volume.cache().get_or_reserve(object_id).await {
            GetResult::Node(node) => {
                // Protecting against the scenario where a directory entry points to another node
                // which has already been loaded and verified with the correct hash. We need to
                // verify that the hash for the blob that is cached here matches the requested hash.
                let blob = node.into_any().downcast::<FxBlob>().map_err(|_| {
                    anyhow!(FxfsError::Inconsistent).context("Loaded non-blob from cache")
                })?;
                ensure!(
                    blob.root() == id.hash,
                    anyhow!(FxfsError::Inconsistent)
                        .context("Loaded blob by node that did not match the given hash")
                );
                Ok(blob as Arc<dyn FxNode>)
            }
            GetResult::Placeholder(placeholder) => {
                let object =
                    ObjectStore::open_object(volume, object_id, HandleOptions::default(), None)
                        .await?;
                let (tree, metadata) = match object.read_attr(BLOB_MERKLE_ATTRIBUTE_ID).await? {
                    None => {
                        // If the file is uncompressed and is small enough, it may not have any
                        // metadata stored on disk.
                        (
                            MerkleTree::from_levels(vec![vec![id.hash]]),
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
                            MerkleTree::from_levels(vec![vec![id.hash]])
                        } else {
                            let mut builder = MerkleTreeBuilder::new();
                            for hash in std::mem::take(&mut metadata.hashes) {
                                builder.push_data_hash(hash.into());
                            }
                            let tree = builder.finish();
                            ensure!(tree.root() == id.hash, FxfsError::Inconsistent);
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

    /// Creates a [`ClientEnd<BlobWriterMarker>`] to write the delivery blob identified by `hash`.
    /// It is safe to create multiple writers for a given `hash`, however only one will succeed.
    /// Requests are handled asynchronously on this volume's execution scope.
    async fn create_blob_writer(
        self: &Arc<Self>,
        hash: Hash,
    ) -> Result<ClientEnd<BlobWriterMarker>, CreateBlobError> {
        let id = hash.into();
        let blob_exists = self
            .open_blob(&id)
            .await
            .map_err(|e| {
                tracing::error!("Failed to lookup blob: {:?}", e);
                CreateBlobError::Internal
            })?
            .is_some();
        if blob_exists {
            return Err(CreateBlobError::AlreadyExists);
        }
        let (client_end, request_stream) =
            create_request_stream::<BlobWriterMarker>().map_err(|e| {
                tracing::error!("Failed to create request stream for BlobWriter: {:?}", e);
                CreateBlobError::Internal
            })?;
        let writer = DeliveryBlobWriter::new(self, hash).await.map_err(|e| {
            tracing::error!("Failed to create blob writer: {:?}", e);
            CreateBlobError::Internal
        })?;
        self.volume().scope().spawn(async move {
            if let Err(e) = writer.handle_requests(request_stream).await {
                tracing::error!("Failed to handle BlobWriter requests: {}", e);
            }
        });
        return Ok(client_end);
    }
}

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

    fn open_count_add_one(&self) {}
    fn open_count_sub_one(self: Arc<Self>) {}

    fn object_descriptor(&self) -> ObjectDescriptor {
        ObjectDescriptor::Directory
    }
}

impl MutableDirectory for BlobDirectory {
    async fn unlink(self: Arc<Self>, name: &str, must_be_directory: bool) -> Result<(), Status> {
        if must_be_directory {
            return Err(Status::INVALID_ARGS);
        }
        self.directory.clone().unlink(name, must_be_directory).await
    }

    async fn update_attributes(
        &self,
        attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        self.directory.update_attributes(attributes).await
    }

    async fn sync(&self) -> Result<(), Status> {
        self.directory.sync().await
    }
}

/// Implementation of VFS pseudo-directory for blobs. Forks a task per connection.
impl DirectoryEntry for BlobDirectory {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_dir(self)
    }
}

impl GetEntryInfo for BlobDirectory {
    fn entry_info(&self) -> EntryInfo {
        self.directory.entry_info()
    }
}

impl vfs::node::Node for BlobDirectory {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        self.directory.get_attributes(requested_attributes).await
    }

    fn query_filesystem(&self) -> Result<FilesystemInfo, Status> {
        self.directory.query_filesystem()
    }
}

/// Implements VFS entry container trait for directories, allowing manipulation of their contents.
impl VfsDirectory for BlobDirectory {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(move |object_request| {
            if path.is_empty() {
                object_request.spawn_connection(
                    scope,
                    OpenedNode::new(self).take(),
                    flags,
                    MutableConnection::create,
                )
            } else {
                Err(Status::NOT_SUPPORTED)
            }
        });
    }

    fn open3(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        object_request.take().handle(|object_request| {
            if path.is_empty() {
                object_request.spawn_connection(
                    scope,
                    OpenedNode::new(self).take(),
                    flags,
                    MutableConnection::create,
                )
            } else {
                Err(Status::NOT_SUPPORTED)
            }
        });
        Ok(())
    }

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
}

impl From<object_store::Directory<FxVolume>> for BlobDirectory {
    fn from(dir: object_store::Directory<FxVolume>) -> Self {
        Self::new(dir.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuchsia::fxblob::testing::{new_blob_fixture, open_blob_fixture, BlobFixture};
    use assert_matches::assert_matches;
    use blob_writer::BlobWriter;
    use delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob};
    use fidl_fuchsia_fxfs::BlobReaderMarker;
    use fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _};
    use fuchsia_component::client::connect_to_protocol_at_dir_svc;
    use fuchsia_fs::directory::{
        readdir_inclusive, DirEntry, DirentKind, WatchEvent, WatchMessage, Watcher,
    };

    use futures::StreamExt as _;
    use std::path::PathBuf;

    #[fasync::run(10, test)]
    async fn test_unlink() {
        let fixture = new_blob_fixture().await;

        let data = [1; 1000];

        let hash = fixture.write_blob(&data, CompressionMode::Never).await;

        assert_eq!(fixture.read_blob(hash).await, data);

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
            hash = fuchsia_merkle::from_slice(&data).root();
            let compressed_data: Vec<u8> = Type1Blob::generate(&data, CompressionMode::Always);

            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true)
                .expect("failed to serve blob volume");
            let blob_proxy =
                connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
                    &blob_volume_outgoing_dir,
                )
                .expect("failed to connect to the Blob service");

            let blob_writer_client_end = blob_proxy
                .create(&hash.into(), false)
                .await
                .expect("transport error on create")
                .expect("failed to create blob");

            let writer = blob_writer_client_end.into_proxy();
            let mut blob_writer = BlobWriter::create(writer, compressed_data.len() as u64)
                .await
                .expect("failed to create BlobWriter");
            blob_writer.write(&compressed_data[..1]).await.unwrap();

            // Before the blob is finished writing, it shouldn't appear in the directory.
            assert_eq!(
                readdir_inclusive(fixture.root()).await.ok(),
                Some(vec![DirEntry { name: ".".to_string(), kind: DirentKind::Directory }])
            );

            blob_writer.write(&compressed_data[1..]).await.unwrap();
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
            let hash = fuchsia_merkle::from_slice(&datum).root();
            let filename = PathBuf::from(format!("{}", hash));
            hashes.push(hash.clone());
            filenames.push(filename.clone());

            let compressed_data: Vec<u8> = Type1Blob::generate(&datum, CompressionMode::Always);
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true)
                .expect("failed to serve blob volume");
            let blob_proxy =
                connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
                    &blob_volume_outgoing_dir,
                )
                .expect("failed to connect to the Blob service");

            let blob_writer_client_end = blob_proxy
                .create(&hash.into(), false)
                .await
                .expect("transport error on create")
                .expect("failed to create blob");

            let writer = blob_writer_client_end.into_proxy();
            let mut blob_writer = BlobWriter::create(writer, compressed_data.len() as u64)
                .await
                .expect("failed to create BlobWriter");
            blob_writer.write(&compressed_data[..compressed_data.len() - 1]).await.unwrap();

            // Before the blob is finished writing, we shouldn't see any watch events for it.
            assert_matches!(
                watcher
                    .next()
                    .on_timeout(zx::MonotonicDuration::from_millis(500).after_now(), || None)
                    .await,
                None
            );

            blob_writer.write(&compressed_data[compressed_data.len() - 1..]).await.unwrap();

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
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;

        let (status, token) = fixture.root().get_token().await.expect("FIDL failed");
        Status::ok(status).unwrap();
        fixture
            .root()
            .rename(&format!("{}", delivery_blob_path(hash)), token.unwrap().into(), "foo")
            .await
            .expect("FIDL failed")
            .expect_err("rename should fail");

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_link_fails() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;

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

    #[fasync::run(10, test)]
    async fn test_verify_cached_hash_node() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;
        let evil_hash =
            Hash::from_str("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();

        // Create a malicious link to the existing blob. This shouldn't be possible without special
        // access either via internal apis or modifying the disk image.
        {
            let root = fixture
                .volume()
                .root()
                .clone()
                .as_node()
                .into_any()
                .downcast::<BlobDirectory>()
                .unwrap()
                .directory()
                .clone();
            root.clone()
                .link(evil_hash.to_string(), root, &hash.to_string())
                .await
                .expect("Linking file");
        }
        let device = fixture.close().await;

        let fixture = open_blob_fixture(device).await;
        {
            // Hold open a ref to keep it in the node cache.
            let _vmo = fixture.get_blob_vmo(hash).await;

            // Open the malicious link
            let blob_reader =
                connect_to_protocol_at_dir_svc::<BlobReaderMarker>(fixture.volume_out_dir())
                    .expect("failed to connect to the BlobReader service");
            blob_reader
                .get_vmo(&evil_hash.into())
                .await
                .expect("transport error on BlobReader.GetVmo")
                .expect_err("Hashes should mismatch");
        }
        fixture.close().await;
    }
}
