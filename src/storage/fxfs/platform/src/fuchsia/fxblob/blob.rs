// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`FxBlob`] node type used to represent an immutable blob persisted to
//! disk which can be read back.

use crate::fuchsia::directory::FxDirectory;
use crate::fuchsia::errors::map_to_status;
use crate::fuchsia::node::{FxNode, OpenedNode};
use crate::fuchsia::pager::{
    default_page_in, MarkDirtyRange, PageInRange, PagerBacked, PagerPacketReceiverRegistration,
};
use crate::fuchsia::volume::{FxVolume, BASE_READ_AHEAD_SIZE};
use anyhow::{anyhow, bail, ensure, Context, Error};
use fidl_fuchsia_feedback::{Annotation, Attachment, CrashReport};
use fidl_fuchsia_mem::Buffer;
use fuchsia_component_client::connect_to_protocol;
use fuchsia_hash::Hash;
use fuchsia_merkle::{hash_block, MerkleTree};
use futures::try_join;
use fxfs::errors::FxfsError;
use fxfs::log::*;
use fxfs::object_handle::{ObjectHandle, ReadObjectHandle};
use fxfs::object_store::{DataObjectHandle, ObjectDescriptor};
use fxfs::round::{round_down, round_up};
use fxfs::serialized_types::BlobMetadata;
use fxfs_macros::ToWeakNode;
use std::future::Future;
use std::num::NonZero;
use std::ops::Range;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use storage_device::buffer;
use zx::{self as zx, AsHandleRef, HandleBased, Status};

#[cfg(any(test, feature = "refault-tracking"))]
use std::sync::atomic::AtomicU8;

pub const BLOCK_SIZE: u64 = fuchsia_merkle::BLOCK_SIZE as u64;

// When the top bit of the open count is set, it means the file has been deleted and when the count
// drops to zero, it will be tombstoned.  Once it has dropped to zero, it cannot be opened again
// (assertions will fire).
const PURGED: usize = 1 << (usize::BITS - 1);

/// Represents an immutable blob stored on Fxfs with associated an merkle tree.
#[derive(ToWeakNode)]
pub struct FxBlob {
    handle: DataObjectHandle<FxVolume>,
    vmo: zx::Vmo,
    open_count: AtomicUsize,
    merkle_root: Hash,
    merkle_leaves: Box<[Hash]>,
    compression_info: Option<CompressionInfo>,
    uncompressed_size: u64, // always set.
    pager_packet_receiver_registration: Arc<PagerPacketReceiverRegistration<Self>>,

    #[cfg(any(test, feature = "refault-tracking"))]
    chunks_supplied: Vec<AtomicU8>,
}

impl FxBlob {
    pub fn new(
        handle: DataObjectHandle<FxVolume>,
        merkle_tree: MerkleTree,
        compression_info: Option<CompressionInfo>,
        uncompressed_size: u64,
    ) -> Arc<Self> {
        // Only the merkle root and leaves are needed, the rest of the tree can be dropped.
        let merkle_root = merkle_tree.root();
        // The merkle leaves are intentionally copied to remove all of the spare capacity from the
        // Vec.
        let merkle_leaves = merkle_tree.as_ref()[0].clone().into_boxed_slice();

        #[cfg(any(test, feature = "refault-tracking"))]
        let chunks_supplied: Vec<AtomicU8> = std::iter::repeat_with(AtomicU8::default)
            .take(uncompressed_size.div_ceil(min_chunk_size(&compression_info)) as usize)
            .collect();

        Arc::new_cyclic(|weak| {
            let (vmo, pager_packet_receiver_registration) = handle
                .owner()
                .pager()
                .create_vmo(weak.clone(), uncompressed_size, zx::VmoOptions::empty())
                .unwrap();
            set_vmo_name(&vmo, &merkle_root);
            Self {
                handle,
                vmo,
                open_count: AtomicUsize::new(0),
                merkle_root,
                merkle_leaves,
                compression_info,
                uncompressed_size,
                pager_packet_receiver_registration: Arc::new(pager_packet_receiver_registration),
                #[cfg(any(test, feature = "refault-tracking"))]
                chunks_supplied,
            }
        })
    }

    /// Returns the new blob and some deferred work in an async future that must complete before
    /// returning to the external caller.
    pub fn overwrite_me(
        self: &Arc<Self>,
        handle: DataObjectHandle<FxVolume>,
        compression_info: Option<CompressionInfo>,
    ) -> (Arc<Self>, Option<Pin<Box<impl Future<Output = ()>>>>) {
        let vmo = self.vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();

        let new_blob = Arc::new(Self {
            handle,
            vmo,
            open_count: AtomicUsize::new(0),
            merkle_root: self.merkle_root,
            merkle_leaves: self.merkle_leaves.iter().cloned().collect(),
            compression_info,
            uncompressed_size: self.uncompressed_size,
            pager_packet_receiver_registration: self.pager_packet_receiver_registration.clone(),
            #[cfg(any(test, feature = "refault-tracking"))]
            chunks_supplied: self
                .chunks_supplied
                .iter()
                .map(|x| AtomicU8::new(x.load(Ordering::Relaxed)))
                .collect(),
        });

        // Lock must be held until the open counts is incremented to prevent concurrent handling of
        // zero children signals.
        let receiver_lock =
            self.pager_packet_receiver_registration.receiver().set_receiver(&new_blob);
        let deferred_work = if receiver_lock.is_strong() {
            // If there was a strong moved between them, then the counts exchange as well. It is
            // only important that the increment happen under the lock as it may handle the next
            // zero children signal, no new requests can now go to the old blob, but to safely
            // ensure that all existing requests finish, we will defer to an async context.
            new_blob.open_count_add_one();
            let old_blob = self.clone();
            Some(Box::pin(async move {
                old_blob.pager().page_in_barrier().await;
                old_blob.open_count_sub_one();
            }))
        } else {
            None
        };
        (new_blob, deferred_work)
    }

    pub fn root(&self) -> Hash {
        self.merkle_root
    }

    #[cfg(any(test, feature = "refault-tracking"))]
    fn record_page_fault(&self, range: &Range<u64>) {
        let chunk_size = min_chunk_size(&self.compression_info);

        let first_chunk = range.start / chunk_size;
        // The end of the range may not be chunk aligned if it's the last chunk.
        let last_chunk = range.end.div_ceil(chunk_size);
        let page_size = zx::system_get_page_size() as u64;

        for chunk in first_chunk..last_chunk {
            let count = self.chunks_supplied[chunk as usize]
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_add(1))
                })
                .unwrap()
                .saturating_add(1);

            fxfs_trace::instant!(
                c"blob_supply",
                fxfs_trace::Scope::Thread,
                "blob" => self.object_id(),
                "chunk" => chunk,
                "count" => count as u32
            );
            if count != 1 {
                let bytes = std::cmp::min(
                    self.uncompressed_size.next_multiple_of(page_size),
                    (chunk + 1) * chunk_size,
                ) - chunk * chunk_size;
                self.handle.owner().refault_tracker().record_refault(bytes, count);
            }
        }
    }
}

impl Drop for FxBlob {
    fn drop(&mut self) {
        let volume = self.handle.owner();
        volume.cache().remove(self);
    }
}

impl OpenedNode<FxBlob> {
    /// Creates a read-only child VMO for this blob backed by the pager. The blob cannot be purged
    /// until all child VMOs have been destroyed.
    ///
    /// *WARNING*: We need to ensure the open count is non-zero before invoking this function, so
    /// it is only implemented for [`OpenedNode<FxBlob>`]. This prevents the blob from being purged
    /// before we get a chance to register it with the pager for [`zx::Signals::VMO_ZERO_CHILDREN`].
    pub fn create_child_vmo(&self) -> Result<zx::Vmo, Status> {
        let blob = self.0.as_ref();
        let child_vmo = blob.vmo.create_child(
            zx::VmoChildOptions::REFERENCE | zx::VmoChildOptions::NO_WRITE,
            0,
            0,
        )?;
        if blob.handle.owner().pager().watch_for_zero_children(blob).map_err(map_to_status)? {
            // Take an open count so that we keep this object alive if it is otherwise closed. This
            // is only valid since we know the current open count is non-zero, otherwise we might
            // increment the open count after the blob has been purged.
            blob.open_count_add_one();
        }
        Ok(child_vmo)
    }
}

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
                .queue_tombstone_object(store.store_object_id(), self.object_id());
        }
    }

    fn object_descriptor(&self) -> ObjectDescriptor {
        ObjectDescriptor::File
    }

    fn terminate(&self) {
        self.pager_packet_receiver_registration.stop_watching_for_zero_children();
    }

    fn mark_to_be_purged(&self) {
        let old = self.open_count.fetch_or(PURGED, Ordering::Relaxed);
        assert!(old & PURGED == 0);
        if old == 0 {
            let store = self.handle.store();
            store
                .filesystem()
                .graveyard()
                .queue_tombstone_object(store.store_object_id(), self.object_id());
        }
    }
}

impl PagerBacked for FxBlob {
    fn pager(&self) -> &crate::pager::Pager {
        self.handle.owner().pager()
    }

    fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration<Self> {
        &self.pager_packet_receiver_registration
    }

    fn vmo(&self) -> &zx::Vmo {
        &self.vmo
    }

    fn page_in(self: Arc<Self>, range: PageInRange<Self>) {
        let read_ahead_size = self.handle.owner().read_ahead_size();
        let read_ahead_size = if let Some(compression_info) = &self.compression_info {
            read_ahead_size_for_chunk_size(compression_info.chunk_size, read_ahead_size)
        } else {
            read_ahead_size
        };
        // Delegate to the generic page handling code.
        default_page_in(self, range, read_ahead_size)
    }

    fn mark_dirty(self: Arc<Self>, _range: MarkDirtyRange<Self>) {
        unreachable!();
    }

    fn on_zero_children(self: Arc<Self>) {
        self.open_count_sub_one();
    }

    fn byte_size(&self) -> u64 {
        self.uncompressed_size
    }

    async fn aligned_read(&self, range: Range<u64>) -> Result<buffer::Buffer<'_>, Error> {
        thread_local! {
            static DECOMPRESSOR: std::cell::RefCell<zstd::bulk::Decompressor<'static>> =
                std::cell::RefCell::new(zstd::bulk::Decompressor::new().unwrap());
        }

        #[cfg(any(test, feature = "refault-tracking"))]
        self.record_page_fault(&range);

        let mut buffer = self.handle.allocate_buffer((range.end - range.start) as usize).await;
        let read = match &self.compression_info {
            None => self.handle.read(range.start, buffer.as_mut()).await?,
            Some(compression_info) => {
                let compressed_offsets =
                    match compression_info.compressed_range_for_uncompressed_range(&range)? {
                        (start, None) => start..self.handle.get_size(),
                        (start, Some(end)) => start..end.get(),
                    };
                let bs = self.handle.block_size();
                let aligned = round_down(compressed_offsets.start, bs)
                    ..round_up(compressed_offsets.end, bs).unwrap();
                let mut compressed_buf =
                    self.handle.allocate_buffer((aligned.end - aligned.start) as usize).await;

                let mut decompression_errors = 0;
                let len = (std::cmp::min(range.end, self.uncompressed_size) - range.start) as usize;
                let decompressed_size = loop {
                    let (read, _) = try_join!(
                        self.handle.read(aligned.start, compressed_buf.as_mut()),
                        async {
                            buffer
                                .allocator()
                                .buffer_source()
                                .commit_range(buffer.range())
                                .map_err(|e| e.into())
                        }
                    )
                    .with_context(|| {
                        format!(
                            "Failed to read compressed range {:?}, len {}",
                            aligned,
                            self.handle.get_size()
                        )
                    })?;
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

                    let buf = buffer.as_mut_slice();
                    match DECOMPRESSOR.with(|decompressor| {
                        fxfs_trace::duration!(c"blob-decompress", "len" => len);
                        let mut decompressor = decompressor.borrow_mut();
                        decompressor.decompress_to_buffer(
                            &compressed_buf.as_slice()[compressed_buf_range],
                            &mut buf[..len],
                        )
                    }) {
                        Ok(size) => break size,
                        Err(error) => {
                            static DONE_ONCE: AtomicBool = AtomicBool::new(false);
                            if !DONE_ONCE.swap(true, Ordering::Relaxed) {
                                if let Ok(proxy) = connect_to_protocol::<
                                    fidl_fuchsia_feedback::CrashReporterMarker,
                                >() {
                                    let size = compressed_buf.len() as u64;
                                    let vmo = zx::Vmo::create(size).unwrap();
                                    vmo.write(compressed_buf.as_slice(), 0).unwrap();
                                    if let Err(e) = proxy
                                        .file_report(CrashReport {
                                            program_name: Some("fxfs".to_string()),
                                            crash_signature: Some(
                                                "fuchsia-fxfs-decompression_error".to_string(),
                                            ),
                                            is_fatal: Some(false),
                                            annotations: Some(vec![
                                                Annotation {
                                                    key: "fxfs.range".to_string(),
                                                    value: format!("{:?}", range),
                                                },
                                                Annotation {
                                                    key: "fxfs.compressed_offsets".to_string(),
                                                    value: format!("{:?}", compressed_offsets),
                                                },
                                                Annotation {
                                                    key: "fxfs.merkle_root".to_string(),
                                                    value: format!("{}", self.merkle_root),
                                                },
                                            ]),
                                            attachments: Some(vec![Attachment {
                                                key: "fxfs_compressed_data".to_string(),
                                                value: Buffer { vmo, size },
                                            }]),
                                            ..Default::default()
                                        })
                                        .await
                                    {
                                        error!(e:?; "Failed to file crash report");
                                    } else {
                                        warn!("Filed crash report for decompression error");
                                    }
                                } else {
                                    error!("Failed to connect to crash report service");
                                }
                            }
                            decompression_errors += 1;
                            if decompression_errors == 2 {
                                bail!(anyhow!(FxfsError::IntegrityError)
                                    .context(format!("Decompression error: {error:?}")));
                            } else {
                                warn!(error:?; "Decompression error; retrying");
                            }
                        }
                    }
                }; // loop
                if decompression_errors > 0 {
                    info!("Read succeeded on second attempt");
                }
                ensure!(
                    decompressed_size == len,
                    anyhow!(FxfsError::IntegrityError).context("Decompressed length mismatch")
                );
                len
            }
        };
        // TODO(https://fxbug.dev/42073035): This should be offloaded to the kernel at which point
        // we can delete this.
        let mut offset = range.start as usize;
        let bs = BLOCK_SIZE as usize;
        {
            fxfs_trace::duration!(c"blob-verify", "len" => read);
            for b in buffer.as_slice()[..read].chunks(bs) {
                ensure!(
                    hash_block(b, offset) == self.merkle_leaves[offset / bs],
                    anyhow!(FxfsError::Inconsistent).context("Hash mismatch")
                );
                offset += bs;
            }
        }
        // Zero the tail.
        buffer.as_mut_slice()[read..].fill(0);
        Ok(buffer)
    }
}

pub struct CompressionInfo {
    chunk_size: u64,
    // The chunked compression format stores 0 as the first offset but it's not stored here. Not
    // storing the 0 avoids the allocation for all blobs smaller than 128KiB (the read-ahead size).
    small_offsets: Box<[u32]>,
    large_offsets: Box<[u64]>,
}

impl CompressionInfo {
    pub fn from_metadata(metadata: BlobMetadata) -> Result<Option<Self>, Error> {
        Ok(if metadata.compressed_offsets.is_empty() {
            None
        } else {
            Some(Self::new(metadata.chunk_size, metadata.compressed_offsets)?)
        })
    }

    fn new(chunk_size: u64, offsets: Vec<u64>) -> Result<Self, Error> {
        // All of the read-ahead sizes must be a multiple of the base read-ahead size.
        let min_read_size = read_ahead_size_for_chunk_size(chunk_size, BASE_READ_AHEAD_SIZE);

        // FxBlob only constructs CompressionInfo when offsets is not empty so there should always
        // be at least 1. The chunked compression format stipulates that the first offset is always
        // zero but this value comes from disk so shouldn't be trusted.
        ensure!(
            *offsets.first().expect("There must at least 1 offset") == 0,
            FxfsError::IntegrityError
        );

        let chunks_per_read = (min_read_size / chunk_size) as usize;
        if offsets.len() <= chunks_per_read {
            // Simple case where the blob is smaller than the read size so only the 0 offset is
            // relevant. The 0 isn't stored so no allocation is necessary.
            Ok(Self { chunk_size, small_offsets: Box::default(), large_offsets: Box::default() })
        } else {
            let partition_point = if *offsets.last().unwrap() <= u32::MAX as u64 {
                // The last element is checked first since most blobs will be smaller than 4GiB.
                offsets.len()
            } else {
                offsets.partition_point(|&x| x <= u32::MAX as u64)
            };

            // The partition point is the index of the first compression offset that's > u32::MAX or
            // the length of `offsets` if there are no compression offsets > u32::MAX. This index
            // might correspond to a compression offset that is in the middle of a read operation.
            // In that case, the index is advanced to the start of the next read operation which
            // might be beyond the end of `offsets`.
            //
            // Example with chunks_per_read = 3 and 8 offsets:
            // 0  1  2  3  4  5  6  7
            // |-----|  |-----|  |---|
            // read 1   read 2   read 3
            //
            // If the partition point is 3 then it will stay at 3.
            // If the partition point is 4 then it will be advanced to 6.
            // If the partition point is 7 then it will be advanced to 9.
            //
            // This is simulating doing the partition on just the list of the first compression
            // offset of each read without materializing the list.
            let read_aligned_partition_point = partition_point.next_multiple_of(chunks_per_read);
            // Subtract 1 because the 0 offset isn't stored.
            let mut small_offsets =
                Vec::with_capacity(read_aligned_partition_point / chunks_per_read - 1);
            small_offsets.extend(
                offsets[0..partition_point]
                    .iter()
                    .step_by(chunks_per_read)
                    .skip(1)
                    .map(|x| *x as u32),
            );

            let large_offsets = if read_aligned_partition_point >= offsets.len() {
                Box::default()
            } else {
                let mut large_offsets = Vec::with_capacity(
                    (offsets.len() - read_aligned_partition_point).div_ceil(chunks_per_read),
                );
                large_offsets.extend(
                    offsets[read_aligned_partition_point..].iter().step_by(chunks_per_read),
                );
                large_offsets.into_boxed_slice()
            };

            Ok(Self {
                chunk_size,
                small_offsets: small_offsets.into_boxed_slice(),
                large_offsets: large_offsets,
            })
        }
    }

    fn compressed_range_for_uncompressed_range(
        &self,
        range: &Range<u64>,
    ) -> Result<(u64, Option<NonZero<u64>>), Error> {
        let min_read_size = read_ahead_size_for_chunk_size(self.chunk_size, BASE_READ_AHEAD_SIZE);
        ensure!(range.start.is_multiple_of(min_read_size), FxfsError::Inconsistent);

        // The "0" compression offset isn't stored so all of the compression offsets are shifted
        // left by 1. This makes `start_index - 1` the start of the range.
        let start_index = (range.start / min_read_size) as usize;
        let start_offset = if start_index == 0 {
            0
        } else if start_index - 1 < self.small_offsets.len() {
            self.small_offsets[start_index - 1] as u64
        } else if start_index - 1 - self.small_offsets.len() < self.large_offsets.len() {
            self.large_offsets[start_index - 1 - self.small_offsets.len()]
        } else {
            return Err(FxfsError::OutOfRange.into());
        };

        // The end of the range may not be aligned to `min_read_size` for the last chunk.
        let end_index = range.end.div_ceil(min_read_size) as usize - 1;
        let end_offset = if end_index < self.small_offsets.len() {
            ensure!(range.end.is_multiple_of(min_read_size), FxfsError::Inconsistent);
            Some(NonZero::new(self.small_offsets[end_index] as u64).unwrap())
        } else if end_index - self.small_offsets.len() < self.large_offsets.len() {
            ensure!(range.end.is_multiple_of(min_read_size), FxfsError::Inconsistent);
            Some(NonZero::new(self.large_offsets[end_index - self.small_offsets.len()]).unwrap())
        } else {
            None
        };
        Ok((start_offset, end_offset))
    }
}

fn set_vmo_name(vmo: &zx::Vmo, merkle_root: &Hash) {
    let trimmed_merkle = &merkle_root.to_string()[0..8];
    let name = format!("blob-{}", trimmed_merkle);
    let name = zx::Name::new(&name).unwrap();
    vmo.set_name(&name).unwrap();
}

#[cfg(any(test, feature = "refault-tracking"))]
fn min_chunk_size(compression_info: &Option<CompressionInfo>) -> u64 {
    if let Some(compression_info) = compression_info {
        read_ahead_size_for_chunk_size(compression_info.chunk_size, BASE_READ_AHEAD_SIZE)
    } else {
        BASE_READ_AHEAD_SIZE
    }
}

fn read_ahead_size_for_chunk_size(chunk_size: u64, suggested_read_ahead_size: u64) -> u64 {
    if chunk_size >= suggested_read_ahead_size {
        chunk_size
    } else {
        round_down(suggested_read_ahead_size, chunk_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuchsia::epochs::Epochs;
    use crate::fuchsia::fxblob::testing::{new_blob_fixture, BlobFixture};
    use crate::fuchsia::memory_pressure::MemoryPressureLevel;
    use crate::fuchsia::pager::PageInRange;
    use crate::fuchsia::volume::{MemoryPressureConfig, MAX_READ_AHEAD_SIZE};
    use assert_matches::assert_matches;
    use delivery_blob::CompressionMode;
    use fuchsia_async as fasync;
    use std::time::Duration;

    #[fasync::run(10, test)]
    async fn test_empty_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;
        assert_eq!(fixture.read_blob(hash).await, data);

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_large_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![3; 3_000_000];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;

        assert_eq!(fixture.read_blob(hash).await, data);

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_large_compressed_blob() {
        let fixture = new_blob_fixture().await;

        let data = vec![3; 3_000_000];
        let hash = fixture.write_blob(&data, CompressionMode::Always).await;

        assert_eq!(fixture.read_blob(hash).await, data);

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_non_page_aligned_blob() {
        let fixture = new_blob_fixture().await;

        let page_size = zx::system_get_page_size() as usize;
        let data = vec![0xffu8; page_size - 1];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;
        assert_eq!(fixture.read_blob(hash).await, data);

        {
            let vmo = fixture.get_blob_vmo(hash).await;
            let mut buf = vec![0x11u8; page_size];
            vmo.read(&mut buf[..], 0).expect("vmo read failed");
            assert_eq!(data, buf[..data.len()]);
            // Ensure the tail is zeroed
            assert_eq!(buf[data.len()], 0);
        }

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_blob_invalid_contents() {
        let fixture = new_blob_fixture().await;

        let data = vec![0xffu8; (MAX_READ_AHEAD_SIZE + BLOCK_SIZE) as usize];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;
        let name = format!("{}", hash);

        {
            // Overwrite the second read-ahead window.  The first window should successfully verify.
            let handle = fixture.get_blob_handle(&name).await;
            let mut transaction =
                handle.new_transaction().await.expect("failed to create transaction");
            let mut buf = handle.allocate_buffer(BLOCK_SIZE as usize).await;
            buf.as_mut_slice().fill(0);
            handle
                .txn_write(&mut transaction, MAX_READ_AHEAD_SIZE, buf.as_ref())
                .await
                .expect("txn_write failed");
            transaction.commit().await.expect("failed to commit transaction");
        }

        {
            let blob_vmo = fixture.get_blob_vmo(hash).await;
            let mut buf = vec![0; BLOCK_SIZE as usize];
            assert_matches!(blob_vmo.read(&mut buf[..], 0), Ok(_));
            assert_matches!(
                blob_vmo.read(&mut buf[..], MAX_READ_AHEAD_SIZE),
                Err(zx::Status::IO_DATA_INTEGRITY)
            );
        }

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_blob_vmos_are_immutable() {
        let fixture = new_blob_fixture().await;

        let data = vec![0xffu8; 500];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;
        let blob_vmo = fixture.get_blob_vmo(hash).await;

        // The VMO shouldn't be resizable.
        assert_matches!(blob_vmo.set_size(20), Err(_));

        // The VMO shouldn't be writable.
        assert_matches!(blob_vmo.write(b"overwrite", 0), Err(_));

        // The VMO's content size shouldn't be modifiable.
        assert_matches!(blob_vmo.set_stream_size(20), Err(_));

        fixture.close().await;
    }

    #[fuchsia::test]
    fn test_compression_info_offsets_must_start_with_zero() {
        assert!(CompressionInfo::new(BASE_READ_AHEAD_SIZE, vec![1]).is_err());
        assert!(CompressionInfo::new(BASE_READ_AHEAD_SIZE, vec![0]).is_ok());
    }

    #[fuchsia::test]
    fn test_compression_info_splitting_offsets() {
        const MAX_SMALL_OFFSET: u64 = u32::MAX as u64;

        // Single chunk blob doesn't store any offsets.
        let compression_info = CompressionInfo::new(BASE_READ_AHEAD_SIZE, vec![0]).unwrap();
        assert!(compression_info.small_offsets.is_empty());
        assert!(compression_info.large_offsets.is_empty());

        // The blob has 4 chunks and there's 4 chunks per read and the 0 offset isn't stored so no
        // offsets are stored.
        let compression_info =
            CompressionInfo::new(BASE_READ_AHEAD_SIZE / 4, vec![0, 10, 20, 30]).unwrap();
        assert!(compression_info.small_offsets.is_empty());
        assert!(compression_info.large_offsets.is_empty());

        // The blob has 5 chunks and there's 4 chunks per read. Only the offset of the 5th chunk is
        // stored.
        let compression_info =
            CompressionInfo::new(BASE_READ_AHEAD_SIZE / 4, vec![0, 10, 20, 30, 40]).unwrap();
        assert_eq!(compression_info.small_offsets.as_ref(), &[40]);
        assert!(compression_info.large_offsets.is_empty());

        // The blob has 5 chunks and there's 4 chunks per read. The 5th chunks offset is large so
        // it's stored as a large offset.
        let compression_info = CompressionInfo::new(
            BASE_READ_AHEAD_SIZE / 4,
            vec![0, 10, 20, 30, MAX_SMALL_OFFSET + 1],
        )
        .unwrap();
        assert!(compression_info.small_offsets.is_empty());
        assert_eq!(compression_info.large_offsets.as_ref(), &[MAX_SMALL_OFFSET + 1]);

        // The blob has 6 chunks and there's 4 chunks per read. The 6th chunk is large but isn't
        // relevant.
        let compression_info = CompressionInfo::new(
            BASE_READ_AHEAD_SIZE / 4,
            vec![0, 10, 20, 30, 40, MAX_SMALL_OFFSET + 1],
        )
        .unwrap();
        assert_eq!(compression_info.small_offsets.as_ref(), &[40]);
        assert!(compression_info.large_offsets.is_empty());

        let compression_info = CompressionInfo::new(
            BASE_READ_AHEAD_SIZE,
            vec![
                0,
                10,
                20,
                30,
                MAX_SMALL_OFFSET + 10,
                MAX_SMALL_OFFSET + 20,
                MAX_SMALL_OFFSET + 30,
                MAX_SMALL_OFFSET + 40,
            ],
        )
        .unwrap();
        assert_eq!(compression_info.small_offsets.as_ref(), &[10, 20, 30]);
        assert_eq!(
            compression_info.large_offsets.as_ref(),
            &[
                MAX_SMALL_OFFSET + 10,
                MAX_SMALL_OFFSET + 20,
                MAX_SMALL_OFFSET + 30,
                MAX_SMALL_OFFSET + 40,
            ]
        );
    }

    #[fuchsia::test]
    fn test_compression_info_compressed_range_for_uncompressed_range() {
        const MAX_SMALL_OFFSET: u64 = u32::MAX as u64;

        fn check_compression_ranges(
            offsets: Vec<u64>,
            expected_ranges: Vec<(u64, Option<u64>)>,
            chunk_size: u64,
            read_ahead_size: u64,
        ) {
            let compression_info = CompressionInfo::new(chunk_size, offsets).unwrap();
            for (i, range) in expected_ranges.into_iter().enumerate() {
                let i = i as u64;
                let result = compression_info
                    .compressed_range_for_uncompressed_range(
                        &(i * read_ahead_size..(i + 1) * read_ahead_size),
                    )
                    .unwrap();
                assert_eq!(result, (range.0, range.1.map(|end| NonZero::new(end).unwrap())));
            }
        }

        check_compression_ranges(
            vec![0, 10, 20, 30],
            vec![(0, None)],
            BASE_READ_AHEAD_SIZE / 4,
            BASE_READ_AHEAD_SIZE,
        );
        check_compression_ranges(
            vec![0, 10, 20, 30],
            vec![(0, Some(10)), (10, Some(20)), (20, Some(30)), (30, None)],
            BASE_READ_AHEAD_SIZE,
            BASE_READ_AHEAD_SIZE,
        );
        check_compression_ranges(
            vec![0, 10, 20, 30],
            vec![(0, Some(20)), (20, None)],
            BASE_READ_AHEAD_SIZE,
            BASE_READ_AHEAD_SIZE * 2,
        );
        check_compression_ranges(
            vec![0, 10, 20, 30, 40],
            vec![(0, Some(40)), (40, None)],
            BASE_READ_AHEAD_SIZE / 4,
            BASE_READ_AHEAD_SIZE,
        );
        check_compression_ranges(
            vec![0, 10, 20, 30, MAX_SMALL_OFFSET + 10],
            vec![(0, Some(MAX_SMALL_OFFSET + 10)), (MAX_SMALL_OFFSET + 10, None)],
            BASE_READ_AHEAD_SIZE / 4,
            BASE_READ_AHEAD_SIZE,
        );
        check_compression_ranges(
            vec![
                0,
                10,
                20,
                30,
                MAX_SMALL_OFFSET + 10,
                MAX_SMALL_OFFSET + 20,
                MAX_SMALL_OFFSET + 30,
                MAX_SMALL_OFFSET + 40,
                MAX_SMALL_OFFSET + 50,
            ],
            vec![
                (0, Some(MAX_SMALL_OFFSET + 10)),
                (MAX_SMALL_OFFSET + 10, Some(MAX_SMALL_OFFSET + 50)),
                (MAX_SMALL_OFFSET + 50, None),
            ],
            BASE_READ_AHEAD_SIZE / 4,
            BASE_READ_AHEAD_SIZE,
        );
    }

    #[fuchsia::test]
    fn test_compression_info_compressed_range_for_uncompressed_range_errors() {
        const MAX_SMALL_OFFSET: u64 = u32::MAX as u64;
        const CHUNK_SIZE: u64 = BASE_READ_AHEAD_SIZE;

        let compression_info = CompressionInfo::new(
            CHUNK_SIZE,
            vec![
                0,
                10,
                20,
                30,
                MAX_SMALL_OFFSET + 10,
                MAX_SMALL_OFFSET + 20,
                MAX_SMALL_OFFSET + 30,
                MAX_SMALL_OFFSET + 40,
                MAX_SMALL_OFFSET + 50,
            ],
        )
        .unwrap();

        // The start of reads must be chunk aligned.
        assert!(compression_info
            .compressed_range_for_uncompressed_range(&(1..BASE_READ_AHEAD_SIZE),)
            .is_err());

        // Reading entirely past the last offset isn't allowed.
        assert!(compression_info
            .compressed_range_for_uncompressed_range(
                &(BASE_READ_AHEAD_SIZE * 9..BASE_READ_AHEAD_SIZE * 12),
            )
            .is_err());

        // Reading a different amount than the read-ahead size isn't allowed for middle offsets.
        assert!(compression_info
            .compressed_range_for_uncompressed_range(&(0..BASE_READ_AHEAD_SIZE + 1),)
            .is_err());
        assert!(compression_info
            .compressed_range_for_uncompressed_range(&(0..BASE_READ_AHEAD_SIZE - 1),)
            .is_err());
        assert!(compression_info
            .compressed_range_for_uncompressed_range(
                &(BASE_READ_AHEAD_SIZE..BASE_READ_AHEAD_SIZE * 2 + 1),
            )
            .is_err());
        assert!(compression_info
            .compressed_range_for_uncompressed_range(
                &(BASE_READ_AHEAD_SIZE..BASE_READ_AHEAD_SIZE * 2 - 1),
            )
            .is_err());

        // Reading less than the read-ahead size for the last offset is allowed.
        assert!(compression_info
            .compressed_range_for_uncompressed_range(
                &(BASE_READ_AHEAD_SIZE * 8..BASE_READ_AHEAD_SIZE * 8 + 4096),
            )
            .is_ok());
    }

    #[fuchsia::test]
    fn test_read_ahead_size_for_chunk_size() {
        assert_eq!(read_ahead_size_for_chunk_size(32 * 1024, 32 * 1024), 32 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(48 * 1024, 32 * 1024), 48 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(64 * 1024, 32 * 1024), 64 * 1024);

        assert_eq!(read_ahead_size_for_chunk_size(32 * 1024, 64 * 1024), 64 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(48 * 1024, 64 * 1024), 48 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(64 * 1024, 64 * 1024), 64 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(96 * 1024, 64 * 1024), 96 * 1024);

        assert_eq!(read_ahead_size_for_chunk_size(32 * 1024, 128 * 1024), 128 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(48 * 1024, 128 * 1024), 96 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(64 * 1024, 128 * 1024), 128 * 1024);
        assert_eq!(read_ahead_size_for_chunk_size(96 * 1024, 128 * 1024), 96 * 1024);
    }

    #[fasync::run(10, test)]
    async fn test_refault_tracking() {
        async fn wait(condition: impl Fn() -> bool, error_msg: &'static str) {
            let mut wait_count = 0;
            while !condition() {
                fasync::Timer::new(Duration::from_millis(20)).await;
                wait_count += 1;
                if wait_count > 100 {
                    panic!("{}", error_msg);
                }
            }
        }

        fn get_chunks(chunks: &Vec<AtomicU8>) -> Vec<u8> {
            chunks.iter().map(|c| c.load(Ordering::Relaxed)).collect()
        }

        let fixture = new_blob_fixture().await;

        {
            let volume = fixture.volume().volume().clone();
            volume.start_background_task(
                MemoryPressureConfig::default(),
                fixture.volumes_directory().memory_pressure_monitor(),
            );

            let data = vec![0xffu8; 252 * 1024];
            let hash = fixture.write_blob(&data, CompressionMode::Never).await;

            let blob = fixture.get_blob(hash).await.unwrap();
            assert_eq!(blob.chunks_supplied.len(), 8);
            blob.vmo.read_to_vec(32 * 1024, 4096).unwrap();

            assert_eq!(&get_chunks(&blob.chunks_supplied), &[1, 1, 1, 1, 0, 0, 0, 0]);

            fixture
                .memory_pressure_proxy()
                .on_level_changed(MemoryPressureLevel::Critical)
                .await
                .expect("Failed to send memory pressure level change");

            wait(
                || volume.read_ahead_size() == BASE_READ_AHEAD_SIZE,
                "read-ahead size didn't change with memory pressure change",
            )
            .await;

            blob.vmo.read_to_vec(164 * 1024, 4096).unwrap();
            assert_eq!(&get_chunks(&blob.chunks_supplied), &[1, 1, 1, 1, 0, 1, 0, 0]);

            let epochs = Epochs::new();

            // We can't evict pages from the VMO so get the kernel to resupply them but we can call
            // page_in directly and wait for the counters to change.
            blob.clone().page_in(PageInRange::new(
                32 * 1024..36 * 1024,
                blob.clone(),
                epochs.add_ref(),
            ));
            wait(
                || blob.chunks_supplied[1].load(Ordering::Relaxed) == 2,
                "chunk was never supplied",
            )
            .await;
            assert_eq!(&get_chunks(&blob.chunks_supplied), &[1, 2, 1, 1, 0, 1, 0, 0]);
            assert_eq!(volume.refault_tracker().count(), 1);
            assert_eq!(volume.refault_tracker().bytes(), 32 * 1024);

            // Page in the last chunk and then do it again.
            blob.vmo.read_to_vec(224 * 1024, 4096).unwrap();
            blob.clone().page_in(PageInRange::new(
                224 * 1024..228 * 1024,
                blob.clone(),
                epochs.add_ref(),
            ));
            wait(
                || blob.chunks_supplied[7].load(Ordering::Relaxed) == 2,
                "chunk was never supplied",
            )
            .await;
            assert_eq!(&get_chunks(&blob.chunks_supplied), &[1, 2, 1, 1, 0, 1, 0, 2]);
            assert_eq!(volume.refault_tracker().count(), 2);
            // The last chunk is only 28KiB.
            assert_eq!(volume.refault_tracker().bytes(), 60 * 1024);
        }

        fixture.close().await;
    }
}
