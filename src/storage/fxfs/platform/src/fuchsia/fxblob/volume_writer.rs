// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the implementation of the BlobVolumeWriter protocol.
//! This allows a new blob volume to be streamed to disk.

use crate::fuchsia::fxblob::BlobDirectory;
use crate::fuchsia::node::FxNode as _;
use crate::fuchsia::volume::{FxVolume, FxVolumeAndRoot};
use anyhow::{Context as _, Error};
use async_trait::async_trait;
use fxfs::errors::FxfsError;
use fxfs::filesystem::FxFilesystemBuilder;
use fxfs::object_handle::{ObjectHandle as _, ReadObjectHandle as _};
use fxfs::object_store::directory::{replace_child_with_object, ReplacedChild};
use fxfs::object_store::transaction::{lock_keys, LockKey};
use fxfs::object_store::volume::root_volume;
use fxfs::object_store::{
    DataObjectHandle, HandleOptions, ObjectDescriptor, ObjectStore, Timestamp,
    BLOB_MERKLE_ATTRIBUTE_ID, NO_OWNER,
};
use fxfs::round::round_up;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::ops::Range;
use std::sync::{Arc, Weak};
use storage_device::buffer::MutableBufferRef;
use storage_device::buffer_allocator::BufferFuture;
use storage_device::{Device, DeviceHolder};
use vfs::directory::entry_container::MutableDirectory as _;

const CHUNK_READ_SIZE: u64 = 131_072; /* 128 KiB */

const BLOB_VOLUME_NAME: &str = "blob";

/// Helper type that implements a read-only [`Device`] for a [`DataObjectHandle`].
struct ReadOnlyDevice {
    handle: DataObjectHandle<FxVolume>,
    block_count: u64,
}

impl ReadOnlyDevice {
    async fn new(handle: DataObjectHandle<FxVolume>) -> Result<Self, Error> {
        let properties = handle.get_properties().await?;
        assert!(properties.allocated_size % handle.block_size() == 0);
        let block_count = properties.allocated_size / handle.block_size();
        Ok(Self { handle, block_count })
    }
}

#[async_trait]
impl Device for ReadOnlyDevice {
    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.handle.allocate_buffer(size)
    }

    fn block_size(&self) -> u32 {
        self.handle.block_size().try_into().unwrap()
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    async fn read(&self, offset: u64, buffer: MutableBufferRef<'_>) -> Result<(), Error> {
        self.handle.read(offset, buffer).await.map(|_| ())
    }

    async fn close(&self) -> Result<(), Error> {
        Ok(())
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn supports_trim(&self) -> bool {
        false
    }
    async fn write_with_opts(
        &self,
        _offset: u64,
        _buffer: storage_device::buffer::BufferRef<'_>,
        _opts: storage_device::WriteOptions,
    ) -> Result<(), Error> {
        unreachable!()
    }

    async fn flush(&self) -> Result<(), Error> {
        unreachable!()
    }
    async fn trim(&self, _range: Range<u64>) -> Result<(), Error> {
        unreachable!()
    }

    fn barrier(&self) {
        unreachable!()
    }
}

/// Creates and pre-allocates a new [`DataObjectHandle`] inside of `owner`. The object is added to
/// the graveyard upon construction so that it will be cleaned up on next mount if we crash.
async fn new_temporary_handle(
    owner: &Arc<BlobDirectory>,
) -> Result<DataObjectHandle<FxVolume>, Error> {
    let parent = owner.directory().clone();
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
    )
    .await
    .context("Failed to create object.")?;
    // Add the object to the graveyard so that it's cleaned up if we crash.
    store.add_to_graveyard(&mut transaction, handle.object_id());
    transaction.commit().await.context("Failed to commit transaction.")?;
    return Ok(handle);
}

async fn write_data(
    handle: &DataObjectHandle<FxVolume>,
    vmo: zx::Vmo,
    size: u64,
) -> Result<(), Error> {
    let mut reader = sparse::reader::SparseReader::new(VmoReader { vmo, size, offset: 0 })?;

    // Pre-allocate enough space for the image.
    let unsparsed_size = {
        let size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        size
    };
    let mut range =
        0..round_up(unsparsed_size, handle.block_size()).ok_or(FxfsError::OutOfRange)?;
    let mut first_time = true;
    while range.start < range.end {
        let mut transaction =
            handle.new_transaction().await.context("Failed to create transaction.")?;
        if first_time {
            handle
                .grow(&mut transaction, 0, unsparsed_size)
                .await
                .with_context(|| format!("Failed to grow handle to {} bytes.", unsparsed_size))?;
            first_time = false;
        }
        handle.preallocate_range(&mut transaction, &mut range).await.with_context(|| {
            format!("Failed to allocate range ({} to {}).", range.start, range.end)
        })?;
        transaction.commit().await.context("Failed to commit transaction.")?;
    }

    // Copy as much data out of the sparse image in multiples of the chunk read size.
    // TODO(https://fxbug.dev/397515768): When we generate a sparse fxblob image, there is usually
    // a trailing chunk of don't-care. The SparseReader correctly expands these to to all zeroes,
    // but we don't need to copy them nor write them to disk.

    let chunk_size_aligned =
        round_up(CHUNK_READ_SIZE, handle.block_size()).ok_or(FxfsError::OutOfRange)? as usize;
    let mut buffer = handle.allocate_buffer(chunk_size_aligned).await;
    let mut remaining = unsparsed_size;
    while remaining >= CHUNK_READ_SIZE {
        let amount =
            reader.read(buffer.as_mut_slice()).context("Failed to read from sparse image")?;
        if amount != buffer.len() {
            return Err(FxfsError::IntegrityError).context("Short read from sparse image");
        }
        let offset = unsparsed_size - remaining;
        handle
            .overwrite(offset, buffer.as_mut(), Default::default())
            .await
            .context("Failed to write data")?;
        remaining -= CHUNK_READ_SIZE;
    }
    // Copy any remaining data.
    if remaining > 0 {
        let remaining_aligned =
            round_up(remaining, handle.block_size()).ok_or(FxfsError::OutOfRange)? as usize;
        let mut buffer = buffer.subslice_mut(0..remaining_aligned);
        let offset = unsparsed_size - remaining;
        let remaining = remaining as usize;
        let amount = reader
            .read(&mut buffer.as_mut_slice()[..remaining])
            .context("Failed to read from sparse image")?;
        if amount != remaining {
            return Err(FxfsError::IntegrityError).context("Short read from sparse image");
        }
        buffer.as_mut_slice()[remaining..].fill(0);
        handle
            .overwrite(offset, buffer, Default::default())
            .await
            .context("Failed to write data")?;
    }

    Ok(())
}

pub(crate) async fn write_new_blob_volume(
    destination: &Arc<BlobDirectory>,
    vmo: zx::Vmo,
    size: u64,
) -> Result<(), Error> {
    log::info!("Deleting existing blobs.");
    delete_all_blobs(destination).await?;
    // Write the image payload into a new, temporary object.
    log::info!("Streaming image to disk.");
    let handle = new_temporary_handle(destination).await?;
    // Store the object ID of the handle so we can tombstone it when we're finished.
    let handle_id = handle.object_id();
    write_data(&handle, vmo, size).await?;

    {
        log::info!("Mounting image.");
        let device = DeviceHolder::new(ReadOnlyDevice::new(handle).await?);
        let fs = FxFilesystemBuilder::new().read_only(true).open(device).await?;
        let root_volume = root_volume(fs.clone()).await?;
        let store = root_volume
            .volume(BLOB_VOLUME_NAME, NO_OWNER, None)
            .await
            .context("unable to open store for blob volume")?;
        let volume = FxVolumeAndRoot::new::<BlobDirectory>(
            Weak::new(),
            store.clone(),
            store.store_object_id(),
        )
        .await?;
        let source = volume
            .root()
            .clone()
            .into_any()
            .downcast::<BlobDirectory>()
            .expect("Root should be BlobDirectory");

        // TODO(https://fxbug.dev/397515768): Instead of copying each blob, we should install them
        // in-place by creating new entries that reference the data already on disk.
        log::info!("Installing blobs.");
        copy_blobs(&source, destination).await?;

        // *WARNING*: We must ensure that we terminate the volume, otherwise some background tasks
        // may hold references to our nested data object backed device.
        volume.into_volume().terminate().await;
    }

    log::info!("Cleaning up.");
    // We should be finished with the temporary handle we used to mount the new image now, so we can
    // safely tombstone it here.
    let store = destination.directory().store();
    store.filesystem().graveyard().queue_tombstone_object(store.store_object_id(), handle_id);
    log::info!("Installation complete.");

    Ok(())
}

async fn delete_all_blobs(dir: &Arc<BlobDirectory>) -> Result<(), Error> {
    let store = dir.directory().store();
    let layer_set = store.tree().layer_set();
    let mut merger = layer_set.merger();
    let mut iter = dir.directory().directory().iter(&mut merger).await?;
    while let Some((name, _, _)) = iter.get() {
        dir.directory().clone().unlink(name, /*must_be_directory=*/ false).await?;
        iter.advance().await?;
    }
    Ok(())
}

/// Copies all blobs from `source` into `dest`.
async fn copy_blobs(source: &Arc<BlobDirectory>, dest: &Arc<BlobDirectory>) -> Result<(), Error> {
    let store = source.directory().store();
    let layer_set = store.tree().layer_set();
    let mut merger = layer_set.merger();
    let mut iter = source.directory().directory().iter(&mut merger).await?;
    while let Some((name, object_id, _)) = iter.get() {
        let object =
            ObjectStore::open_object(source.volume(), object_id, HandleOptions::default(), None)
                .await?;
        copy_blob(object, dest, name).await?;
        iter.advance().await?;
    }
    Ok(())
}

async fn copy_blob(
    source_object: DataObjectHandle<FxVolume>,
    dest: &Arc<BlobDirectory>,
    name: &str,
) -> Result<(), Error> {
    let merkle = source_object.read_attr(BLOB_MERKLE_ATTRIBUTE_ID).await?;
    // Create a new directory entry for the blob.
    let dest_directory = dest.directory();
    let dest_store = dest_directory.store();
    let dest_object;
    {
        let keys =
            lock_keys![LockKey::object(dest_store.store_object_id(), dest_directory.object_id())];
        let mut transaction = dest_store
            .filesystem()
            .clone()
            .new_transaction(keys, Default::default())
            .await
            .context("Failed to create transaction.")?;
        dest_object = ObjectStore::create_object(
            dest_directory.volume(),
            &mut transaction,
            // Checksums are redundant for blobs, which are already content-verified.
            HandleOptions { skip_checksums: true, ..Default::default() },
            None,
        )
        .await
        .context("Failed to create object.")?;
        // Add the object to the graveyard so that it's cleaned up if we crash.
        dest_store.add_to_graveyard(&mut transaction, dest_object.object_id());
        transaction.commit().await.context("Failed to commit transaction.")?;
    }
    // Allocate space for it.
    let size = source_object.get_size();
    let rounded_size = round_up(size, dest_object.block_size()).ok_or(FxfsError::OutOfRange)?;
    {
        let mut range = 0..rounded_size;
        let mut first_time = true;
        while range.start < range.end {
            let mut transaction =
                dest_object.new_transaction().await.context("Failed to create transaction.")?;
            if first_time {
                dest_object
                    .grow(&mut transaction, 0, size)
                    .await
                    .with_context(|| format!("Failed to grow handle to {} bytes.", size))?;
                first_time = false;
            }
            dest_object.preallocate_range(&mut transaction, &mut range).await.with_context(
                || format!("Failed to allocate range ({} to {}).", range.start, range.end),
            )?;
            transaction.commit().await.context("Failed to commit transaction.")?;
        }
    }
    // Copy the blob data to the new handle.
    // TODO(https://fxbug.dev/397515768): Limit buffer size.
    let mut buf = source_object.allocate_buffer(rounded_size as usize).await;
    source_object.read(0, buf.as_mut()).await?;
    dest_object.overwrite(0, buf.as_mut(), Default::default()).await?;

    // Copy metadata.
    if let Some(metadata) = merkle {
        dest_object.write_attr(BLOB_MERKLE_ATTRIBUTE_ID, &metadata).await?;
    }

    // Add it to the blob directory and remove from graveyard.
    let mut transaction = dest_directory
        .directory()
        .acquire_context_for_replace(None, &name, false)
        .await?
        .transaction;
    dest_object.store().remove_from_graveyard(&mut transaction, dest_object.object_id());

    let ReplacedChild::None = replace_child_with_object(
        &mut transaction,
        Some((dest_object.object_id(), ObjectDescriptor::File)),
        (dest_directory.directory(), &name),
        0,
        Timestamp::now(),
    )
    .await
    .context("Replacing child failed.")?
    else {
        return Err(FxfsError::Inconsistent.into());
    };

    transaction.commit().await?;

    Ok(())
}

struct VmoReader {
    vmo: zx::Vmo,
    size: u64,
    offset: u64,
}

// TODO(https://fxbug.dev/397515768): Propagate read errors, don't unwrap.
impl std::io::Read for VmoReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.offset >= self.size || buf.len() == 0 {
            return Ok(0);
        }
        let bytes_available = (self.size - self.offset) as usize;
        let bytes_read = std::cmp::min(buf.len(), bytes_available);
        self.vmo.read(&mut buf[..bytes_read], self.offset).unwrap();
        self.offset += bytes_read as u64;
        Ok(bytes_read)
    }
}

// TODO(https://fxbug.dev/397515768): Propagate seek errors, don't unwrap.
impl std::io::Seek for VmoReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.offset = match pos {
            SeekFrom::Current(offset) => self.offset.checked_add_signed(offset).unwrap(),
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => self.size.checked_add_signed(offset).unwrap(),
        };
        Ok(self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuchsia::fxblob::testing::{new_blob_fixture, open_blob_fixture, BlobFixture as _};
    use delivery_blob::CompressionMode;
    use fidl_fuchsia_fxfs::BlobVolumeWriterMarker;
    use fuchsia_component_client::connect_to_protocol_at_dir_svc;
    use fxfs_make_blob_image::FxBlobBuilder;
    use ramdevice_client::RamdiskClientBuilder;
    use storage_device::block_device::BlockDevice;
    use zx::HandleBased as _;
    use {fidl_fuchsia_mem as fmem, fuchsia_async as fasync};

    // Ensure the volume name from `make-blob-image` matches what we expect here.
    #[test]
    fn volume_name_matches() {
        assert_eq!(BLOB_VOLUME_NAME, fxfs_make_blob_image::BLOB_VOLUME_NAME);
    }

    const TEST_BLOBS: [(&str, &[u8]); 2] = [
        (
            "9a24fe2fb8da617f39d303750bbe23f4e03a8b5f4d52bc90b2e5e9e44daddb3a",
            "Goodbye, stranger!".as_bytes(),
        ),
        ("1194c76d2d3b61f29df97a85ede7b2fd2b293b452f53072356e3c5c939c8131d", &['a' as u8; 65_537]),
    ];
    const BLOCK_SIZE: u64 = 512;
    const NUM_BLOCKS: u64 = 8192;
    const DEVICE_SIZE: u64 = BLOCK_SIZE * NUM_BLOCKS;

    struct VmoWriter {
        vmo: zx::Vmo,
        size: u64,
        offset: u64,
        // TODO(https://fxbug.dev/397515768): SparseImageBuilder should be able to provide how many
        // bytes are required for the sparse output before it's actually built.
        max_offset: u64,
    }

    impl std::io::Seek for VmoWriter {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.offset = match pos {
                SeekFrom::Current(offset) => self.offset.checked_add_signed(offset).unwrap(),
                SeekFrom::Start(offset) => offset,
                SeekFrom::End(offset) => self.size.checked_add_signed(offset).unwrap(),
            };
            self.max_offset = std::cmp::max(self.offset, self.max_offset);
            Ok(self.offset)
        }
    }

    impl std::io::Write for VmoWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.offset >= self.size || buf.len() == 0 {
                return Ok(0);
            }
            let bytes_available = (self.size - self.offset) as usize;
            let bytes_written = std::cmp::min(buf.len(), bytes_available);
            self.vmo.write(&buf[..bytes_written], self.offset).unwrap();
            self.offset += bytes_written as u64;
            self.max_offset = std::cmp::max(self.offset, self.max_offset);
            Ok(bytes_written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    async fn create_sparse_fxblob_image() -> (zx::Vmo, u64) {
        // TODO(https://fxbug.dev/397515768): It would be preferable if we avoid using a ramdisk
        // here and instead used vmo_backed_block_server.
        let (fxblob_vmo, used_space) = {
            let vmo = zx::Vmo::create(DEVICE_SIZE).unwrap();
            let ramdisk = RamdiskClientBuilder::new_with_vmo(
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                Some(BLOCK_SIZE),
            )
            .build()
            .await
            .unwrap();
            let device = DeviceHolder::new(
                BlockDevice::new(
                    Box::new(
                        crate::component::new_block_client(
                            ramdisk.open().expect("Unable to open ramdisk"),
                        )
                        .await
                        .expect("Unable to create block client"),
                    ),
                    false,
                )
                .await
                .unwrap(),
            );
            let fxblob = FxBlobBuilder::new(device, /*compression_enabled*/ true).await.unwrap();
            for (hash, data) in TEST_BLOBS {
                let blob = fxblob.generate_blob(data.to_vec()).unwrap();
                assert_eq!(blob.hash(), hash.try_into().unwrap());
                fxblob.install_blob(&blob).await.unwrap();
            }

            let used_space = fxblob.finalize().await.unwrap();
            ramdisk.destroy_and_wait_for_removal().await.unwrap();
            (vmo, used_space)
        };

        let (sparse_vmo, sparse_size) = {
            // TODO(https://fxbug.dev/397515768): Use the correct size here once we can determine
            // how many bytes we need for the sparse output ahead of time.
            let mut sparse_writer = VmoWriter {
                vmo: zx::Vmo::create(DEVICE_SIZE).unwrap(),
                size: DEVICE_SIZE,
                offset: 0,
                max_offset: 0,
            };
            sparse::builder::SparseImageBuilder::new()
                .set_block_size(BLOCK_SIZE as u32)
                .add_chunk(sparse::builder::DataSource::Vmo {
                    vmo: fxblob_vmo,
                    size: used_space,
                    offset: 0,
                })
                .add_chunk(sparse::builder::DataSource::Skip(DEVICE_SIZE - used_space))
                .build(&mut sparse_writer)
                .unwrap();
            let VmoWriter { vmo, max_offset, .. } = sparse_writer;
            (vmo, max_offset)
        };

        (sparse_vmo, sparse_size)
    }

    #[fasync::run(10, test)]
    async fn write_image() {
        // Create a new filesystem which contains the empty blob.
        let fixture = new_blob_fixture().await;
        let empty_blob_hash = fixture.write_blob(&[], CompressionMode::Never).await;

        // Generate a new fxfs filesystem in a VMO containing different blobs, and write it into
        // the existing filesystem using our protocol.
        {
            let (vmo, size) = create_sparse_fxblob_image().await;
            let writer =
                connect_to_protocol_at_dir_svc::<BlobVolumeWriterMarker>(fixture.volume_out_dir())
                    .unwrap();
            writer.write(fmem::Buffer { vmo, size }).await.unwrap().unwrap();
        }

        // Close the test fixture and, re-open it.
        let device = fixture.close().await;
        let fixture = open_blob_fixture(device).await;

        // Verify that we find the new blobs we expect.
        for (hash, data) in TEST_BLOBS {
            let hash = hash.try_into().unwrap();
            assert_eq!(fixture.read_blob(hash).await.as_slice(), data);
        }

        // Ensure the empty blob was deleted.
        assert!(!fixture.blob_exists(empty_blob_hash).await);

        fixture.close().await;
    }
}
