// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements fuchsia.fxfs/BlobWriter for writing delivery blobs.

use crate::fuchsia::directory::FxDirectory;
use crate::fuchsia::errors::map_to_status;
use crate::fuchsia::fxblob::BlobDirectory;
use crate::fuchsia::node::FxNode;
use crate::fuchsia::volume::FxVolume;
use anyhow::{Context as _, Error};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use delivery_blob::compression::{decode_archive, ChunkInfo, ChunkedDecompressor};
use delivery_blob::Type1Blob;
use fidl::endpoints::{ControlHandle as _, RequestStream as _};
use fidl_fuchsia_fxfs::{BlobWriterRequest, BlobWriterRequestStream};
use fuchsia_hash::Hash;
use fuchsia_merkle::{MerkleTree, MerkleTreeBuilder};
use futures::{try_join, TryStreamExt as _};
use fxfs::errors::FxfsError;
use fxfs::object_handle::{ObjectHandle, WriteObjectHandle};
use fxfs::object_store::directory::{replace_child_with_object, ReplacedChild};
use fxfs::object_store::transaction::{lock_keys, LockKey};
use fxfs::object_store::{
    DataObjectHandle, HandleOptions, ObjectDescriptor, ObjectStore, Timestamp,
    BLOB_MERKLE_ATTRIBUTE_ID,
};
use fxfs::round::{round_down, round_up};
use fxfs::serialized_types::BlobMetadata;
use lazy_static::lazy_static;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use zx::{self as zx, HandleBased as _, Status};

lazy_static! {
    static ref RING_BUFFER_SIZE: u64 = 64 * (zx::system_get_page_size() as u64);
}

const PAYLOAD_BUFFER_FLUSH_THRESHOLD: usize = 131_072; /* 128 KiB */

/// Maximum amount of *compressed* data we'll record when we fail to decompress a chunk.
/// We use base64 encoding, so the actual storage of the data will be large (~50 KiB).
const MAX_CHUNK_DATA_ON_ERROR: usize = 32_768; /* 32 KiB */

fn on_decompression_error(
    merkle_root: Hash,
    chunk_index: usize,
    chunk_info: ChunkInfo,
    chunk_data: &[u8],
) {
    log::error!("Failed to decompress chunk {chunk_index} ({chunk_info:?}) for blob {merkle_root}");
    static RECORDED_CORRUPT_BLOB: AtomicBool = AtomicBool::new(false);
    if RECORDED_CORRUPT_BLOB.fetch_or(true, Ordering::Relaxed) == true {
        // To avoid buffering too much data in memory, we only keep the first corruption instance.
        return;
    }
    let len = std::cmp::min(chunk_data.len(), MAX_CHUNK_DATA_ON_ERROR);
    let encoded_chunk = BASE64_STANDARD.encode(&chunk_data[..len]);
    fxfs::metrics::detail().record_child("delivery_blob_corruption", |node| {
        node.record_string("merkle_root", &String::from(merkle_root));
        node.record_int("chunk_index", chunk_index as i64);
        node.record_int("compressed_length", chunk_info.compressed_range.len() as i64);
        node.record_int("decompressed_length", chunk_info.decompressed_range.len() as i64);
        node.record_string("chunk_data", encoded_chunk);
    });
}

/// Represents an RFC-0207 compliant delivery blob that is being written. Used to implement the
/// fuchisa.fxfs/BlobWriter protocol (see [`Self::handle_requests`]).
///
/// Once all data for the delivery blob has been written, the calculated Merkle root is verified
/// against [`Self::hash`]. On success, the blob is added under the [`Self::parent`] directory.
pub struct DeliveryBlobWriter {
    /// The expected hash for this blob. **MUST** match the Merkle root from [`Self::tree_builder`].
    hash: Hash,
    /// Handle to this blob within the filesystem.
    handle: DataObjectHandle<FxVolume>,
    /// The parent directory we will add this blob to once verified.
    parent: Arc<FxDirectory>,
    /// Set to true once we have verified the blob and ensured it was added to the blob directory.
    is_completed: bool,
    /// Total number of bytes we expect for this delivery blob (via fuchsia.fxfs/BlobWriter.GetVmo).
    /// This is checked against the header + payload size encoded within the delivery blob.
    expected_size: Option<u64>,
    /// Total number of bytes that have been written to this delivery blob so far. When this reaches
    /// [`Self::expected_size`], the blob will be verified.
    total_written: u64,
    /// Vmo for the ring buffer used for the fuchsia.fxfs/BlobWriter protocol.
    vmo: Option<zx::Vmo>,
    /// Temporary buffer used to copy data from [`Self::vmo`].
    buffer: Vec<u8>,
    /// The header for this delivery blob. Decoded once we have enough data.
    header: Option<Type1Blob>,
    /// The Merkle tree builder for this blob. Once the Merkle tree is complete, the root hash of
    /// the blob is verified, before storing the Merkle tree as part of the blob's metadata.
    tree_builder: MerkleTreeBuilder,
    /// Set to true when we've allocated space for the blob payload on disk.
    allocated_space: bool,
    /// How many bytes from the delivery blob payload have been written to disk so far.
    payload_persisted: u64,
    /// Offset within the delivery blob payload we started writing data to disk.
    payload_offset: u64,
    /// Streaming decompressor used to calculate the Merkle tree of compressed delivery blobs.
    /// Initialized once the seek table has been decoded and verified.
    decompressor: Option<ChunkedDecompressor>,
}

impl DeliveryBlobWriter {
    /// Creates a new object in the `parent` object store that manages writing the blob `hash`.
    /// After the blob is fully written and verified, it will be added as an entry under `parent`.
    /// If this object is dropped before the blob is fully written/verified, it will be tombstoned.
    pub(crate) async fn new(parent: &Arc<BlobDirectory>, hash: Hash) -> Result<Self, Error> {
        let parent = parent.directory().clone();
        let store = parent.store();
        let keys = lock_keys![LockKey::object(store.store_object_id(), parent.object_id())];
        let mut transaction = store
            .filesystem()
            .clone()
            .new_transaction(keys, Default::default())
            .await
            .context("Failed to create transaction.")?;
        let handle = ObjectStore::create_object(
            parent.volume(),
            &mut transaction,
            // Checksums are redundant for blobs, which are already content-verified.
            HandleOptions { skip_checksums: true, ..Default::default() },
            None,
            None,
        )
        .await
        .context("Failed to create object.")?;
        // Add the object to the graveyard so that it's cleaned up if we crash.
        store.add_to_graveyard(&mut transaction, handle.object_id());
        transaction.commit().await.context("Failed to commit transaction.")?;
        Ok(Self {
            hash,
            handle,
            parent,
            is_completed: false,
            expected_size: None,
            total_written: 0,
            vmo: None,
            buffer: Default::default(),
            header: None,
            tree_builder: Default::default(),
            allocated_space: false,
            payload_persisted: 0,
            payload_offset: 0,
            decompressor: None,
        })
    }
}

impl Drop for DeliveryBlobWriter {
    /// Tombstone the object if we didn't finish writing the blob or the hash didn't match.
    fn drop(&mut self) {
        if !self.is_completed {
            let store = self.handle.store();
            store
                .filesystem()
                .graveyard()
                .queue_tombstone_object(store.store_object_id(), self.handle.object_id());
        }
    }
}

impl DeliveryBlobWriter {
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

    async fn write_payload(&mut self) -> Result<(), Error> {
        self.ensure_allocated().await.with_context(|| {
            format!(
                "Failed to allocate space for blob (required space = {} bytes)",
                self.storage_size()
            )
        })?;
        let final_write =
            (self.payload_persisted as usize + self.buffer.len()) == self.header().payload_length;
        let block_size = self.handle.block_size() as usize;
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
        let mut buffer = self.handle.allocate_buffer(aligned_len).await;
        buffer.as_mut_slice()[..len].copy_from_slice(&self.buffer[..len]);
        buffer.as_mut_slice()[len..].fill(0);

        // Overwrite allocated bytes in the object's handle.
        let overwrite_fut = async {
            self.handle
                .overwrite(self.payload_persisted - self.payload_offset, buffer.as_mut(), false)
                .await
                .context("Failed to write data to object handle.")
        };
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
                parse_seek_table(self.decompressor().seek_table())
                    .context("Failed to parse seek table.")?
            } else {
                (self.header().payload_length as u64, 0u64, vec![])
            };

            Ok(Some(BlobMetadata { hashes, chunk_size, compressed_offsets, uncompressed_size }))
        } else {
            Ok(None)
        }
    }

    async fn ensure_allocated(&mut self) -> Result<(), Error> {
        if self.allocated_space {
            return Ok(());
        }
        let size = self.storage_size() as u64;
        let mut range = 0..round_up(size, self.handle.block_size()).ok_or(FxfsError::OutOfRange)?;
        let mut first_time = true;
        while range.start < range.end {
            let mut transaction =
                self.handle.new_transaction().await.context("Failed to create transaction.")?;
            if first_time {
                self.handle
                    .grow(&mut transaction, 0, size)
                    .await
                    .with_context(|| format!("Failed to grow handle to {} bytes.", size))?;
                first_time = false;
            }
            self.handle.preallocate_range(&mut transaction, &mut range).await.with_context(
                || format!("Failed to allocate range ({} to {}).", range.start, range.end),
            )?;
            transaction.commit().await.context("Failed to commit transaction.")?;
        }
        self.allocated_space = true;
        Ok(())
    }

    async fn complete(&mut self) -> Result<(), Error> {
        debug_assert!(self.payload_persisted == self.header().payload_length as u64);
        // Finish building Merkle tree and verify the hash matches the filename.
        let merkle_tree = std::mem::take(&mut self.tree_builder).finish();
        if merkle_tree.root() != self.hash {
            return Err(FxfsError::IntegrityError).with_context(|| {
                format!(
                    "Calculated Merkle root ({}) does not match given hash ({}).",
                    merkle_tree.root(),
                    self.hash
                )
            });
        }
        // Write metadata and ensure everything is flushed to disk.
        let metadata =
            self.generate_metadata(merkle_tree).context("Failed to generate metadata for blob.")?;
        if let Some(metadata) = metadata {
            let mut serialized = vec![];
            bincode::serialize_into(&mut serialized, &metadata)?;
            self.handle
                .write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &serialized)
                .await
                .context("Failed to write metadata for blob.")?;
        }
        self.handle.flush().await.context("Failed to flush blob.")?;

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
            .await
            .context("Failed to acquire context for replacement.")?
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
        .await
        .context("Replacing child failed.")?
        {
            ReplacedChild::None => {}
            _ => {
                return Err(FxfsError::AlreadyExists)
                    .with_context(|| format!("Blob {} already exists", self.hash));
            }
        }

        transaction
            .commit_with_callback(|_| {
                self.is_completed = true;
                // This can't actually add the node to the cache, because it hasn't been created
                // ever at this point. Passes in None for now as a result.
                self.parent.did_add(&name, None);
            })
            .await
            .context("Failed to commit transaction!")?;
        Ok(())
    }

    async fn truncate(&mut self, length: u64) -> Result<(), Error> {
        if self.expected_size.is_some() {
            return Err(Status::BAD_STATE).context("Blob was already truncated.");
        }
        if length < Type1Blob::HEADER.header_length as u64 {
            return Err(Status::INVALID_ARGS).context("Invalid size (too small).");
        }
        self.expected_size = Some(length);
        Ok(())
    }

    /// Process the data that exists in the writer's buffer. The writer cannot recover from errors,
    /// so this function should not be invoked after it returns any errors.
    async fn process_buffer(&mut self) -> Result<(), Error> {
        // Decode delivery blob header.
        if self.header.is_none() {
            let Some((header, payload)) =
                Type1Blob::parse(&self.buffer).context("Failed to decode delivery blob header.")?
            else {
                return Ok(()); // Not enough data to decode header yet.
            };
            let expected_size = self.expected_size.unwrap_or_default() as usize;
            let delivery_size = header.header.header_length as usize + header.payload_length;
            if expected_size != delivery_size {
                return Err(FxfsError::IntegrityError).with_context(|| {
                    format!(
                        "Expected size ({}) does not match size from blob header ({})!",
                        expected_size, delivery_size
                    )
                });
            }
            self.buffer = Vec::from(payload);
            self.header = Some(header);
        }

        // If blob is compressed, decode chunked archive header & initialize decompressor.
        if self.header().is_compressed && self.decompressor.is_none() {
            let prev_buff_len = self.buffer.len();
            let archive_length = self.header().payload_length;
            let Some((seek_table, chunk_data)) = decode_archive(&self.buffer, archive_length)
                .context("Failed to decode archive header")?
            else {
                return Ok(()); // Not enough data to decode archive header/seek table.
            };
            // We store the seek table out-of-line with the data, so we don't persist that
            // part of the payload directly.
            self.buffer = Vec::from(chunk_data);
            self.payload_offset = (prev_buff_len - self.buffer.len()) as u64;
            self.payload_persisted = self.payload_offset;
            let hash = self.hash.clone();
            self.decompressor = Some(
                ChunkedDecompressor::new_with_error_handler(
                    seek_table,
                    Box::new(move |chunk_index, chunk_info, chunk_data| {
                        on_decompression_error(hash, chunk_index, chunk_info, chunk_data);
                    }),
                )
                .context("Failed to create decompressor")?,
            );
        }

        // Write payload to disk and update Merkle tree.
        if !self.buffer.is_empty() {
            // This ends up being a large future, so we box it.
            Box::pin(self.write_payload()).await?;
        }
        Ok(())
    }

    async fn get_vmo(&mut self, size: u64) -> Result<zx::Vmo, Error> {
        self.truncate(size)
            .await
            .with_context(|| format!("Failed to truncate blob {} to size {}", self.hash, size))?;
        if self.vmo.is_some() {
            return Err(FxfsError::AlreadyExists)
                .with_context(|| format!("VMO was already created for blob {}", self.hash));
        }
        let vmo = zx::Vmo::create(*RING_BUFFER_SIZE).with_context(|| {
            format!("Failed to create VMO of size {} for writing", *RING_BUFFER_SIZE)
        })?;
        let vmo_dup = vmo
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .context("Failed to duplicate VMO handle")?;
        self.vmo = Some(vmo);
        Ok(vmo_dup)
    }

    /// Called when there is more data in the ring buffer ready to be processed. Once all data has
    /// been written and verified, it will be added to the parent directory.
    ///
    /// *WARNING*: If this function fails, the writer will remain in an invalid state. Errors should
    /// be latched by the caller instead of calling this function again. The writer can be closed,
    /// and a new one can be opened to attempt writing the delivery blob again.
    async fn bytes_ready(&mut self, bytes_written: u64) -> Result<(), Error> {
        if bytes_written > *RING_BUFFER_SIZE {
            return Err(FxfsError::OutOfRange).with_context(|| {
                format!(
                    "bytes_written ({}) exceeds size of ring buffer ({})",
                    bytes_written, *RING_BUFFER_SIZE
                )
            });
        }
        let expected_size = self
            .expected_size
            .ok_or(Status::BAD_STATE)
            .context("Must call BlobWriter.GetVmo before writing data to blob.")?;
        if (self.total_written + bytes_written) > expected_size {
            return Err(Status::BUFFER_TOO_SMALL).with_context(|| {
                format!(
                    "Wrote more bytes than passed to BlobWriter.GetVmo (expected = {}, written = {}).",
                    expected_size,
                    self.total_written + bytes_written
                )
            });
        }
        let Some(ref vmo) = self.vmo else {
            return Err(Status::BAD_STATE)
                .context("BlobWriter.GetVmo must be called before BlobWriter.BytesReady.");
        };
        // Extend our write buffer by the amount of data that was written.
        let prev_len = self.buffer.len();
        self.buffer.resize(prev_len + bytes_written as usize, 0);
        let mut buf = &mut self.buffer[prev_len..];
        let write_offset = self.total_written;
        self.total_written += bytes_written;
        // Copy data from the ring buffer into our internal buffer.
        let vmo_offset = write_offset % *RING_BUFFER_SIZE;
        if vmo_offset + bytes_written > *RING_BUFFER_SIZE {
            let split = (*RING_BUFFER_SIZE - vmo_offset) as usize;
            vmo.read(&mut buf[0..split], vmo_offset).context("failed to read from VMO")?;
            vmo.read(&mut buf[split..], 0).context("failed to read from VMO")?;
        } else {
            vmo.read(&mut buf, vmo_offset).context("failed to read from VMO")?;
        }
        // Process the data from the buffer.
        self.process_buffer().await.with_context(|| {
            // LINT.IfChange(blob_write_failure)
            format!(
                "failed to write blob {} (bytes_written = {}, offset = {}, delivery_size = {})",
                self.hash, bytes_written, write_offset, expected_size
            )
            // LINT.ThenChange(/tools/testing/tefmocheck/string_in_log_check.go:blob_write_failure)
        })?;
        // If all bytes for this delivery blob were written successfully, attempt to verify the blob
        // and add it to the parent directory.
        if self.total_written == expected_size {
            self.complete().await?;
        }
        Ok(())
    }

    /// Handle fuchsia.fxfs/BlobWriter requests for this delivery blob.
    pub(crate) async fn handle_requests(
        mut self,
        mut request_stream: BlobWriterRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = request_stream.try_next().await? {
            match request {
                BlobWriterRequest::GetVmo { size, responder } => {
                    let result = self.get_vmo(size).await.map_err(|error| {
                        log::error!(error:?; "BlobWriter.GetVmo failed.");
                        map_to_status(error).into_raw()
                    });
                    responder.send(result).unwrap_or_else(|error| {
                        log::error!(error:?; "Failed to send BlobWriter.GetVmo response.");
                    });
                }
                BlobWriterRequest::BytesReady { bytes_written, responder } => {
                    let result = self.bytes_ready(bytes_written).await.map_err(|error| {
                        log::error!(error:?; "BlobWriter.BytesReady failed.");
                        map_to_status(error)
                    });
                    responder.send(result.map_err(Status::into_raw)).unwrap_or_else(|error| {
                        log::error!(error:?; "Failed to send BlobWriter.BytesReady response.");
                    });
                    // If any error occurs when handling a BytesReady request, the writer will
                    // remain in an unrecoverable state. The client must use the BlobCreator
                    // protocol to open a new writer and try writing the blob again.
                    if let Err(status) = result {
                        request_stream.control_handle().shutdown_with_epitaph(status);
                        break;
                    }
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
    use super::*;
    use crate::fuchsia::fxblob::testing::{new_blob_fixture, BlobFixture};
    use core::ops::Range;
    use delivery_blob::CompressionMode;
    use fidl_fuchsia_fxfs::CreateBlobError;
    use fidl_fuchsia_io::UnlinkOptions;
    use fuchsia_async as fasync;
    use rand::{thread_rng, Rng};

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
        let hash = fuchsia_merkle::from_slice(&data).root();
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
        let incorrect_hash = fuchsia_merkle::from_slice(&data[..data.len() - 1]).root();
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

    /// Ensure we get `IO_DATA_INTEGRITY` if one of the compressed chunks is corrupted.
    #[fasync::run(10, test)]
    async fn test_detect_corruption() {
        let fixture = new_blob_fixture().await;

        let mut data = vec![1; 65536];
        thread_rng().fill(&mut data[..]);

        let hash = fuchsia_merkle::from_slice(&data).root();
        let mut compressed_data = Type1Blob::generate(&data, CompressionMode::Always);
        let len = compressed_data.len();
        compressed_data[len - 1024..].fill(0);
        {
            let writer =
                fixture.create_blob(&hash.into(), false).await.expect("failed to create blob");
            let vmo = writer
                .get_vmo(compressed_data.len() as u64)
                .await
                .expect("transport error on get_vmo")
                .expect("failed to get vmo");
            vmo.write(&compressed_data, 0).expect("failed to write to vmo");

            assert_eq!(
                writer
                    .bytes_ready(compressed_data.len() as u64)
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

        let hash = fuchsia_merkle::from_slice(&data).root();
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

            let hash = fuchsia_merkle::from_slice(&data).root();
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

        let hash = fuchsia_merkle::from_slice(&data).root();
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

        let hash = fuchsia_merkle::from_slice(&data).root();
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

        let hash = fuchsia_merkle::from_slice(&data).root();
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

            let hash = fuchsia_merkle::from_slice(&data).root();
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
        let hash = fuchsia_merkle::from_slice(&data).root();
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
            assert!(vmo.get_size().unwrap() >= bytes_to_write as u64);
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
