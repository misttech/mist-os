// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`FxUnsealedBlob`] node type used to represent an uncompressed blob
//! in the process of being written/verified to persistent storage.

use {
    crate::fuchsia::{
        directory::FxDirectory,
        errors::map_to_status,
        fxblob::{blob::PURGED, directory::BlobDirectory},
        node::FxNode,
        volume::info_to_filesystem_info,
        volume::FxVolume,
    },
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    delivery_blob::{
        compression::{decode_archive, ChunkInfo, ChunkedDecompressor},
        Type1Blob,
    },
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io::{
        self as fio, FilesystemInfo, MutableNodeAttributes, NodeAttributeFlags, NodeAttributes,
        NodeAttributes2, NodeAttributesQuery, NodeMarker,
    },
    fuchsia_hash::Hash,
    fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
    fuchsia_zircon::{self as zx, HandleBased as _, Status},
    futures::lock::Mutex as AsyncMutex,
    fxfs::{
        errors::FxfsError,
        log::*,
        object_handle::{ObjectHandle, ObjectProperties, WriteObjectHandle},
        object_store::{
            directory::{replace_child_with_object, ObjectDescriptor},
            transaction::{LockKey, Options},
            StoreObjectHandle, Timestamp, BLOB_MERKLE_ATTRIBUTE_ID,
        },
        round::{round_down, round_up},
        serialized_types::BlobMetadata,
    },
    lazy_static::lazy_static,
    std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    vfs::{
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        file::{FidlIoConnection, File, FileIo, FileOptions, SyncMode},
        node::Node,
        path::Path,
        ToObjectRequest,
    },
};

lazy_static! {
    pub static ref RING_BUFFER_SIZE: u64 = 64 * (zx::system_get_page_size() as u64);
}

const PAYLOAD_BUFFER_FLUSH_THRESHOLD: usize = 131_072; /* 128 KiB */

/// Interface for the fuchsia.storage.fxfs.BlobWriter protocol that uses a ring buffer to transfer
/// data using a VMO.
#[async_trait]
pub trait BlobWriterProtocol {
    /// Get the VMO to write to for this blob, and prepare the blob to accept `size` bytes.
    async fn get_vmo(&self, size: u64) -> Result<zx::Vmo, Error>;

    /// Signal that `bytes_written` bytes can be read out of the VMO provided by `get_vmo`.
    async fn bytes_ready(&self, bytes_written: u64) -> Result<(), Error>;
}

/// Represents an RFC-0207 compliant delivery blob that is being written.
/// The blob cannot be read until writes complete and hash is verified.
pub struct FxDeliveryBlob {
    hash: Hash,
    handle: StoreObjectHandle<FxVolume>,
    parent: Arc<BlobDirectory>,
    open_count: AtomicUsize,
    inner: AsyncMutex<Inner>,
}

struct Inner {
    /// Total number of bytes we expect for the delivery blob to be fully written. This is the same
    /// number of bytes passed to truncate. We expect this to be non-zero for a delivery blob.
    delivery_size: Option<u64>,
    /// Write offset with respect to the fuchsia.io protocol.
    delivery_bytes_written: u64,
    /// Vmo used for the blob writer protocol.
    vmo: Option<zx::Vmo>,
    /// Internal buffer of data being written via the write protocols.
    buffer: Vec<u8>,
    header: Option<Type1Blob>,
    tree_builder: MerkleTreeBuilder,
    /// Set to true when we've allocated space for the blob payload on disk.
    allocated_space: bool,
    /// How many bytes from the delivery blob payload have been written to disk so far.
    payload_persisted: u64,
    /// Offset within the delivery blob payload we started writing data to disk.
    payload_offset: u64,
    /// Decompressor used when writing compressed delivery blobs.
    decompressor: Option<ChunkedDecompressor>,
    /// Latched write error.
    write_error: Option<Status>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            delivery_size: None,
            delivery_bytes_written: 0,
            vmo: None,
            buffer: Default::default(),
            header: None,
            tree_builder: Default::default(),
            allocated_space: false,
            payload_persisted: 0,
            payload_offset: 0,
            decompressor: None,
            write_error: None,
        }
    }
}

impl Inner {
    fn header(&self) -> &Type1Blob {
        self.header.as_ref().unwrap()
    }

    fn decompressor(&self) -> &ChunkedDecompressor {
        self.decompressor.as_ref().unwrap()
    }

    fn storage_size(&self) -> usize {
        let header = self.header();
        if header.is_compressed {
            let seek_table = self.decompressor().seek_table();
            if seek_table.is_empty() {
                return 0;
            }
            // TODO(fxbug.dev/127530): If the uncompressed size of the blob is smaller than the
            // filesystem's block size, we should decompress it before persisting it on disk.
            return seek_table.last().unwrap().compressed_range.end;
        }
        // Data is uncompressed, storage size is equal to the payload length.
        header.payload_length
    }

    async fn write_payload(&mut self, handle: &StoreObjectHandle<FxVolume>) -> Result<(), Error> {
        debug_assert!(self.allocated_space);
        let final_write =
            (self.payload_persisted as usize + self.buffer.len()) == self.header().payload_length;
        let block_size = handle.block_size() as usize;
        let flush_threshold = std::cmp::max(block_size, PAYLOAD_BUFFER_FLUSH_THRESHOLD);
        // If we expect more data but haven't met the flush threshold, wait for more.
        if !final_write && self.buffer.len() < flush_threshold {
            return Ok(());
        }
        let len =
            if final_write { self.buffer.len() } else { round_down(self.buffer.len(), block_size) };
        // Move data into transfer buffer, zero pad if required.
        let aligned_len = round_up(len, block_size).ok_or(FxfsError::OutOfRange)?;
        let mut buffer = handle.allocate_buffer(aligned_len);
        buffer.as_mut_slice()[..len].copy_from_slice(&self.buffer[..len]);
        buffer.as_mut_slice()[len..].fill(0);
        self.buffer.drain(..len);
        // Update Merkle tree and overwrite allocated bytes in the object's handle.
        let data = &buffer.as_slice()[..len];
        if let Some(ref mut decompressor) = self.decompressor {
            // Data is compressed, decompress to update Merkle tree.
            decompressor
                .update(data, &mut |chunk_data| self.tree_builder.write(chunk_data))
                .context("Failed to decompress archive")?;
        } else {
            // Data is uncompressed, use payload to update Merkle tree.
            self.tree_builder.write(data);
        }
        // Overwrite allocated bytes in the object's handle.
        debug_assert!(self.payload_persisted >= self.payload_offset);
        handle
            .overwrite(self.payload_persisted - self.payload_offset, buffer.as_mut(), false)
            .await?;
        self.payload_persisted += len as u64;
        Ok(())
    }

    fn generate_metadata(&self, merkle_tree: MerkleTree) -> Result<Option<BlobMetadata>, Error> {
        // We only write metadata if the Merkle tree has multiple levels or the data is compressed.
        let is_compressed = self.header().is_compressed;
        // Special case: handle empty compressed archive.
        if is_compressed && self.decompressor().seek_table().is_empty() {
            return Ok(None);
        }
        if merkle_tree.as_ref().len() > 1 || is_compressed {
            let mut hashes = vec![];
            hashes.reserve(merkle_tree.as_ref()[0].len());
            for hash in &merkle_tree.as_ref()[0] {
                hashes.push(**hash);
            }
            let (uncompressed_size, chunk_size, compressed_offsets) = if is_compressed {
                parse_seek_table(self.decompressor().seek_table())?
            } else {
                (self.header().payload_length as u64, 0u64, vec![])
            };

            Ok(Some(BlobMetadata { hashes, chunk_size, compressed_offsets, uncompressed_size }))
        } else {
            Ok(None)
        }
    }
}

impl FxDeliveryBlob {
    pub(crate) fn new(
        parent: Arc<BlobDirectory>,
        hash: Hash,
        handle: StoreObjectHandle<FxVolume>,
    ) -> Arc<Self> {
        let file = Arc::new(Self {
            hash,
            handle,
            parent,
            open_count: AtomicUsize::new(0),
            inner: Default::default(),
        });
        file
    }

    async fn allocate(&self, size: usize) -> Result<(), Error> {
        let mut transaction = self.handle.new_transaction().await?;
        let size: u64 = size.try_into()?;
        self.handle
            .preallocate_range(
                &mut transaction,
                0..round_up(size, self.handle.block_size()).ok_or(FxfsError::OutOfRange)?,
            )
            .await?;
        self.handle.grow(&mut transaction, 0, size).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn complete(&self, metadata: Option<BlobMetadata>) -> Result<(), Error> {
        self.handle.flush().await?;

        if let Some(metadata) = metadata {
            let mut serialized = vec![];
            bincode::serialize_into(&mut serialized, &metadata)?;
            self.handle.write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &serialized).await?;
        }

        let volume = self.handle.owner();
        let store = self.handle.store();

        let dir = volume
            .cache()
            .get(store.root_directory_object_id())
            .unwrap()
            .into_any()
            .downcast::<BlobDirectory>()
            .expect("Expected blob directory");

        let keys = [LockKey::object(store.store_object_id(), dir.object_id())];
        let mut transaction = store.filesystem().new_transaction(&keys, Options::default()).await?;

        let object_id = self.handle.object_id();
        store.remove_from_graveyard(&mut transaction, object_id);

        let name = format!("{}", self.hash);
        replace_child_with_object(
            &mut transaction,
            Some((object_id, ObjectDescriptor::File)),
            (dir.directory().directory(), &name),
            0,
            Timestamp::now(),
        )
        .await?;

        let parent = self.parent().unwrap();
        transaction.commit_with_callback(|_| parent.did_add(&name)).await?;
        Ok(())
    }
}

impl DirectoryEntry for FxDeliveryBlob {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            if !path.is_empty() {
                return Err(Status::NOT_FILE);
            }
            object_request.spawn_connection(scope, self, flags, FidlIoConnection::create)
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.object_id(), fio::DirentType::File)
    }
}

#[async_trait]
impl FxNode for FxDeliveryBlob {
    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        Some(self.parent.directory().clone())
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
impl Node for FxDeliveryBlob {
    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        Err(Status::BAD_STATE)
    }

    async fn get_attributes(
        &self,
        _requested_attributes: NodeAttributesQuery,
    ) -> Result<NodeAttributes2, Status> {
        Err(Status::BAD_STATE)
    }
}

#[async_trait]
impl File for FxDeliveryBlob {
    fn writable(&self) -> bool {
        true
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, length: u64) -> Result<(), Status> {
        let mut inner = self.inner.lock().await;
        if let Some(previous_size) = inner.delivery_size {
            error!(previous_size, "truncate already called on blob");
            return Err(Status::BAD_STATE);
        }
        if length < Type1Blob::HEADER.header_length as u64 {
            error!("specified size is too small for delivery blob");
            return Err(Status::INVALID_ARGS);
        }
        inner.delivery_size = Some(length);
        Ok(())
    }

    async fn get_size(&self) -> Result<u64, Status> {
        Err(Status::BAD_STATE)
    }

    async fn set_attrs(
        &self,
        _flags: NodeAttributeFlags,
        _attrs: NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::BAD_STATE)
    }

    async fn update_attributes(&self, _attributes: MutableNodeAttributes) -> Result<(), Status> {
        Err(Status::BAD_STATE)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        // TODO(fxbug.dev/122125): Implement sync.
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
        return Err(Status::BAD_STATE);
    }
}

#[async_trait]
impl BlobWriterProtocol for FxDeliveryBlob {
    async fn get_vmo(&self, size: u64) -> Result<zx::Vmo, Error> {
        self.truncate(size)
            .await
            .with_context(|| format!("Failed to truncate blob {} to size {}", self.hash, size))?;
        let mut inner = self.inner.lock().await;
        if inner.vmo.is_some() {
            return Err(FxfsError::AlreadyExists)
                .with_context(|| format!("VMO was already created for blob {}", self.hash));
        }
        let vmo = zx::Vmo::create(*RING_BUFFER_SIZE).with_context(|| {
            format!("Failed to create VMO of size {} for writing", *RING_BUFFER_SIZE)
        })?;
        let vmo_dup = vmo
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .context("Failed to duplicate VMO handle")?;
        inner.vmo = Some(vmo);
        Ok(vmo_dup)
    }

    async fn bytes_ready(&self, bytes_written: u64) -> Result<(), Error> {
        // TODO(https://fxbug.dev/126617): Remove extra copy.
        // TODO(https://fxbug.dev/127530): Share ring buffer code with FxUnsealedBlob.
        if bytes_written > *RING_BUFFER_SIZE {
            return Err(FxfsError::OutOfRange).with_context(|| {
                format!(
                    "bytes_written ({}) exceeds size of ring buffer ({})",
                    bytes_written, *RING_BUFFER_SIZE
                )
            });
        }
        let mut buf = vec![0; bytes_written as usize];
        let write_offset;
        {
            let inner = self.inner.lock().await;
            let Some(ref vmo) = inner.vmo else {
                return Err(anyhow!("get_vmo was not called before attempting to write bytes"));
            };
            write_offset = inner.delivery_bytes_written;
            let vmo_offset = write_offset % *RING_BUFFER_SIZE;
            if vmo_offset + bytes_written > *RING_BUFFER_SIZE {
                let split = (*RING_BUFFER_SIZE - vmo_offset) as usize;
                vmo.read(&mut buf[0..split], vmo_offset).context("failed to read from VMO")?;
                vmo.read(&mut buf[split..], 0).context("failed to read from VMO")?;
            } else {
                vmo.read(&mut buf, vmo_offset).context("failed to read from VMO")?;
            }
        }
        self.write_at(write_offset, &buf)
            .await
            .with_context(|| {
                format!(
            "failed to to write to blob {} (bytes_written = {}, write_offset = {}, buf.len() = {})",
            self.hash, bytes_written, write_offset, buf.len()
        )
            })
            .map_err(|err| {
                eprintln!("{:?}", err);
                err
            })?;
        Ok(())
    }
}

#[async_trait]
impl FileIo for FxDeliveryBlob {
    async fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<u64, Status> {
        return Err(Status::BAD_STATE);
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
        let mut inner = self.inner.lock().await;
        if let Some(e) = inner.write_error {
            return Err(e); // Error was latched from previous write call.
        }
        if let None = inner.delivery_size {
            error!("truncate must be called before writing blobs");
            return Err(Status::BAD_STATE);
        }
        if offset != inner.delivery_bytes_written {
            error!("only append is supported when writing blobs");
            return Err(Status::NOT_SUPPORTED);
        }
        let content_len = content.len() as u64;
        if (inner.delivery_bytes_written + content_len) > inner.delivery_size.unwrap() {
            error!("write exceeds truncated blob length");
            return Err(Status::BUFFER_TOO_SMALL);
        }
        if let Some(ref header) = inner.header {
            if (inner.payload_persisted + content_len) > header.payload_length as u64 {
                error!("write exceeds delivery header payload length");
                return Err(Status::BUFFER_TOO_SMALL);
            }
        }

        // Any errors occurring below will be latched as the blob will be in an unrecoverable state.
        // Writers will need to close and try to rewrite the blob.
        async {
            inner.buffer.extend_from_slice(content);
            inner.delivery_bytes_written += content_len;

            // Decode delivery blob header.
            if inner.header.is_none() {
                let Some((header, payload)) = Type1Blob::parse(&inner.buffer)
                    .context("Failed to decode delivery blob header")?
                else {
                    return Ok(()); // Not enough data to decode header yet.
                };
                inner.buffer = Vec::from(payload);
                inner.header = Some(header);
            }

            // If blob is compressed, decode chunked archive header & initialize decompressor.
            if inner.header().is_compressed && inner.decompressor.is_none() {
                let prev_buff_len = inner.buffer.len();
                let archive_length = inner.header().payload_length;
                let Some((seek_table, chunk_data)) = decode_archive(&inner.buffer, archive_length)
                    .context("Failed to decode chunked archive")?
                else {
                    return Ok(());  // Not enough data to decode archive header/seek table.
                };
                // We store the seek table out-of-line with the data, so we don't persist that
                // part of the payload directly.
                inner.buffer = Vec::from(chunk_data);
                inner.payload_offset = (prev_buff_len - inner.buffer.len()) as u64;
                inner.payload_persisted = inner.payload_offset;
                inner.decompressor = Some(
                    ChunkedDecompressor::new(seek_table)
                        .context("Failed to create decompressor")?,
                );
            }

            // Allocate storage space on the filesystem to write the blob payload.
            if !inner.allocated_space {
                let amount = inner.storage_size();
                self.allocate(amount)
                    .await
                    .with_context(|| format!("Failed to allocate {} bytes", amount))?;
                inner.allocated_space = true;
            }

            // Write payload to disk and update Merkle tree.
            if !inner.buffer.is_empty() {
                inner.write_payload(&self.handle).await?;
            }

            let blob_complete = inner.delivery_bytes_written == inner.delivery_size.unwrap();
            if blob_complete {
                debug_assert!(inner.payload_persisted == inner.header().payload_length as u64);
                // Finish building Merkle tree and verify the hash matches the filename.
                let merkle_tree = std::mem::take(&mut inner.tree_builder).finish();
                if merkle_tree.root() != self.hash {
                    return Err(FxfsError::IntegrityError).with_context(|| {
                        format!(
                            "Calculated Merkle root ({}) does not match blob name ({})",
                            merkle_tree.root(),
                            self.hash
                        )
                    });
                }
                // Calculate metadata and promote verified blob into a directory entry.
                let metadata = inner.generate_metadata(merkle_tree)?;
                self.complete(metadata).await?;
            }
            Ok(())
        }
        .await
        .map_err(|e| {
            // Log and latch any errors occurring above.
            error!("Failed to write {hash}: {error:?}", hash = self.hash, error = e);
            let status = map_to_status(e);
            inner.write_error = Some(status);
            Err(status)
        })?;
        Ok(content_len)
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        unimplemented!("use write_at instead")
    }
}

fn parse_seek_table(
    seek_table: &Vec<ChunkInfo>,
) -> Result<(/*uncompressed_size*/ u64, /*chunk_size*/ u64, /*compressed_offsets*/ Vec<u64>), Error>
{
    let uncompressed_size = seek_table.last().unwrap().decompressed_range.end;
    let chunk_size = seek_table.first().unwrap().decompressed_range.len();
    // fxblob only supports archives with equally sized chunks.
    if seek_table.len() > 1
        && seek_table[1..seek_table.len() - 1]
            .iter()
            .any(|entry| entry.decompressed_range.len() != chunk_size)
    {
        return Err(FxfsError::NotSupported)
            .context("Unsupported archive: compressed length of each chunk must be the same");
    }
    let compressed_offsets = seek_table
        .iter()
        .map(|entry| TryInto::<u64>::try_into(entry.compressed_range.start))
        .collect::<Result<Vec<_>, _>>()?;

    // TODO(fxbug.dev/127530): The pager assumes chunk_size alignment is at least the size of a
    // Merkle tree block. We should allow arbitrary chunk sizes. For now, we reject archives with
    // multiple chunks that don't meet this requirement (since we control archive generation), and
    // round up the chunk size for archives with a single chunk, as we won't read past the file end.
    let chunk_size: u64 = chunk_size.try_into()?;
    let alignment: u64 = fuchsia_merkle::BLOCK_SIZE.try_into()?;
    let aligned_chunk_size = if seek_table.len() > 1 {
        if chunk_size < alignment || chunk_size % alignment != 0 {
            return Err(FxfsError::NotSupported)
                .context("Unsupported archive: chunk size must be multiple of Merkle tree block");
        }
        chunk_size
    } else {
        round_up(chunk_size, alignment).unwrap()
    };

    Ok((uncompressed_size.try_into()?, aligned_chunk_size, compressed_offsets))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::fuchsia::{
            fxblob::testing::{new_blob_fixture, BlobFixture},
            testing::{open_file, open_file_checked, TestFixture},
        },
        core::ops::Range,
        delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob},
        fidl_fuchsia_fxfs,
        fidl_fuchsia_io::{
            self as fio, NodeAttributeFlags, NodeAttributes, OpenFlags, VmoFlags, MAX_TRANSFER_SIZE,
        },
        fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
        fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
        fuchsia_zircon::Status,
        rand::{thread_rng, Rng},
    };

    async fn resize_and_write(
        fixture: &TestFixture,
        path: &str,
        payload: &[u8],
    ) -> Result<(), Status> {
        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            path,
        )
        .await;
        blob.resize(payload.len() as u64).await.expect("fidl error").map_err(Status::from_raw)?;
        for chunk in payload.chunks(MAX_TRANSFER_SIZE as usize) {
            assert_eq!(
                blob.write(&chunk).await.expect("fidl error").map_err(Status::from_raw)?,
                chunk.len() as u64
            );
        }
        Ok(())
    }

    fn generate_list_of_writes(compressed_data_len: u64) -> Vec<Range<u64>> {
        let mut list_of_writes = vec![];
        let mut bytes_left_to_write = compressed_data_len;
        let mut write_offset = 0;
        let half_ring_buffer = *RING_BUFFER_SIZE / 2;
        while bytes_left_to_write > half_ring_buffer {
            list_of_writes.push(write_offset..write_offset + half_ring_buffer);
            write_offset += half_ring_buffer;
            bytes_left_to_write -= half_ring_buffer;
        }
        if bytes_left_to_write > 0 {
            list_of_writes.push(write_offset..write_offset + bytes_left_to_write);
        }
        list_of_writes
    }

    #[fasync::run(10, test)]
    async fn test_write_uncompressed_empty() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();
        let payload = Type1Blob::generate(&[], CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert!(fixture.read_blob(&format!("{}", hash)).await.is_empty());
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_uncompressed_small() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 3_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_uncompressed_large() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 3_000_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_uncompressed_block_aligned() {
        let fixture = new_blob_fixture().await;
        const NUM_BLOCKS: usize = 4;
        let amount = NUM_BLOCKS * fixture.fs().root_parent_store().block_size() as usize;
        let data = vec![3; amount];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_uncompressed_chunks() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 50_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(hash),
        )
        .await;
        blob.resize(payload.len() as u64)
            .await
            .expect("fidl error")
            .map_err(Status::from_raw)
            .unwrap();
        // Write in batches of 4 bytes. This ensures we handle incomplete headers gracefully.
        for chunk in payload.chunks(4) {
            assert_eq!(
                blob.write(&chunk).await.expect("fidl error").map_err(Status::from_raw).unwrap(),
                chunk.len() as u64
            );
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_compressed_empty() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();
        let payload = Type1Blob::generate(&[], CompressionMode::Always);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert!(fixture.read_blob(&format!("{}", hash)).await.is_empty());
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_compressed_small() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 3_000];
        let block_size = fixture.fs().root_parent_store().block_size() as usize;
        assert!(data.len() <= block_size);
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Always);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_compressed_small_data_large() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 500_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Always);
        // `data` is highly compressible, so should fit within a single block.
        let block_size = fixture.fs().root_parent_store().block_size() as usize;
        assert!(payload.len() <= block_size);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_compressed_large() {
        let fixture = new_blob_fixture().await;
        let data: Vec<u8> = {
            const DATA_LEN: usize = 3_000_000;
            let range = rand::distributions::Uniform::<u8>::new_inclusive(0, 255);
            rand::thread_rng().sample_iter(&range).take(DATA_LEN).collect()
        };
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Always);
        // `data` is random, so even when compressed it should span multiple blocks.
        let block_size = fixture.fs().root_parent_store().block_size() as usize;
        assert!(payload.len() > block_size);
        resize_and_write(&fixture, &delivery_blob_path(hash), &payload).await.unwrap();
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_write_compressed_chunks() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 50_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root();
        let payload = Type1Blob::generate(&data, CompressionMode::Always);
        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(hash),
        )
        .await;
        blob.resize(payload.len() as u64)
            .await
            .expect("fidl error")
            .map_err(Status::from_raw)
            .unwrap();
        // Write in batches of 4 bytes. This ensures we handle incomplete headers gracefully.
        for chunk in payload.chunks(4) {
            assert_eq!(
                blob.write(&chunk).await.expect("fidl error").map_err(Status::from_raw).unwrap(),
                chunk.len() as u64
            );
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    /// A blob should fail to write if the calculated Merkle root doesn't match the filename.
    #[fasync::run(10, test)]
    async fn test_reject_bad_hash() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 1_000];

        let correct_hash = MerkleTree::from_reader(data.as_slice()).unwrap().root().to_string();
        let incorrect_hash =
            MerkleTree::from_reader(&data[..data.len() - 1]).unwrap().root().to_string();
        assert_ne!(correct_hash, incorrect_hash);

        let payload = Type1Blob::generate(&data, CompressionMode::Never);
        let write_error =
            resize_and_write(&fixture, &delivery_blob_path(&incorrect_hash), &payload)
                .await
                .unwrap_err();
        assert_eq!(write_error, Status::IO_DATA_INTEGRITY);

        // Ensure we can't open either hash.
        assert!(open_file(fixture.root(), OpenFlags::RIGHT_READABLE, &correct_hash).await.is_err());
        assert!(open_file(fixture.root(), OpenFlags::RIGHT_READABLE, &incorrect_hash)
            .await
            .is_err());

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_reject_write_too_many_bytes() {
        let fixture = new_blob_fixture().await;
        let data = vec![3; 1_000];
        let hash = MerkleTree::from_reader(data.as_slice()).unwrap().root().to_string();
        // Append some more bytes onto the end of payload.
        let mut payload = Type1Blob::generate(&data, CompressionMode::Never);
        let original_payload_len = payload.len() as u64;
        payload.extend_from_slice(&[1, 2, 3, 4]);

        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(hash),
        )
        .await;
        blob.resize(original_payload_len)
            .await
            .expect("fidl error")
            .map_err(Status::from_raw)
            .unwrap();
        assert!(payload.len() < MAX_TRANSFER_SIZE as usize);
        assert_eq!(
            Status::BUFFER_TOO_SMALL,
            blob.write(&payload).await.expect("fidl error").map_err(Status::from_raw).unwrap_err()
        );

        fixture.close().await;
    }

    /// We should fail early when truncating a delivery blob if the size is too small.
    #[fasync::run(10, test)]
    async fn test_reject_too_small() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();
        // The smallest possible delivery blob should be an uncompressed null/empty Type 1 blob.
        let payload = Type1Blob::generate(&[], CompressionMode::Never);

        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(&hash),
        )
        .await;

        let resize_error = blob
            .resize((payload.len() - 1) as u64)
            .await
            .expect("fidl error")
            .map_err(Status::from_raw)
            .unwrap_err();
        assert_eq!(resize_error, Status::INVALID_ARGS);

        fixture.close().await;
    }

    /// Ensure we cannot call certain fuchsia.io methods while a blob is being written.
    #[fasync::run(10, test)]
    async fn test_disallowed_io_methods() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();

        let blob = open_file_checked(
            fixture.root(),
            OpenFlags::CREATE | OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE,
            &delivery_blob_path(hash),
        )
        .await;

        assert_eq!(
            Status::BAD_STATE,
            Status::from_raw(
                blob.get_backing_memory(VmoFlags::READ).await.expect("fidl error").unwrap_err()
            )
        );

        assert_eq!(
            Status::BAD_STATE,
            Status::from_raw(blob.get_attr().await.expect("fidl error").0)
        );

        assert_eq!(
            Status::BAD_STATE,
            Status::from_raw(
                blob.set_attr(
                    NodeAttributeFlags::CREATION_TIME,
                    &NodeAttributes {
                        mode: 0,
                        id: 0,
                        content_size: 0,
                        storage_size: 0,
                        link_count: 0,
                        creation_time: 0,
                        modification_time: 0,
                    }
                )
                .await
                .expect("fidl error")
            )
        );

        fixture.close().await;
    }

    /// Tests for the new write API.
    #[fasync::run(10, test)]
    async fn test_new_write_empty_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);

        {
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");
            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true, true)
                .await
                .expect("failed to create_and_serve the blob volume");
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

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let vmo = writer
                .get_vmo(compressed_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            let vmo_size = vmo.get_size().expect("failed to get vmo size");
            let list_of_writes = generate_list_of_writes(compressed_data.len() as u64);
            let mut write_offset = 0;
            for range in list_of_writes {
                let len = range.end - range.start;
                vmo.write(
                    &compressed_data[range.start as usize..range.end as usize],
                    write_offset % vmo_size,
                )
                .expect("failed to write to vmo");
                let _ = writer
                    .bytes_ready(len)
                    .await
                    .expect("transport error on bytes_ready")
                    .expect("failed to write data to vmo");
                write_offset += len;
            }
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_new_write_small_blob_no_wrap() {
        let fixture = new_blob_fixture().await;

        let mut data = vec![1; 196608];
        thread_rng().fill(&mut data[..]);

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);

        {
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true, true)
                .await
                .expect("failed to create_and_serve the blob volume");
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

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let vmo = writer
                .get_vmo(compressed_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            let vmo_size = vmo.get_size().expect("failed to get vmo size");
            let list_of_writes = generate_list_of_writes(compressed_data.len() as u64);
            let mut write_offset = 0;
            for range in list_of_writes {
                let len = range.end - range.start;
                vmo.write(
                    &compressed_data[range.start as usize..range.end as usize],
                    write_offset % vmo_size,
                )
                .expect("failed to write to vmo");
                let _ = writer
                    .bytes_ready(len)
                    .await
                    .expect("transport error on bytes_ready")
                    .expect("failed to write data to vmo");
                write_offset += len;
            }
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_new_write_large_blob_wraps() {
        let fixture = new_blob_fixture().await;

        let mut data = vec![1; 1024921];
        thread_rng().fill(&mut data[..]);

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);
        assert!(compressed_data.len() as u64 > *RING_BUFFER_SIZE);

        {
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true, true)
                .await
                .expect("failed to create_and_serve the blob volume");
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

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let vmo = writer
                .get_vmo(compressed_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            let vmo_size = vmo.get_size().expect("failed to get vmo size");

            let list_of_writes = generate_list_of_writes(compressed_data.len() as u64);
            let mut write_offset = 0;
            for range in list_of_writes {
                let len = range.end - range.start;
                vmo.write(
                    &compressed_data[range.start as usize..range.end as usize],
                    write_offset % vmo_size,
                )
                .expect("failed to write to vmo");
                let _ = writer
                    .bytes_ready(len)
                    .await
                    .expect("transport error on bytes_ready")
                    .expect("failed to write data to vmo");
                write_offset += len;
            }
        }
        assert_eq!(fixture.read_blob(&format!("{}", hash)).await, data);
        fixture.close().await;
    }
}
