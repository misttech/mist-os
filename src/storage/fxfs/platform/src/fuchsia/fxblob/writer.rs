// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`FxUnsealedBlob`] node type used to represent an uncompressed blob
//! in the process of being written/verified to persistent storage.

use {
    crate::fuchsia::{
        directory::FxDirectory, errors::map_to_status, fxblob::directory::BlobDirectory,
        node::FxNode, volume::FxVolume,
    },
    anyhow::{Context as _, Error},
    delivery_blob::{
        compression::{decode_archive, ChunkInfo, ChunkedDecompressor},
        Type1Blob,
    },
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_fxfs::{BlobWriterMarker, BlobWriterRequest},
    fuchsia_hash::Hash,
    fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
    fuchsia_zircon::{self as zx, HandleBased as _, Status},
    futures::{lock::Mutex as AsyncMutex, try_join, TryStreamExt},
    fxfs::{
        errors::FxfsError,
        object_handle::{ObjectHandle, WriteObjectHandle},
        object_store::{
            directory::{replace_child_with_object, ReplacedChild},
            DataObjectHandle, ObjectDescriptor, Timestamp, BLOB_MERKLE_ATTRIBUTE_ID,
        },
        round::{round_down, round_up},
        serialized_types::BlobMetadata,
    },
    lazy_static::lazy_static,
    std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

lazy_static! {
    pub static ref RING_BUFFER_SIZE: u64 = 64 * (zx::system_get_page_size() as u64);
}

const PAYLOAD_BUFFER_FLUSH_THRESHOLD: usize = 131_072; /* 128 KiB */

/// Represents an RFC-0207 compliant delivery blob that is being written.
/// The blob cannot be read until writes complete and hash is verified.
pub struct FxDeliveryBlob {
    hash: Hash,
    handle: DataObjectHandle<FxVolume>,
    parent: Arc<BlobDirectory>,
    open_count: AtomicUsize,
    is_completed: AtomicBool,
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
            // TODO(https://fxbug.dev/42078146): If the uncompressed size of the blob is smaller than the
            // filesystem's block size, we should decompress it before persisting it on disk.
            return seek_table.last().unwrap().compressed_range.end;
        }
        // Data is uncompressed, storage size is equal to the payload length.
        header.payload_length
    }

    async fn write_payload(&mut self, handle: &DataObjectHandle<FxVolume>) -> Result<(), Error> {
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
        // Update Merkle tree.
        let data = &self.buffer.as_slice()[..len];
        let update_merkle_tree_fut = async {
            if let Some(ref mut decompressor) = self.decompressor {
                // Data is compressed, decompress to update Merkle tree.
                decompressor
                    .update(data, &mut |chunk_data| self.tree_builder.write(chunk_data))
                    .context("Failed to decompress archive")?;
            } else {
                // Data is uncompressed, use payload to update Merkle tree.
                self.tree_builder.write(data);
            }
            Ok::<(), Error>(())
        };

        debug_assert!(self.payload_persisted >= self.payload_offset);

        // Copy data into transfer buffer, zero pad if required.
        let aligned_len = round_up(len, block_size).ok_or(FxfsError::OutOfRange)?;
        let mut buffer = handle.allocate_buffer(aligned_len).await;
        buffer.as_mut_slice()[..len].copy_from_slice(&self.buffer[..len]);
        buffer.as_mut_slice()[len..].fill(0);

        // Overwrite allocated bytes in the object's handle.
        let overwrite_fut =
            handle.overwrite(self.payload_persisted - self.payload_offset, buffer.as_mut(), false);
        // NOTE: `overwrite_fut` needs to be polled first to initiate the asynchronous write to the
        // block device which will then run in parallel with the synchronous
        // `update_merkle_tree_fut`.
        try_join!(overwrite_fut, update_merkle_tree_fut)?;
        self.buffer.drain(..len);
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
        handle: DataObjectHandle<FxVolume>,
    ) -> Arc<Self> {
        let file = Arc::new(Self {
            hash,
            handle,
            parent,
            open_count: AtomicUsize::new(0),
            is_completed: AtomicBool::new(false),
            inner: Default::default(),
        });
        file
    }

    async fn allocate(&self, size: usize) -> Result<(), Error> {
        let size = size as u64;
        let mut range = 0..round_up(size, self.handle.block_size()).ok_or(FxfsError::OutOfRange)?;
        let mut first_time = true;
        while range.start < range.end {
            let mut transaction = self.handle.new_transaction().await?;
            if first_time {
                self.handle.grow(&mut transaction, 0, size).await?;
                first_time = false;
            }
            self.handle.preallocate_range(&mut transaction, &mut range).await?;
            transaction.commit().await?;
        }
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

        let name = format!("{}", self.hash);
        let mut transaction = dir
            .directory()
            .directory()
            .acquire_context_for_replace(None, &name, false)
            .await?
            .transaction;

        let object_id = self.handle.object_id();
        store.remove_from_graveyard(&mut transaction, object_id);

        match replace_child_with_object(
            &mut transaction,
            Some((object_id, ObjectDescriptor::File)),
            (dir.directory().directory(), &name),
            0,
            Timestamp::now(),
        )
        .await?
        {
            ReplacedChild::None => {}
            _ => {
                return Err(FxfsError::AlreadyExists)
                    .with_context(|| format!("Blob {} already exists", self.hash));
            }
        }

        let parent = self.parent().unwrap();
        transaction
            .commit_with_callback(|_| {
                self.is_completed.store(true, Ordering::Relaxed);
                // This can't actually add the node to the cache, because it hasn't been created
                // ever at this point. Passes in None for now as a result.
                parent.did_add(&name, None);
            })
            .await?;
        Ok(())
    }
}

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

    fn open_count_add_one(&self) {
        self.open_count.fetch_add(1, Ordering::Relaxed);
    }

    fn open_count_sub_one(self: Arc<Self>) {
        let old = self.open_count.fetch_sub(1, Ordering::Relaxed);
        assert!(old > 0);
        let is_completed = self.is_completed.load(Ordering::Relaxed);
        if old == 1 && !is_completed {
            let store = self.handle.store();
            store
                .filesystem()
                .graveyard()
                .queue_tombstone_object(store.store_object_id(), self.object_id());
        }
    }

    fn object_descriptor(&self) -> ObjectDescriptor {
        ObjectDescriptor::File
    }
}

impl FxDeliveryBlob {
    async fn truncate(&self, length: u64) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        if inner.delivery_size.is_some() {
            return Err(Status::BAD_STATE).context("Blob was already truncated.");
        }
        if length < Type1Blob::HEADER.header_length as u64 {
            return Err(Status::INVALID_ARGS).context("Invalid size (too small).");
        }
        inner.delivery_size = Some(length);
        Ok(())
    }

    /// Appends |content| to this delivery blob.
    ///
    /// *WARNING*: If this function fails, the blob will remain in an invalid state. Errors should
    /// be latched by the caller instead of calling this function again. The blob can be closed and
    /// re-opened to attempt writing again.
    async fn append(&self, content: &[u8], inner: &mut Inner) -> Result<u64, Error> {
        let delivery_size = inner
            .delivery_size
            .ok_or(Status::BAD_STATE)
            .context("Must truncate blob before writing.")?;
        let content_len = content.len() as u64;
        if (inner.delivery_bytes_written + content_len) > delivery_size {
            return Err(Status::BUFFER_TOO_SMALL).with_context(|| {
                format!(
                    "Wrote more bytes than truncated size (truncated = {}, written = {}).",
                    delivery_size,
                    inner.delivery_bytes_written + content_len
                )
            });
        }
        async {
            inner.buffer.extend_from_slice(content);
            inner.delivery_bytes_written += content_len;

            // Decode delivery blob header.
            if inner.header.is_none() {
                let Some((header, payload)) = Type1Blob::parse(&inner.buffer)
                    .context("Failed to decode delivery blob header.")?
                else {
                    return Ok(()); // Not enough data to decode header yet.
                };
                let expected_size = header.header.header_length as usize + header.payload_length;
                if expected_size != delivery_size as usize {
                    return Err(FxfsError::IntegrityError).with_context(|| {
                        format!(
                            "Truncated size ({}) does not match size from blob header ({})!",
                            delivery_size, expected_size
                        )
                    });
                }
                inner.buffer = Vec::from(payload);
                inner.header = Some(header);
            }

            // If blob is compressed, decode chunked archive header & initialize decompressor.
            if inner.header().is_compressed && inner.decompressor.is_none() {
                let prev_buff_len = inner.buffer.len();
                let archive_length = inner.header().payload_length;
                let Some((seek_table, chunk_data)) = decode_archive(&inner.buffer, archive_length)
                    .context("Failed to decode archive header")?
                else {
                    return Ok(()); // Not enough data to decode archive header/seek table.
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
        .await?;
        Ok(content_len)
    }

    pub async fn get_vmo(&self, size: u64) -> Result<zx::Vmo, Error> {
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

    pub async fn bytes_ready(&self, bytes_written: u64) -> Result<(), Error> {
        // TODO(https://fxbug.dev/42077275): Remove extra copy.
        if bytes_written > *RING_BUFFER_SIZE {
            return Err(FxfsError::OutOfRange).with_context(|| {
                format!(
                    "bytes_written ({}) exceeds size of ring buffer ({})",
                    bytes_written, *RING_BUFFER_SIZE
                )
            });
        }
        let mut buf = vec![0; bytes_written as usize];
        let mut inner = self.inner.lock().await;
        let write_offset = inner.delivery_bytes_written;
        {
            let Some(ref vmo) = inner.vmo else {
                return Err(Status::BAD_STATE)
                    .context("BlobWriter.GetVmo must be called before BlobWriter.BytesReady.");
            };
            let vmo_offset = write_offset % *RING_BUFFER_SIZE;
            if vmo_offset + bytes_written > *RING_BUFFER_SIZE {
                let split = (*RING_BUFFER_SIZE - vmo_offset) as usize;
                vmo.read(&mut buf[0..split], vmo_offset).context("failed to read from VMO")?;
                vmo.read(&mut buf[split..], 0).context("failed to read from VMO")?;
            } else {
                vmo.read(&mut buf, vmo_offset).context("failed to read from VMO")?;
            }
        }
        self.append(&buf, &mut inner).await.with_context(|| {
            format!(
                "failed to write blob {} (bytes_written = {}, offset = {}, delivery_size = {})",
                self.hash,
                bytes_written,
                write_offset,
                inner.delivery_size.unwrap_or_default()
            )
        })?;
        Ok(())
    }

    pub async fn handle_requests(
        &self,
        server_end: ServerEnd<BlobWriterMarker>,
    ) -> Result<(), Error> {
        let mut latched_error = None;
        let mut stream = server_end.into_stream()?;
        while let Some(request) = stream.try_next().await? {
            match request {
                BlobWriterRequest::GetVmo { size, responder } => {
                    let res = match self.get_vmo(size).await {
                        Ok(vmo) => Ok(vmo),
                        Err(e) => {
                            tracing::error!("BlobWriter.GetVmo error: {:?}", e);
                            Err(map_to_status(e).into_raw())
                        }
                    };
                    responder.send(res).unwrap_or_else(|e| {
                        tracing::error!("Error sending BlobWriter.GetVmo response: {:?}", e);
                    });
                }
                BlobWriterRequest::BytesReady { bytes_written, responder } => {
                    let res = if let Some(status) = &latched_error {
                        Err(*status)
                    } else {
                        match self.bytes_ready(bytes_written).await {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                tracing::error!("BlobWriter.BytesReady error: {:?}", e);
                                let status = map_to_status(e).into_raw();
                                latched_error = Some(status);
                                Err(status)
                            }
                        }
                    };
                    responder.send(res).unwrap_or_else(|e| {
                        tracing::error!("Error sending BlobWriter.BytesReady response: {:?}", e);
                    });
                }
            }
        }
        Ok(())
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

    // TODO(https://fxbug.dev/42078146): The pager assumes chunk_size alignment is at least the size of a
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
        crate::fuchsia::fxblob::testing::{new_blob_fixture, BlobFixture},
        core::ops::Range,
        delivery_blob::CompressionMode,
        fidl_fuchsia_fxfs::CreateBlobError,
        fidl_fuchsia_io::UnlinkOptions,
        fuchsia_async as fasync,
        rand::{thread_rng, Rng},
    };

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
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
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
        assert_eq!(fixture.read_blob(hash).await, data);
        fixture.close().await;
    }

    /// We should fail early when truncating a delivery blob if the size is too small.
    #[fasync::run(10, test)]
    async fn test_reject_too_small() {
        let fixture = new_blob_fixture().await;
        let hash = MerkleTreeBuilder::new().finish().root();
        // The smallest possible delivery blob should be an uncompressed null/empty Type 1 blob.
        let delivery_data = Type1Blob::generate(&[], CompressionMode::Never);

        {
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
            assert_eq!(
                writer
                    .get_vmo(delivery_data.len() as u64 - 1)
                    .await
                    .expect("transport error on get_vmo")
                    .map_err(Status::from_raw)
                    .expect_err("get_vmo unexpectedly succeeded"),
                zx::Status::INVALID_ARGS
            );
        }
        fixture.close().await;
    }

    /// A blob should fail to write if the calculated Merkle root doesn't match the filename.
    #[fasync::run(10, test)]
    async fn test_reject_bad_hash() {
        let fixture = new_blob_fixture().await;

        let data = vec![3; 1_000];
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data[..data.len() - 1]);
        let incorrect_hash = builder.finish().root();
        let delivery_data = Type1Blob::generate(&data, CompressionMode::Never);

        {
            let writer = fixture
                .create_blob(&incorrect_hash.into(), false)
                .await
                .expect("failed to create blob");
            let vmo = writer
                .get_vmo(delivery_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");
            vmo.write(&delivery_data, 0).expect("failed to write to vmo");

            assert_eq!(
                writer
                    .bytes_ready(delivery_data.len() as u64)
                    .await
                    .expect("transport error on bytes_ready")
                    .map_err(Status::from_raw)
                    .expect_err("write unexpectedly succeeded"),
                zx::Status::IO_DATA_INTEGRITY
            );
        }
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_new_rewrite_fails() {
        let fixture = new_blob_fixture().await;

        let mut data = vec![1; 196608];
        thread_rng().fill(&mut data[..]);

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);

        {
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
            let vmo = writer
                .get_vmo(compressed_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            let writer_2 =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
            let vmo_2 = writer_2
                .get_vmo(compressed_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            let vmo_size = vmo.get_size().expect("failed to get vmo size");
            let list_of_writes = generate_list_of_writes(compressed_data.len() as u64);
            let mut write_offset = 0;
            for range in &list_of_writes {
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

            let mut count = 0;
            write_offset = 0;
            for range in &list_of_writes {
                let len = range.end - range.start;
                vmo_2
                    .write(
                        &compressed_data[range.start as usize..range.end as usize],
                        write_offset % vmo_size,
                    )
                    .expect("failed to write to vmo");
                if count == list_of_writes.len() - 1 {
                    assert_eq!(
                        writer_2
                            .bytes_ready(len)
                            .await
                            .expect("transport error on bytes_ready")
                            .map_err(Status::from_raw)
                            .expect_err("write unexpectedly succeeded"),
                        zx::Status::ALREADY_EXISTS
                    );
                } else {
                    let _ = writer_2
                        .bytes_ready(len)
                        .await
                        .expect("transport error on bytes_ready")
                        .expect("failed to write data to vmo");
                }
                write_offset += len;
                count += 1;
            }
        }
        assert_eq!(fixture.read_blob(hash).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_new_tombstone_dropped_incomplete_delivery_blobs() {
        let fixture = new_blob_fixture().await;
        for _ in 0..3 {
            // `data` is half the size of the test device. If we don't tombstone, we will get
            // an OUT_OF_SPACE error on the second write.
            let data = vec![1; 4194304];

            let mut builder = MerkleTreeBuilder::new();
            builder.write(&data);
            let hash = builder.finish().root();
            let compressed_data = Type1Blob::generate(&data, CompressionMode::Never);

            {
                let writer =
                    fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
                let vmo = writer
                    .get_vmo(compressed_data.len() as u64)
                    .await
                    .expect("transport error on get_vmo")
                    .expect("failed to get vmo");

                let vmo_size = vmo.get_size().expect("failed to get vmo size");
                // Write all but the last byte to avoid completing the blob.
                let list_of_writes = generate_list_of_writes((compressed_data.len() as u64) - 1);
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
            fixture.fs().graveyard().flush().await;
        }

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
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
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
        assert_eq!(fixture.read_blob(hash).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_new_write_blob_already_exists() {
        let fixture = new_blob_fixture().await;

        let data = vec![1; 65536];

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let delivery_data = Type1Blob::generate(&data, CompressionMode::Never);

        {
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
            let vmo = writer
                .get_vmo(delivery_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");

            vmo.write(&delivery_data, 0).expect("failed to write to vmo");
            writer
                .bytes_ready(delivery_data.len() as u64)
                .await
                .expect("transport error on bytes_ready")
                .expect("failed to write data to vmo");

            assert_eq!(fixture.read_blob(hash).await, data);
            assert_eq!(
                fixture.create_blob(&hash.into(), false).await.expect_err("rewrite succeeded"),
                CreateBlobError::AlreadyExists
            );
        }

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
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
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
        assert_eq!(fixture.read_blob(hash).await, data);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_allocate_with_large_transaction() {
        const NUM_BLOBS: usize = 1024;
        let fixture = new_blob_fixture().await;

        let mut data = [0; 128];
        let mut hashes = Vec::new();

        // It doesn't matter if these blobs are compressed. We just need to fragment space.
        for _ in 0..NUM_BLOBS {
            thread_rng().fill(&mut data[..]);
            let hash = fixture.write_blob(&data, CompressionMode::Never).await;
            hashes.push(hash);
        }

        // Delete every second blob, fragmenting free space.
        for ix in 0..NUM_BLOBS / 2 {
            fixture
                .root()
                .unlink(&format!("{}", hashes[ix * 2]), &UnlinkOptions::default())
                .await
                .expect("FIDL failed")
                .expect("unlink failed");
        }

        // Create one large blob (reusing fragmented extents).
        {
            let mut data = vec![1; 1024921];
            thread_rng().fill(&mut data[..]);

            let mut builder = MerkleTreeBuilder::new();
            builder.write(&data);
            let hash = builder.finish().root();
            let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);
            assert!(compressed_data.len() as u64 > *RING_BUFFER_SIZE);

            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
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
                    .expect("failed to write data to vmo.");
                write_offset += len;
            }
            // Ensure that the blob is readable and matches what we expect.
            assert_eq!(fixture.read_blob(hash).await, data);
        }

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn bytes_ready_should_fail_if_size_invalid() {
        let fixture = new_blob_fixture().await;
        // Generate a delivery blob (size doesn't matter).
        let mut data = vec![1; 196608];
        thread_rng().fill(&mut data[..]);
        let mut builder = MerkleTreeBuilder::new();
        builder.write(&data);
        let hash = builder.finish().root();
        let blob_data = Type1Blob::generate(&data, CompressionMode::Always);
        // To simplify the test, we make sure to write enough bytes on the first call to bytes_ready
        // so that the header can be decoded (and thus the length mismatch is detected).
        let bytes_to_write = std::cmp::min((*RING_BUFFER_SIZE / 2) as usize, blob_data.len());
        Type1Blob::parse(&blob_data[..bytes_to_write])
            .unwrap()
            .expect("bytes_to_write must be long enough to cover delivery blob header!");
        // We can call get_vmo with the wrong size, because we don't know what size the blob should
        // be. Once we call bytes_ready with enough data for the header to be decoded, we should
        // detect the failure.
        for incorrect_size in [blob_data.len() - 1, blob_data.len() + 1] {
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
            let vmo = writer
                .get_vmo(incorrect_size as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");
            assert!(vmo.get_size().unwrap() > bytes_to_write as u64);
            vmo.write(&blob_data[..bytes_to_write], 0).unwrap();
            writer
                .bytes_ready(bytes_to_write as u64)
                .await
                .expect("transport error on bytes_ready")
                .expect_err("should fail writing blob if size passed to get_vmo is incorrect");
        }
        fixture.close().await;
    }
}
