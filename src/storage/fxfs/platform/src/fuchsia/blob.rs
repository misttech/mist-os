// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        directory::FxDirectory,
        errors::map_to_status,
        node::{FxNode, GetResult, OpenedNode},
        pager::{PagerBackedVmo, TransferBuffers, TRANSFER_BUFFER_MAX_SIZE},
        vmo_data_buffer::VmoDataBuffer,
        volume::info_to_filesystem_info,
        volume::FxVolume,
    },
    anyhow::{anyhow, bail, ensure, Context, Error},
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io::{
        self as fio, FilesystemInfo, NodeAttributeFlags, NodeAttributes, NodeMarker, VmoFlags,
        WatchMask,
    },
    fuchsia_component::client::connect_to_protocol,
    fuchsia_hash::Hash,
    fuchsia_merkle::{hash_block, MerkleTree, MerkleTreeBuilder},
    fuchsia_zircon::Status,
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::{channel::oneshot, future::BoxFuture, join, FutureExt},
    fxfs::{
        async_enter,
        errors::FxfsError,
        log::*,
        object_handle::{
            GetProperties, ObjectHandle, ObjectProperties, ReadObjectHandle, WriteObjectHandle,
        },
        object_store::{
            self,
            directory::{self, ObjectDescriptor},
            transaction::{LockKey, Options},
            HandleOptions, HandleOwner, ObjectStore, StoreObjectHandle, Timestamp,
            BLOB_MERKLE_ATTRIBUTE_ID,
        },
        round::{round_down, round_up},
        serialized_types::BlobMetadata,
    },
    once_cell::sync::Lazy,
    once_cell::sync::OnceCell,
    std::{
        io::Read,
        ops::Range,
        str::FromStr,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    },
    storage_device::buffer,
    vfs::{
        common::rights_to_posix_mode_bits,
        directory::{
            dirents_sink::{self, Sink},
            entry::{DirectoryEntry, EntryInfo},
            entry_container::{DirectoryWatcher, MutableDirectory},
            mutable::connection::io1::MutableConnection,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        file::{
            connection::io1::{
                create_connection_async, create_node_reference_connection_async,
                create_stream_connection_async,
            },
            File, FileIo, FileOptions,
        },
        path::Path,
        ObjectRequest, ObjectRequestRef, ProtocolsExt, ToObjectRequest,
    },
};

pub(crate) const BLOCK_SIZE: u64 = fuchsia_merkle::BLOCK_SIZE as u64;

pub(crate) const READ_AHEAD_SIZE: u64 = 131_072;

/// A flat directory containing content-addressable blobs (names are their hashes).
/// It is not possible to create sub-directories.
/// It is not possible to write to an existing blob.
/// It is not possible to open or read a blob until it is written and verified.
pub struct BlobDirectory {
    directory: Arc<FxDirectory>,
}

impl BlobDirectory {
    fn new(directory: FxDirectory) -> Self {
        Self { directory: Arc::new(directory) }
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

    fn open_count_add_one(&self) {
        self.directory.open_count_add_one()
    }

    fn open_count_sub_one(&self) {
        self.directory.open_count_sub_one()
    }
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
        flags.to_object_request(server_end).spawn(
            &scope.clone(),
            move |object_request, shutdown| {
                async move {
                    let node = self.lookup(flags, path).await.map_err(|e| {
                        debug!(?e, "lookup failed");
                        map_to_status(e)
                    })?;
                    if node.is::<BlobDirectory>() {
                        Ok(MutableConnection::create_connection_async(
                            scope,
                            node.downcast::<BlobDirectory>()
                                .unwrap_or_else(|_| unreachable!())
                                .take(),
                            flags.to_directory_options()?,
                            object_request.take(),
                            shutdown,
                        )
                        .boxed())
                    } else if node.is::<FxBlob>() {
                        let node = node.downcast::<FxBlob>().unwrap_or_else(|_| unreachable!());
                        FxBlob::create_connection_async(
                            node,
                            scope,
                            flags.to_file_options()?,
                            object_request,
                            shutdown,
                        )
                    } else if node.is::<FxUnsealedBlob>() {
                        let node =
                            node.downcast::<FxUnsealedBlob>().unwrap_or_else(|_| unreachable!());
                        Ok(FxUnsealedBlob::create_connection(
                            node,
                            scope,
                            flags.to_file_options()?,
                            object_request.take(),
                            shutdown,
                        )
                        .boxed())
                    } else {
                        unreachable!();
                    }
                }
                .boxed()
            },
        );
    }

    fn entry_info(&self) -> EntryInfo {
        self.directory.entry_info()
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

    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        self.directory.get_attrs().await
    }

    fn close(&self) -> Result<(), Status> {
        self.directory.close()
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

// When the top bit of the open count is set, it means the file has been deleted and when the count
// drops to zero, it will be tombstoned.  Once it has dropped to zero, it cannot be opened again
// (assertions will fire).
const PURGED: usize = 1 << (usize::BITS - 1);

/// Represents an immutable blob stored on Fxfs with associated an merkle tree.
pub struct FxBlob {
    handle: StoreObjectHandle<FxVolume>,
    buffer: VmoDataBuffer,
    open_count: AtomicUsize,
    merkle_tree: MerkleTree,
    compressed_chunk_size: u64,
    compressed_offsets: Vec<u64>,
    uncompressed_size: u64,
}

impl FxBlob {
    pub fn new(
        handle: StoreObjectHandle<FxVolume>,
        merkle_tree: MerkleTree,
        compressed_chunk_size: u64,
        compressed_offsets: Vec<u64>,
        uncompressed_size: u64,
    ) -> Arc<Self> {
        let buffer = handle.owner().create_data_buffer(handle.object_id(), uncompressed_size);
        let file = Arc::new(Self {
            handle,
            buffer,
            open_count: AtomicUsize::new(0),
            merkle_tree,
            compressed_chunk_size,
            compressed_offsets,
            uncompressed_size,
        });
        file.handle.owner().pager().register_file(&file);
        file
    }

    fn create_connection_async(
        this: OpenedNode<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
        shutdown: oneshot::Receiver<()>,
    ) -> Result<BoxFuture<'static, ()>, zx::Status> {
        Ok(if options.is_node {
            create_node_reference_connection_async(
                scope,
                this.take(),
                options,
                object_request.take(),
                shutdown,
            )
            .boxed()
        } else {
            let stream_options = options
                .to_stream_options()
                .unwrap_or_else(|| panic!("Invalid options for stream connection: {options:?}"));
            let stream = zx::Stream::create(stream_options, this.buffer.vmo(), 0)?;
            create_stream_connection_async(
                scope,
                this.take(),
                options,
                object_request.take(),
                /*readable=*/ true,
                /*writable=*/ false,
                /*executable=*/ true,
                stream,
                shutdown,
            )
            .boxed()
        })
    }
}

/// Implement VFS pseudo-directory entry for a blob.
impl DirectoryEntry for FxBlob {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<NodeMarker>,
    ) {
        flags.to_object_request(server_end).spawn(
            &scope.clone(),
            move |object_request, shutdown| {
                Box::pin(async move {
                    if !path.is_empty() {
                        return Err(Status::NOT_FILE);
                    }
                    Self::create_connection_async(
                        OpenedNode::new(self),
                        scope,
                        flags.to_file_options()?,
                        object_request,
                        shutdown,
                    )
                })
            },
        );
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.object_id(), fio::DirentType::File)
    }
}

impl Drop for FxBlob {
    fn drop(&mut self) {
        let volume = self.handle.owner();
        volume.cache().remove(self);
        volume.pager().unregister_file(self);
    }
}

/// Implements an on-demand paged VMO that decompresses blobs on the fly from a compressed on-disk
/// representation.
impl FxBlob {
    async fn read_uncached(&self, range: Range<u64>) -> Result<buffer::Buffer<'_>, Error> {
        let mut buffer = self.handle.allocate_buffer((range.end - range.start) as usize);
        // TODO(fxbug.dev/122125): zero the tail
        let read = if self.compressed_offsets.is_empty() {
            self.handle.read(range.start, buffer.as_mut()).await?
        } else {
            ensure!(self.compressed_chunk_size > 0, FxfsError::Inconsistent);
            let indices = (range.start / self.compressed_chunk_size) as usize
                ..(range.end / self.compressed_chunk_size) as usize;
            let seek_table_len = self.compressed_offsets.len();
            ensure!(
                indices.start < seek_table_len && indices.end <= seek_table_len,
                anyhow!(FxfsError::OutOfRange).context(format!(
                    "Out of bounds seek table access {:?}, len {}",
                    indices, seek_table_len
                ))
            );
            let compressed_offsets = self.compressed_offsets[indices.start]
                ..if indices.end == seek_table_len {
                    self.handle.get_size()
                } else {
                    self.compressed_offsets[indices.end]
                };
            let bs = self.handle.block_size();
            let aligned = round_down(compressed_offsets.start, bs)
                ..round_up(compressed_offsets.end, bs).unwrap();
            let mut compressed_buf =
                self.handle.allocate_buffer((aligned.end - aligned.start) as usize);
            let read =
                self.handle.read(aligned.start, compressed_buf.as_mut()).await.context(format!(
                    "Failed to read compressed range {:?}, len {}",
                    aligned,
                    self.handle.get_size()
                ))?;
            let compressed_buf_range = (compressed_offsets.start - aligned.start) as usize
                ..(compressed_offsets.end - aligned.start) as usize;
            ensure!(
                read >= compressed_buf_range.end - compressed_buf_range.start,
                anyhow!(FxfsError::Inconsistent).context(format!(
                    "Unexpected EOF, read {}, but expected {}",
                    read,
                    compressed_buf_range.end - compressed_buf_range.start,
                ))
            );
            let len = (std::cmp::min(range.end, self.uncompressed_size) - range.start) as usize;
            let buf = buffer.as_mut_slice();
            zstd::Decoder::new(std::io::Cursor::new(
                &compressed_buf.as_slice()[compressed_buf_range],
            ))?
            .read_exact(&mut buf[..len])?;
            len
        };
        // TODO(fxbug.dev/122055): This should be offloaded to the kernel at which point we can
        // delete this.
        let hashes = &self.merkle_tree.as_ref()[0];
        let mut offset = range.start as usize;
        let bs = BLOCK_SIZE as usize;
        for b in buffer.as_slice()[..read].chunks(bs) {
            ensure!(
                hash_block(b, offset) == hashes[offset / bs],
                anyhow!(FxfsError::Inconsistent).context("Hash mismatch")
            );
            offset += bs;
        }
        Ok(buffer)
    }

    fn align_range(&self, range: Range<u64>) -> Range<u64> {
        if self.compressed_offsets.is_empty() {
            round_down(range.start, BLOCK_SIZE)..round_up(range.end, BLOCK_SIZE).unwrap()
        } else {
            round_down(range.start, self.compressed_chunk_size)
                ..round_up(range.end, self.compressed_chunk_size).unwrap()
        }
    }
}

/// Implements VFS pseudo-filesystem entries for blobs.
#[async_trait]
impl FxNode for FxBlob {
    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        unreachable!(); // Add a parent back-reference if needed.
    }

    fn set_parent(&self, _parent: Arc<FxDirectory>) {
        // NOP
    }

    async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        unimplemented!()
    }

    fn open_count_add_one(&self) {
        let old = self.open_count.fetch_add(1, Ordering::Relaxed);
        assert!(old != PURGED && old != PURGED - 1);
    }

    fn open_count_sub_one(&self) {
        let old = self.open_count.fetch_sub(1, Ordering::Relaxed);
        assert!(old & !PURGED > 0);
        if old == PURGED + 1 {
            let store = self.handle.store();
            store
                .filesystem()
                .graveyard()
                .queue_tombstone(store.store_object_id(), self.object_id());
        }
    }
}

static VMEX_RESOURCE: OnceCell<zx::Resource> = OnceCell::new();

/// Takes the vmex resource routed to us and stashes it into a global variable.
pub async fn init_vmex_resource() -> Result<(), Error> {
    let client = connect_to_protocol::<fidl_fuchsia_kernel::VmexResourceMarker>()?;
    VMEX_RESOURCE.set(client.get().await?).map_err(|_| anyhow!(FxfsError::AlreadyBound))
}

/// Implement VFS trait so blobs can be accessed as files.
#[async_trait]
impl File for FxBlob {
    async fn open(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, _length: u64) -> Result<(), Status> {
        Err(Status::ACCESS_DENIED)
    }

    async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
        // We do not support exact/duplicate sharing mode.
        if flags.contains(VmoFlags::SHARED_BUFFER) {
            error!("get_backing_memory does not support exact sharing mode!");
            return Err(Status::NOT_SUPPORTED);
        }
        // We only support the combination of WRITE when a private COW clone is explicitly
        // specified. This implicitly restricts any mmap call that attempts to use MAP_SHARED +
        // PROT_WRITE.
        if flags.contains(VmoFlags::WRITE) && !flags.contains(VmoFlags::PRIVATE_CLONE) {
            error!("get_buffer only supports VmoFlags::WRITE with VmoFlags::PRIVATE_CLONE!");
            return Err(Status::NOT_SUPPORTED);
        }

        let vmo = self.buffer.vmo();
        let size = self.uncompressed_size;

        let mut child_options = zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE;
        // By default, SNAPSHOT includes WRITE, so we explicitly remove it if not required.
        if !flags.contains(VmoFlags::WRITE) {
            child_options |= zx::VmoChildOptions::NO_WRITE
        }

        let mut child_vmo = vmo.create_child(child_options, 0, size)?;

        if flags.contains(VmoFlags::EXECUTE) {
            // TODO(fxbug.dev/122125): Filter out other flags.
            child_vmo = child_vmo
                .replace_as_executable(VMEX_RESOURCE.get().ok_or(Status::NOT_SUPPORTED)?)?;
        }

        if self.handle.owner().pager().watch_for_zero_children(self).map_err(map_to_status)? {
            // Take an open count so that we keep this object alive if it is otherwise closed.
            self.open_count_add_one();
        }

        Ok(child_vmo)
    }

    async fn get_size(&self) -> Result<u64, Status> {
        Ok(self.uncompressed_size)
    }

    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        let props = self.handle.get_properties().await.map_err(map_to_status)?;
        Ok(NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ true),
            id: self.handle.object_id(),
            content_size: self.uncompressed_size,
            storage_size: props.allocated_size,
            link_count: props.refs,
            creation_time: props.creation_time.as_nanos(),
            modification_time: props.modification_time.as_nanos(),
        })
    }

    async fn set_attrs(
        &self,
        _flags: NodeAttributeFlags,
        _attrs: NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::ACCESS_DENIED)
    }

    async fn close(&self) -> Result<(), Status> {
        self.open_count_sub_one();
        Ok(())
    }

    async fn sync(&self) -> Result<(), Status> {
        Ok(())
    }

    fn query_filesystem(&self) -> Result<FilesystemInfo, Status> {
        let store = self.handle.store();
        Ok(info_to_filesystem_info(
            store.filesystem().get_info(),
            store.filesystem().block_size(),
            store.object_count(),
            self.handle.owner().id(),
        ))
    }

    fn event(&self) -> Result<Option<zx::Event>, Status> {
        let event = zx::Event::create();
        // The file is immediately readable (see `fuchsia.io2.File.Describe`).
        event.signal_handle(zx::Signals::empty(), zx::Signals::USER_0)?;
        Ok(Some(event))
    }
}

#[async_trait]
impl PagerBackedVmo for FxBlob {
    fn pager_key(&self) -> u64 {
        self.handle.object_id()
    }

    fn vmo(&self) -> &zx::Vmo {
        self.buffer.vmo()
    }

    // TODO(fxbug.dev/122125): refactor and share with file.rs
    async fn page_in(self: Arc<Self>, mut range: Range<u64>) {
        async_enter!("page_in");
        const ZERO_VMO_SIZE: u64 = TRANSFER_BUFFER_MAX_SIZE;
        static ZERO_VMO: Lazy<zx::Vmo> = Lazy::new(|| zx::Vmo::create(ZERO_VMO_SIZE).unwrap());

        let vmo = self.vmo();
        let aligned_size = round_up(self.uncompressed_size, zx::system_get_page_size()).unwrap();
        let mut offset = std::cmp::max(range.start, aligned_size);
        while offset < range.end {
            let end = std::cmp::min(range.end, offset + ZERO_VMO_SIZE);
            self.handle.owner().pager().supply_pages(vmo, offset..end, &ZERO_VMO, 0);
            offset = end;
        }
        if aligned_size < range.end {
            range.end = aligned_size;
        } else {
            range = round_down(range.start, READ_AHEAD_SIZE)
                ..round_up(range.end, READ_AHEAD_SIZE).unwrap();
            if range.end > aligned_size {
                range.end = aligned_size;
            }
        }
        if range.end <= range.start {
            return;
        }
        range.start = round_down(range.start, self.handle.block_size());
        range = self.align_range(range);

        static TRANSFER_BUFFERS: Lazy<TransferBuffers> = Lazy::new(|| TransferBuffers::new());
        let (buffer_result, transfer_buffer) = join!(self.read_uncached(range.clone()), async {
            let buffer = TRANSFER_BUFFERS.get().await;
            // Committing pages in the kernel is time consuming, so we do this in parallel
            // to the read.  This assumes that the implementation of join! polls the other
            // future first (which happens to be the case for now).
            buffer.commit(range.end - range.start);
            buffer
        });
        let buffer = match buffer_result {
            Ok(buffer) => buffer,
            Err(e) => {
                error!(
                    ?range,
                    merkle_root = %self.merkle_tree.root(),
                    ?self.uncompressed_size,
                    error = e.as_value(),
                    "Failed to load range"
                );
                // TODO(fxbug.dev/122125): Should we fuse further reads shut?  This would match
                // blobfs' behaviour.
                self.handle.owner().pager().report_failure(
                    self.vmo(),
                    range.clone(),
                    map_to_status(e),
                );
                return;
            }
        };
        let mut buf = buffer.as_slice();
        // TODO(fxbug.dev/122125): read_uncached should return a buffer representing the correct
        // size
        if range.start + buf.len() as u64 > aligned_size {
            buf = &buf[..(aligned_size - range.start) as usize];
        }
        while !buf.is_empty() {
            let (source, remainder) =
                buf.split_at(std::cmp::min(buf.len(), TRANSFER_BUFFER_MAX_SIZE as usize));
            buf = remainder;
            let range_chunk = range.start..range.start + source.len() as u64;
            match transfer_buffer.vmo().write(source, transfer_buffer.offset()) {
                Ok(_) => {
                    self.handle.owner().pager().supply_pages(
                        self.vmo(),
                        range_chunk,
                        transfer_buffer.vmo(),
                        transfer_buffer.offset(),
                    );
                }
                Err(e) => {
                    // Failures here due to OOM will get reported as IO errors, as those are
                    // considered transient.
                    error!(
                            range = ?range_chunk,
                            error = e.as_value(),
                            "Failed to transfer range");
                    self.handle.owner().pager().report_failure(
                        self.vmo(),
                        range_chunk,
                        zx::Status::IO,
                    );
                }
            }
            range.start += source.len() as u64;
        }
    }

    async fn mark_dirty(self: Arc<Self>, _range: Range<u64>) {
        unreachable!();
    }

    fn on_zero_children(&self) {
        self.open_count_sub_one();
    }
}

/// Represents a blob that is being written.
/// The blob cannot be read until writes complete and hash is verified.
/// Another blob of the same name (hash) cannot be written at the same time.
pub struct FxUnsealedBlob {
    parent: Arc<BlobDirectory>,
    hash: Hash,
    handle: StoreObjectHandle<FxVolume>,
    open_count: AtomicUsize,
    inner: Mutex<Inner>,
}

struct Inner {
    writing: bool,
    write_offset: u64,
    merkle_builder: Option<MerkleTreeBuilder>,
    buffer: Vec<u8>,
}

impl FxUnsealedBlob {
    pub fn new(
        parent: Arc<BlobDirectory>,
        hash: Hash,
        handle: StoreObjectHandle<FxVolume>,
    ) -> Arc<Self> {
        let file = Arc::new(Self {
            parent,
            hash,
            handle,
            open_count: AtomicUsize::new(0),
            inner: Mutex::new(Inner {
                writing: false,
                write_offset: 0,
                merkle_builder: Some(MerkleTreeBuilder::new()),
                buffer: Vec::new(),
            }),
        });
        file
    }

    async fn complete(&self, size: u64, tree: MerkleTree) -> Result<(), Error> {
        self.handle.flush().await?;
        // Write the Merkle tree if it has more than a single hash.
        if tree.as_ref().len() > 1 {
            let mut hashes = Vec::new();
            for hash in &tree.as_ref()[0] {
                hashes.push(**hash);
            }
            let mut serialized = Vec::new();
            let metadata = BlobMetadata {
                hashes,
                chunk_size: 0,
                compressed_offsets: Vec::new(),
                uncompressed_size: size,
            };
            bincode::serialize_into(&mut serialized, &metadata).unwrap();
            // TODO(fxbug.dev/122125): Is this the best place to store merkle tree data?
            // (Inline attribute, prepended to data, ..?)
            self.handle.write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &serialized).await?;
        }

        // We've finished writing the blob, so promote this blob.
        let volume = self.handle.owner();
        let store = self.handle.store();

        let dir = volume
            .cache()
            .get(store.root_directory_object_id())
            .unwrap()
            .into_any()
            .downcast::<BlobDirectory>()
            .unwrap_or_else(|_| panic!("Expected blob directory"));

        let keys = [LockKey::object(store.store_object_id(), dir.object_id())];
        let mut transaction = store.filesystem().new_transaction(&keys, Options::default()).await?;

        let object_id = self.handle.object_id();
        store.remove_from_graveyard(&mut transaction, object_id);

        let name = format!("{}", self.hash);
        directory::replace_child_with_object(
            &mut transaction,
            Some((object_id, ObjectDescriptor::File)),
            (dir.directory.directory(), &name),
            0,
            Timestamp::now(),
        )
        .await?;

        let parent = self.parent().unwrap();
        transaction.commit_with_callback(|_| parent.did_add(&name)).await?;
        Ok(())
    }
}

// Note the asymmetry in read/write paths. Blobs are written compressed and read decompressed.
// Writes to FxUnsealedBlob are assumed to be pre-compressed, so we don't need the complexity found
// in FxBlob.

impl FxUnsealedBlob {
    async fn create_connection(
        this: OpenedNode<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequest,
        shutdown: oneshot::Receiver<()>,
    ) {
        create_connection_async(
            scope,
            this.take(),
            options,
            object_request,
            /*readable=*/ true,
            /*writable=*/ true,
            /*executable=*/ false,
            shutdown,
        )
        .await
    }
}

impl DirectoryEntry for FxUnsealedBlob {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<NodeMarker>,
    ) {
        flags.to_object_request(server_end).spawn(
            &scope.clone(),
            move |object_request, shutdown| {
                Box::pin(async move {
                    if !path.is_empty() {
                        return Err(Status::NOT_FILE);
                    }
                    Ok(Self::create_connection(
                        OpenedNode::new(self),
                        scope,
                        flags.to_file_options()?,
                        object_request.take(),
                        shutdown,
                    ))
                })
            },
        );
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.object_id(), fio::DirentType::File)
    }
}

#[async_trait]
impl FxNode for FxUnsealedBlob {
    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        Some(self.parent.directory.clone())
    }

    fn set_parent(&self, _parent: Arc<FxDirectory>) {
        unreachable!()
    }

    async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        unimplemented!()
    }

    fn open_count_add_one(&self) {
        let old = self.open_count.fetch_add(1, Ordering::Relaxed);
        assert!(old != PURGED && old != PURGED - 1);
    }

    fn open_count_sub_one(&self) {
        let old = self.open_count.fetch_sub(1, Ordering::Relaxed);
        assert!(old & !PURGED > 0);
        if old == PURGED + 1 {
            let store = self.handle.store();
            store
                .filesystem()
                .graveyard()
                .queue_tombstone(store.store_object_id(), self.object_id());
        }
    }
}

#[async_trait]
impl File for FxUnsealedBlob {
    async fn open(&self, _optionss: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, length: u64) -> Result<(), Status> {
        if length == 0 {
            // TODO(fxbug.dev/122125): There could be races when truncating concurrently.
            let tree = self.inner.lock().unwrap().merkle_builder.take().unwrap().finish();
            if tree.root() != self.hash {
                return Err(Status::IO_DATA_INTEGRITY);
            }
            return self.complete(0, tree).await.map_err(map_to_status);
        }
        // TODO(fxbug.dev/122125): This needs some locks.
        // TODO(fxbug.dev/122125): What if truncate has already been called.
        let mut transaction = self.handle.new_transaction().await.map_err(map_to_status)?;
        self.handle
            .preallocate_range(
                &mut transaction,
                0..round_up(length, self.handle.block_size()).unwrap(),
            )
            .await
            .map_err(map_to_status)?;
        self.handle.grow(&mut transaction, 0, length).await.map_err(map_to_status)?;
        transaction.commit().await.map_err(map_to_status)?;
        Ok(())
    }

    async fn get_size(&self) -> Result<u64, Status> {
        Ok(self.handle.get_size())
    }

    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        let props = self.handle.get_properties().await.map_err(map_to_status)?;
        Ok(NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ true, /*x*/ false),
            id: self.handle.object_id(),
            content_size: props.data_attribute_size,
            storage_size: props.allocated_size,
            link_count: props.refs,
            creation_time: props.creation_time.as_nanos(),
            modification_time: props.modification_time.as_nanos(),
        })
    }

    async fn set_attrs(
        &self,
        _flags: NodeAttributeFlags,
        _attrs: NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::ACCESS_DENIED)
    }

    async fn close(&self) -> Result<(), Status> {
        Ok(())
    }

    async fn sync(&self) -> Result<(), Status> {
        // TODO(fxbug.dev/122125): Implement this.
        Ok(())
    }

    fn query_filesystem(&self) -> Result<FilesystemInfo, Status> {
        let store = self.handle.store();
        Ok(info_to_filesystem_info(
            store.filesystem().get_info(),
            store.filesystem().block_size(),
            store.object_count(),
            self.handle.owner().id(),
        ))
    }

    async fn get_backing_memory(&self, _flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
        unimplemented!()
    }
}

#[async_trait]
impl FileIo for FxUnsealedBlob {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status> {
        let mut buf = self.handle.allocate_buffer(buffer.len());
        let read = self.handle.read(offset, buf.as_mut()).await.map_err(map_to_status)?;
        buffer[..read].copy_from_slice(&buf.as_slice()[..read]);
        Ok(read as u64)
    }

    // TODO(fxbug.dev/122125): Support receiving blobs in the Delivery Format (RFC-0207).
    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.merkle_builder.is_none() || inner.writing {
                return Err(Status::BAD_STATE);
            }
            if offset != inner.write_offset {
                return Err(Status::NOT_SUPPORTED);
            }
            inner.writing = true;
        }

        // Clean up if we are dropped.
        struct CleanUp<'a>(&'a FxUnsealedBlob);
        impl Drop for CleanUp<'_> {
            fn drop(&mut self) {
                self.0.inner.lock().unwrap().writing = false;
            }
        }
        let clean_up = CleanUp(self);

        let size = self.handle.get_size();
        if content.len() as u64 > size - offset {
            return Err(Status::INVALID_ARGS);
        }

        let new_len = offset + content.len() as u64;

        let (write_offset, buffer, tree) = {
            let mut inner = self.inner.lock().unwrap();
            inner.buffer.extend_from_slice(&content);
            inner.write_offset = new_len;
            inner.merkle_builder.as_mut().unwrap().write(content);
            inner.writing = false;
            std::mem::forget(clean_up);

            let write_offset = new_len - inner.buffer.len() as u64;

            let tree = if new_len == size {
                // Verify the hash.
                let tree = inner.merkle_builder.take().unwrap().finish();
                if tree.root() != self.hash {
                    return Err(Status::IO_DATA_INTEGRITY);
                }
                Some(tree)
            } else {
                None
            };

            let buffer = if inner.buffer.len() >= 131072 || tree.is_some() {
                let bs = self.handle.block_size() as usize;
                let len = inner.buffer.len();
                let (len, aligned_len) = if tree.is_some() {
                    (len, round_down(len + bs - 1, bs))
                } else {
                    let len = round_down(len, bs);
                    (len, len)
                };
                let mut buffer = self.handle.allocate_buffer(aligned_len);
                buffer.as_mut_slice()[..len].copy_from_slice(&inner.buffer[..len]);
                buffer.as_mut_slice()[len..].fill(0);
                inner.buffer.drain(..len);
                Some(buffer)
            } else {
                None
            };

            (write_offset, buffer, tree)
        };

        if let Some(mut buffer) = buffer {
            self.handle
                .overwrite(write_offset, buffer.as_mut(), false)
                .await
                .map_err(map_to_status)?;
        }

        if let Some(tree) = tree {
            self.complete(size, tree).await.map_err(map_to_status)?;
        }

        Ok(content.len() as u64)
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        Err(Status::NOT_SUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{BLOCK_SIZE, READ_AHEAD_SIZE},
        crate::fuchsia::{
            testing::{open_file_checked, TestFixture, TestFixtureOptions},
            volume::FxVolume,
        },
        assert_matches::assert_matches,
        async_trait::async_trait,
        fidl_fuchsia_io::{self as fio, MAX_TRANSFER_SIZE},
        fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _},
        fuchsia_fs::directory::{
            readdir_inclusive, DirEntry, DirentKind, WatchEvent, WatchMessage, Watcher,
        },
        fuchsia_merkle::{Hash, MerkleTreeBuilder},
        fuchsia_zircon::{self as zx, DurationNum as _},
        futures::StreamExt as _,
        fxfs::{
            object_handle::{ObjectHandle as _, WriteBytes as _},
            object_store::{
                directory::Directory,
                transaction::{LockKey, TransactionHandler as _},
                DirectWriter, HandleOptions, ObjectStore, StoreObjectHandle,
                BLOB_MERKLE_ATTRIBUTE_ID,
            },
            round::round_up,
            serialized_types::BlobMetadata,
        },
        std::path::PathBuf,
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

    async fn new_blob_fixture() -> TestFixture {
        TestFixture::open(
            DeviceHolder::new(FakeDevice::new(16384, 512)),
            TestFixtureOptions { encrypted: false, as_blob: true, format: true },
        )
        .await
    }

    #[async_trait]
    trait BlobFixture {
        async fn write_blob(&self, data: &[u8]) -> Hash;
        async fn read_blob(&self, name: &str) -> Vec<u8>;
        async fn get_blob_handle(&self, name: &str) -> StoreObjectHandle<FxVolume>;
    }

    #[async_trait]
    impl BlobFixture for TestFixture {
        async fn write_blob(&self, data: &[u8]) -> Hash {
            let mut builder = MerkleTreeBuilder::new();
            builder.write(&data);
            let hash = builder.finish().root();

            let blob = open_file_checked(
                self.root(),
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
            for chunk in data.chunks(MAX_TRANSFER_SIZE as usize) {
                assert_eq!(
                    blob.write(&chunk).await.expect("FIDL call failed").expect("write failed"),
                    chunk.len() as u64
                );
            }

            hash
        }

        async fn read_blob(&self, name: &str) -> Vec<u8> {
            let blob = open_file_checked(self.root(), fio::OpenFlags::RIGHT_READABLE, name).await;
            let mut data = Vec::new();
            loop {
                let chunk = blob
                    .read(MAX_TRANSFER_SIZE)
                    .await
                    .expect("FIDL call failed")
                    .expect("read failed");
                let done = chunk.len() < MAX_TRANSFER_SIZE as usize;
                data.extend(chunk);
                if done {
                    break;
                }
            }
            data
        }

        async fn get_blob_handle(&self, name: &str) -> StoreObjectHandle<FxVolume> {
            let root_object_id = self.volume().volume().store().root_directory_object_id();
            let root_dir =
                Directory::open(self.volume().volume(), root_object_id).await.expect("open failed");
            let (object_id, _) = root_dir
                .lookup(name)
                .await
                .expect("lookup failed")
                .expect("file doesn't exist yet");

            ObjectStore::open_object(
                self.volume().volume(),
                object_id,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open_object failed")
        }
    }

    #[fasync::run(10, test)]
    async fn test_simple() {
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
    async fn test_empty_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data).await;
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_large_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![3; 3_000_000];
        let hash = fixture.write_blob(&data).await;

        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_large_compressed_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![3; 3_000_000];
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let tree = builder.finish();
        {
            // Manually insert the blob with our own metadata.
            // TODO(fxbug.dev/122056): Refactor to share implementation with blob.rs and make-blob-image.
            let root_object_id = fixture.volume().volume().store().root_directory_object_id();
            let root_dir = Directory::open(fixture.volume().volume(), root_object_id)
                .await
                .expect("open failed");

            let handle;
            let keys = [LockKey::object(
                fixture.volume().volume().store().store_object_id(),
                root_object_id,
            )];
            let mut transaction =
                fixture.fs().clone().new_transaction(&keys, Default::default()).await.unwrap();
            handle = root_dir
                .create_child_file(&mut transaction, &format!("{}", tree.root()))
                .await
                .unwrap();
            transaction.commit().await.unwrap();

            let mut writer = DirectWriter::new(&handle, Default::default());
            let mut compressed_offsets = vec![];
            let mut offset = 0;
            let chunk_size = round_up(data.len() as u64 / 2, BLOCK_SIZE).unwrap();
            for chunk in data.chunks(chunk_size as usize) {
                let mut compressor = zstd::bulk::Compressor::new(1).ok().unwrap();
                compressor
                    .set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))
                    .ok()
                    .unwrap();
                let contents = compressor.compress(&chunk).unwrap();
                compressed_offsets.push(offset);
                offset += contents.len() as u64;
                writer.write_bytes(&contents[..]).await.unwrap();
            }
            writer.complete().await.unwrap();

            let mut serialized = Vec::new();
            let len = data.len() as u64;
            bincode::serialize_into(
                &mut serialized,
                &BlobMetadata {
                    hashes: tree.as_ref()[0]
                        .clone()
                        .into_iter()
                        .map(|h| h.into())
                        .collect::<Vec<[u8; 32]>>(),
                    chunk_size,
                    compressed_offsets,
                    uncompressed_size: len,
                },
            )
            .unwrap();
            handle.write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &serialized).await.unwrap();
        }

        assert_eq!(fixture.read_blob(&format!("{}", tree.root())).await, data);

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_blob_invalid_contents() {
        let fixture = new_blob_fixture().await;

        let data = vec![0xffu8; (READ_AHEAD_SIZE + BLOCK_SIZE) as usize];
        let hash = fixture.write_blob(&data).await;
        let name = format!("{}", hash);

        {
            // Overwrite the second read-ahead window.  The first window should successfully verify.
            let handle = fixture.get_blob_handle(&name).await;
            let mut transaction =
                handle.new_transaction().await.expect("failed to create transaction");
            let mut buf = handle.allocate_buffer(BLOCK_SIZE as usize);
            buf.as_mut_slice().fill(0);
            handle
                .txn_write(&mut transaction, READ_AHEAD_SIZE, buf.as_ref())
                .await
                .expect("txn_write failed");
            transaction.commit().await.expect("failed to commit transaction");
        }

        {
            let blob =
                open_file_checked(fixture.root(), fio::OpenFlags::RIGHT_READABLE, &name).await;
            assert_matches!(blob.read(MAX_TRANSFER_SIZE).await.expect("FIDL call failed"), Ok(_));
            blob.seek(fio::SeekOrigin::Start, READ_AHEAD_SIZE as i64)
                .await
                .expect("FIDL call failed")
                .map_err(zx::Status::from_raw)
                .expect("seek failed");
            assert_matches!(
                blob.read(MAX_TRANSFER_SIZE)
                    .await
                    .expect("FIDL call failed")
                    .map_err(zx::Status::from_raw),
                Err(zx::Status::IO_DATA_INTEGRITY)
            );
        }

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
        zx::Status::ok(status).unwrap();
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
        zx::Status::ok(status).unwrap();
        let status = fixture
            .root()
            .link(&format!("{}", hash), token.unwrap().into(), "foo")
            .await
            .expect("FIDL failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::NOT_SUPPORTED);

        fixture.close().await;
    }
}
