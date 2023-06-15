// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`FxBlob`] node type used to represent an immutable blob persisted to
//! disk which can be read back.

use {
    crate::fuchsia::{
        directory::FxDirectory,
        errors::map_to_status,
        node::{FxNode, OpenedNode},
        pager::{PagerBackedVmo, TransferBuffers, TRANSFER_BUFFER_MAX_SIZE},
        vmo_data_buffer::VmoDataBuffer,
        volume::info_to_filesystem_info,
        volume::FxVolume,
    },
    anyhow::{anyhow, ensure, Context, Error},
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io::{
        self as fio, FilesystemInfo, NodeAttributeFlags, NodeAttributes, NodeMarker, VmoFlags,
    },
    fuchsia_component::client::connect_to_protocol,
    fuchsia_merkle::{hash_block, MerkleTree},
    fuchsia_zircon::Status,
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::{future::BoxFuture, join},
    fxfs::{
        async_enter,
        errors::FxfsError,
        log::*,
        object_handle::{GetProperties, ObjectHandle, ObjectProperties, ReadObjectHandle},
        object_store::{HandleOwner, StoreObjectHandle},
        round::{round_down, round_up},
    },
    once_cell::sync::{Lazy, OnceCell},
    std::{
        io::Read,
        ops::Range,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    },
    storage_device::buffer,
    vfs::{
        common::rights_to_posix_mode_bits,
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        file::{File, FileOptions, GetVmo, StreamIoConnection, SyncMode},
        path::Path,
        ObjectRequestRef, ProtocolsExt, ToObjectRequest,
    },
};

pub const BLOCK_SIZE: u64 = fuchsia_merkle::BLOCK_SIZE as u64;

pub const READ_AHEAD_SIZE: u64 = 131_072;

// When the top bit of the open count is set, it means the file has been deleted and when the count
// drops to zero, it will be tombstoned.  Once it has dropped to zero, it cannot be opened again
// (assertions will fire).
pub const PURGED: usize = 1 << (usize::BITS - 1);

static VMEX_RESOURCE: OnceCell<zx::Resource> = OnceCell::new();

/// Attempt to initialize the vmex resource routed to this component. Without a vmex, attempts to
/// get the backing memory of a blob with executable rights will fail with NOT_SUPPORTED.
pub async fn init_vmex_resource() -> Result<(), Error> {
    let client = connect_to_protocol::<fidl_fuchsia_kernel::VmexResourceMarker>()?;
    VMEX_RESOURCE.set(client.get().await?).map_err(|_| anyhow!(FxfsError::AlreadyBound))
}

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

    pub fn create_connection_async(
        this: OpenedNode<Self>,
        scope: ExecutionScope,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<BoxFuture<'static, ()>, zx::Status> {
        object_request.create_connection(scope, this.take(), protocols, StreamIoConnection::create)
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
        flags.to_object_request(server_end).spawn(&scope.clone(), move |object_request| {
            Box::pin(async move {
                if !path.is_empty() {
                    return Err(Status::NOT_FILE);
                }
                Self::create_connection_async(OpenedNode::new(self), scope, flags, object_request)
            })
        });
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

    fn open_count_sub_one(self: Arc<Self>) {
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
impl vfs::node::Node for FxBlob {
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

    fn close(self: Arc<Self>) {
        self.open_count_sub_one();
    }
}

/// Implement VFS trait so blobs can be accessed as files.
#[async_trait]
impl File for FxBlob {
    fn executable(&self) -> bool {
        true
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
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

    async fn set_attrs(
        &self,
        _flags: NodeAttributeFlags,
        _attrs: NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::ACCESS_DENIED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
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
                    error = ?e,
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
                            error = ?e,
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

    fn on_zero_children(self: Arc<Self>) {
        self.open_count_sub_one();
    }
}

impl GetVmo for FxBlob {
    fn get_vmo(&self) -> &zx::Vmo {
        self.buffer.vmo()
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
        fidl_fuchsia_io::MAX_TRANSFER_SIZE,
        fuchsia_async as fasync,
        fuchsia_merkle::MerkleTreeBuilder,
        fxfs::{
            object_handle::WriteBytes as _,
            object_store::{
                directory::Directory,
                transaction::{LockKey, TransactionHandler as _},
                DirectWriter, BLOB_MERKLE_ATTRIBUTE_ID,
            },
            round::round_up,
            serialized_types::BlobMetadata,
        },
    };

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
                .create_child_file(&mut transaction, &format!("{}", tree.root()), None)
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
}
