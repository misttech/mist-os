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
use crate::fuchsia::volume::FxVolume;
use anyhow::{anyhow, ensure, Context, Error};
use fuchsia_hash::Hash;
use fuchsia_merkle::{hash_block, MerkleTree};
use futures::try_join;
use fxfs::errors::FxfsError;
use fxfs::object_handle::{ObjectHandle, ReadObjectHandle};
use fxfs::object_store::{DataObjectHandle, ObjectDescriptor};
use fxfs::round::{round_down, round_up};
use fxfs_macros::ToWeakNode;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use storage_device::buffer;
use zx::{self as zx, AsHandleRef, HandleBased, Status};

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
    merkle_tree: MerkleTree,
    compressed_chunk_size: u64,   // zero if blob is not compressed.
    compressed_offsets: Vec<u64>, // unused if blob is not compressed.
    uncompressed_size: u64,       // always set.
    pager_packet_receiver_registration: PagerPacketReceiverRegistration<Self>,
}

impl FxBlob {
    pub fn new(
        handle: DataObjectHandle<FxVolume>,
        merkle_tree: MerkleTree,
        compressed_chunk_size: u64,
        compressed_offsets: Vec<u64>,
        uncompressed_size: u64,
    ) -> Arc<Self> {
        let (vmo, pager_packet_receiver_registration) =
            handle.owner().pager().create_vmo(uncompressed_size, zx::VmoOptions::empty()).unwrap();
        let trimmed_merkle = &merkle_tree.root().to_string()[0..8];
        let name = format!("blob-{}", trimmed_merkle);
        let name = zx::Name::new(&name).unwrap();
        vmo.set_name(&name).unwrap();
        let file = Arc::new(Self {
            handle,
            vmo,
            open_count: AtomicUsize::new(0),
            merkle_tree,
            compressed_chunk_size,
            compressed_offsets,
            uncompressed_size,
            pager_packet_receiver_registration,
        });
        file.handle.owner().pager().register_file(&file);
        file
    }

    pub fn merkle_root(&self) -> Hash {
        self.merkle_tree.root()
    }

    /// Marks the blob as being purged.  Returns true if there are no open references.
    pub fn mark_to_be_purged(&self) -> bool {
        let mut old = self.open_count.load(Ordering::Relaxed);
        loop {
            assert_eq!(old & PURGED, 0);
            match self.open_count.compare_exchange_weak(
                old,
                old | PURGED,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return old == 0,
                Err(x) => old = x,
            }
        }
    }

    pub fn root(&self) -> Hash {
        self.merkle_tree.root()
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
        // Only allow read access to the VMO.
        // TODO(https://fxbug.dev/329429293): Remove when RFC-0238 is implemented.
        child_vmo.replace_handle(
            zx::Rights::BASIC | zx::Rights::MAP | zx::Rights::GET_PROPERTY | zx::Rights::READ,
        )
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
        // Delegate to the generic page handling code.
        default_page_in(self, range)
    }

    fn mark_dirty(self: Arc<Self>, _range: MarkDirtyRange<Self>) {
        unreachable!();
    }

    fn on_zero_children(self: Arc<Self>) {
        self.open_count_sub_one();
    }

    fn read_alignment(&self) -> u64 {
        if self.compressed_offsets.is_empty() {
            BLOCK_SIZE
        } else {
            self.compressed_chunk_size
        }
    }

    fn byte_size(&self) -> u64 {
        self.uncompressed_size
    }

    async fn aligned_read(&self, range: Range<u64>) -> Result<buffer::Buffer<'_>, Error> {
        thread_local! {
            static DECOMPRESSOR: std::cell::RefCell<zstd::bulk::Decompressor<'static>> =
                std::cell::RefCell::new(zstd::bulk::Decompressor::new().unwrap());
        }
        let block_alignment = self.read_alignment();
        ensure!(block_alignment > 0, FxfsError::Inconsistent);
        debug_assert_eq!(block_alignment % zx::system_get_page_size() as u64, 0);

        let mut buffer = self.handle.allocate_buffer((range.end - range.start) as usize).await;
        let read = if self.compressed_offsets.is_empty() {
            self.handle.read(range.start, buffer.as_mut()).await?
        } else {
            let indices = (range.start / block_alignment) as usize
                ..range.end.div_ceil(block_alignment) as usize;
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
                self.handle.allocate_buffer((aligned.end - aligned.start) as usize).await;
            let (read, _) =
                try_join!(self.handle.read(aligned.start, compressed_buf.as_mut()), async {
                    buffer
                        .allocator()
                        .buffer_source()
                        .commit_range(buffer.range())
                        .map_err(|e| e.into())
                })
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
            let len = (std::cmp::min(range.end, self.uncompressed_size) - range.start) as usize;
            let buf = buffer.as_mut_slice();
            let decompressed_size = DECOMPRESSOR
                .with(|decompressor| {
                    fxfs_trace::duration!(c"blob-decompress", "len" => len);
                    let mut decompressor = decompressor.borrow_mut();
                    decompressor.decompress_to_buffer(
                        &compressed_buf.as_slice()[compressed_buf_range],
                        &mut buf[..len],
                    )
                })
                .map_err(|_| FxfsError::IntegrityError)?;
            ensure!(decompressed_size == len, FxfsError::IntegrityError);
            len
        };
        // TODO(https://fxbug.dev/42073035): This should be offloaded to the kernel at which point we can
        // delete this.
        let hashes = &self.merkle_tree.as_ref()[0];
        let mut offset = range.start as usize;
        let bs = BLOCK_SIZE as usize;
        {
            fxfs_trace::duration!(c"blob-verify", "len" => read);
            for b in buffer.as_slice()[..read].chunks(bs) {
                ensure!(
                    hash_block(b, offset) == hashes[offset / bs],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuchsia::fxblob::testing::{new_blob_fixture, BlobFixture};
    use crate::fuchsia::pager::READ_AHEAD_SIZE;
    use assert_matches::assert_matches;
    use delivery_blob::CompressionMode;
    use fuchsia_async as fasync;

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

        let data = vec![0xffu8; (READ_AHEAD_SIZE + BLOCK_SIZE) as usize];
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
                .txn_write(&mut transaction, READ_AHEAD_SIZE, buf.as_ref())
                .await
                .expect("txn_write failed");
            transaction.commit().await.expect("failed to commit transaction");
        }

        {
            let blob_vmo = fixture.get_blob_vmo(hash).await;
            let mut buf = vec![0; BLOCK_SIZE as usize];
            assert_matches!(blob_vmo.read(&mut buf[..], 0), Ok(_));
            assert_matches!(
                blob_vmo.read(&mut buf[..], READ_AHEAD_SIZE),
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
}
