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
use fxfs::object_store::transaction::{lock_keys, LockKey};
use fxfs::object_store::volume::root_volume;
use fxfs::object_store::{DataObjectHandle, HandleOptions, ObjectStore, NO_OWNER};
use fxfs::round::round_up;
use std::ops::Range;
use std::sync::{Arc, Weak};
use storage_device::buffer::MutableBufferRef;
use storage_device::buffer_allocator::BufferFuture;
use storage_device::{Device, DeviceHolder};

const CHUNK_READ_SIZE: u64 = 131_072; /* 128 KiB */

// TODO(https://fxbug.dev/397515768): Use correct blob volume name. Right now this is just set to
// the same thing as we use for testing.
const BLOB_VOLUME_NAME: &str = "vol";

struct ReadOnlyDataObjectDevice {
    handle: DataObjectHandle<FxVolume>,
    block_count: u64,
}

impl ReadOnlyDataObjectDevice {
    async fn new(handle: DataObjectHandle<FxVolume>) -> Result<Self, Error> {
        let properties = handle.get_properties().await?;
        assert!(properties.allocated_size % handle.block_size() == 0);
        let block_count = properties.allocated_size / handle.block_size();
        Ok(Self { handle, block_count })
    }
}

#[async_trait]
impl Device for ReadOnlyDataObjectDevice {
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

async fn new_temporary_handle(
    blob_directory: &Arc<BlobDirectory>,
    size: u64,
) -> Result<DataObjectHandle<FxVolume>, Error> {
    let parent = blob_directory.directory().clone();
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
    // Pre-allocate enough space for the image.
    let mut range = 0..round_up(size, handle.block_size()).ok_or(FxfsError::OutOfRange)?;
    let mut first_time = true;
    while range.start < range.end {
        let mut transaction =
            handle.new_transaction().await.context("Failed to create transaction.")?;
        if first_time {
            handle
                .grow(&mut transaction, 0, size)
                .await
                .with_context(|| format!("Failed to grow handle to {} bytes.", size))?;
            first_time = false;
        }
        handle.preallocate_range(&mut transaction, &mut range).await.with_context(|| {
            format!("Failed to allocate range ({} to {}).", range.start, range.end)
        })?;
        transaction.commit().await.context("Failed to commit transaction.")?;
    }
    return Ok(handle);
}

async fn write_data(
    handle: &DataObjectHandle<FxVolume>,
    src: zx::Vmo,
    size: u64,
) -> Result<(), Error> {
    // Copy as much data out of the source VMO in multiples of the chunk read size.
    let chunk_size_aligned =
        round_up(CHUNK_READ_SIZE, handle.block_size()).ok_or(FxfsError::OutOfRange)? as usize;
    let mut buffer = handle.allocate_buffer(chunk_size_aligned).await;
    let mut remaining = size;
    while remaining >= CHUNK_READ_SIZE {
        let offset = size - remaining;
        src.read(buffer.as_mut_slice(), offset).context("Failed to read from VMO")?;
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
        let offset = size - remaining;
        let remaining = remaining as usize;
        src.read(&mut buffer.as_mut_slice()[..remaining], offset)
            .context("Failed to read from VMO")?;
        buffer.as_mut_slice()[remaining..].fill(0);
        handle
            .overwrite(offset, buffer, Default::default())
            .await
            .context("Failed to write data")?;
    }

    Ok(())
}

pub(crate) async fn write_new_blob_volume(
    existing: &Arc<BlobDirectory>,
    vmo: zx::Vmo,
    size: u64,
) -> Result<(), Error> {
    // TODO(https://fxbug.dev/397515768): Delete all existing blobs before writing the new image.

    // Write the image payload into a new, temporary object.
    log::info!("Creating new object to write image.");
    let handle = new_temporary_handle(existing, size).await?;
    log::info!("Streaming image to disk.");
    write_data(&handle, vmo, size).await?;

    log::info!("Mounting and verifying new image.");
    let device = DeviceHolder::new(ReadOnlyDataObjectDevice::new(handle).await?);
    let fs = FxFilesystemBuilder::new().read_only(true).open(device).await?;

    log::info!("Loading blob info.");
    {
        let root_volume = root_volume(fs.clone()).await?;
        let store = root_volume
            .volume(BLOB_VOLUME_NAME, NO_OWNER, None)
            .await
            .context("unable to open store for blob volume")?;
        let new_blob_vol = FxVolumeAndRoot::new::<BlobDirectory>(
            Weak::new(),
            store.clone(),
            store.store_object_id(),
        )
        .await?;
        let new_blob_dir = new_blob_vol
            .root()
            .clone()
            .into_any()
            .downcast::<BlobDirectory>()
            .expect("Root should be BlobDirectory");

        install_blobs(&new_blob_dir, existing).await?;

        // *WARNING*: We must ensure that we terminate the volume, otherwise some background tasks
        // may hold references to our nested data object backed device.
        new_blob_vol.into_volume().terminate().await;
    }

    Ok(())
}

/// Install the blobs from `new` into `existing`.
// TODO(https://fxbug.dev/397515768): This doesn't actually install anything right now, this is just
// a prototype that verifies the test blobs we expect to exist in both directories are there.
async fn install_blobs(
    source: &Arc<BlobDirectory>,
    dest: &Arc<BlobDirectory>,
) -> Result<(), Error> {
    let expected_existing = ["15ec7bf0b50732b49f8228e07d24365338f9e3ab994b00af08e5a3bffe55fd8b"];
    for hash in expected_existing {
        dest.lookup_blob(hash.try_into()?).await?;
    }
    let expected_new = [
        "9a24fe2fb8da617f39d303750bbe23f4e03a8b5f4d52bc90b2e5e9e44daddb3a",
        "1194c76d2d3b61f29df97a85ede7b2fd2b293b452f53072356e3c5c939c8131d",
    ];
    for hash in expected_new {
        source.lookup_blob(hash.try_into()?).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuchsia::fxblob::testing::{new_blob_fixture, open_blob_fixture, BlobFixture};
    use crate::fuchsia::testing::{TestFixture, TestFixtureOptions};
    use delivery_blob::CompressionMode;
    use fidl_fuchsia_fxfs::BlobVolumeWriterMarker;
    use fuchsia_component_client::connect_to_protocol_at_dir_svc;
    use fxfs::filesystem::SyncOptions;
    use ramdevice_client::RamdiskClientBuilder;
    use storage_device::block_device::BlockDevice;
    use zx::HandleBased as _;
    use {fidl_fuchsia_mem as fmem, fuchsia_async as fasync};

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

    async fn create_blob_image_vmo() -> (zx::Vmo, u64) {
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

        let fixture = TestFixture::open(
            device,
            TestFixtureOptions {
                as_blob: true,
                format: true,
                encrypted: false,
                ..Default::default()
            },
        )
        .await;

        for (original_hash, data) in TEST_BLOBS {
            let hash = fixture.write_blob(data, CompressionMode::Never).await;
            assert_eq!(hash.to_string(), original_hash);
        }

        fixture.fs().journal().compact().await.unwrap();
        fixture.fs().sync(SyncOptions { flush_device: true, ..Default::default() }).await.unwrap();
        // TODO(https://fxbug.dev/423696656): We should just call `close` here, but `BlockDevice`
        // cannot be reopened for running fsck.
        fixture.close_no_fsck().await;
        ramdisk.destroy_and_wait_for_removal().await.unwrap();

        // TODO(https://fxbug.dev/397515768): Only pass the bytes in the image that were used (i.e.
        // based on the maximum offset used by the allocator).
        (vmo, DEVICE_SIZE)
    }

    #[fasync::run(10, test)]
    async fn write_image() {
        // Create a new filesystem which contains the empty blob.
        let fixture = new_blob_fixture().await;
        let empty_blob_hash = fixture.write_blob(&[], CompressionMode::Never).await;
        assert_eq!(&fixture.read_blob(empty_blob_hash.clone()).await, &[]);

        // Generate a new fxfs filesystem in a VMO containing different blobs, and write it into
        // the existing filesystem using our protocol.
        {
            let (vmo, size) = create_blob_image_vmo().await;
            let writer =
                connect_to_protocol_at_dir_svc::<BlobVolumeWriterMarker>(fixture.volume_out_dir())
                    .unwrap();
            writer.write(fmem::Buffer { vmo, size }).await.unwrap().unwrap();
        }

        // Close the test fixture, and re-open it. For now we don't modify the device when we call
        // BlobVolumeWriter.Write, so we verify that the existing blobs are still there.
        let device = fixture.close().await;
        let fixture = open_blob_fixture(device).await;
        assert_eq!(&fixture.read_blob(empty_blob_hash.clone()).await, &[]);
        fixture.close().await;
    }
}
