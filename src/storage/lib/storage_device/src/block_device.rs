// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::buffer::{BufferFuture, BufferRef, MutableBufferRef};
use crate::buffer_allocator::{BufferAllocator, BufferSource};
use crate::Device;
use anyhow::{bail, ensure, Error};
use async_trait::async_trait;
use block_client::{BlockClient, BlockFlags, BufferSlice, MutableBufferSlice, VmoId};
use fuchsia_zircon::Status;
use std::ops::Range;

/// BlockDevice is an implementation of Device backed by a real block device behind a FIFO.
pub struct BlockDevice {
    allocator: BufferAllocator,
    remote: Box<dyn BlockClient>,
    read_only: bool,
    vmoid: VmoId,
}

const TRANSFER_VMO_SIZE: usize = 128 * 1024 * 1024;

impl BlockDevice {
    /// Creates a new BlockDevice over |remote|.
    pub async fn new(remote: Box<dyn BlockClient>, read_only: bool) -> Result<Self, Error> {
        let buffer_source = BufferSource::new(TRANSFER_VMO_SIZE);
        let vmoid = remote.attach_vmo(buffer_source.vmo()).await?;
        let allocator = BufferAllocator::new(remote.block_size() as usize, buffer_source);
        Ok(Self { allocator, remote, read_only, vmoid })
    }
}

#[async_trait]
impl Device for BlockDevice {
    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.allocator.allocate_buffer(size)
    }

    fn block_size(&self) -> u32 {
        self.remote.block_size()
    }

    fn block_count(&self) -> u64 {
        self.remote.block_count()
    }

    async fn read(&self, offset: u64, buffer: MutableBufferRef<'_>) -> Result<(), Error> {
        if buffer.len() == 0 {
            return Ok(());
        }
        ensure!(self.vmoid.is_valid(), "Device is closed");
        assert_eq!(offset % self.block_size() as u64, 0);
        assert_eq!(buffer.range().start % self.block_size() as usize, 0);
        assert_eq!((offset + buffer.len() as u64) % self.block_size() as u64, 0);
        self.remote
            .read_at(
                MutableBufferSlice::new_with_vmo_id(
                    &self.vmoid,
                    buffer.range().start as u64,
                    buffer.len() as u64,
                ),
                offset,
            )
            .await
    }

    async fn write(&self, offset: u64, buffer: BufferRef<'_>) -> Result<(), Error> {
        if self.read_only {
            bail!(Status::ACCESS_DENIED);
        }
        if buffer.len() == 0 {
            return Ok(());
        }
        ensure!(self.vmoid.is_valid(), "Device is closed");
        assert_eq!(offset % self.block_size() as u64, 0);
        assert_eq!(buffer.range().start % self.block_size() as usize, 0);
        assert_eq!((offset + buffer.len() as u64) % self.block_size() as u64, 0);
        self.remote
            .write_at(
                BufferSlice::new_with_vmo_id(
                    &self.vmoid,
                    buffer.range().start as u64,
                    buffer.len() as u64,
                ),
                offset,
            )
            .await
    }

    async fn trim(&self, range: Range<u64>) -> Result<(), Error> {
        if self.read_only {
            bail!(Status::ACCESS_DENIED);
        }
        assert_eq!(range.start % self.block_size() as u64, 0);
        assert_eq!(range.end % self.block_size() as u64, 0);
        self.remote.trim(range).await
    }

    async fn close(&self) -> Result<(), Error> {
        // We can leak the VMO id because we are closing the device.
        self.vmoid.take().into_id();
        self.remote.close().await
    }

    async fn flush(&self) -> Result<(), Error> {
        self.remote.flush().await
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn supports_trim(&self) -> bool {
        self.remote.block_flags().contains(BlockFlags::TRIM_SUPPORT)
    }
}

impl Drop for BlockDevice {
    fn drop(&mut self) {
        // We can't detach the VmoId because we're not async here, but we are tearing down the
        // connection to the block device so we don't really need to.
        self.vmoid.take().into_id();
    }
}

#[cfg(test)]
mod tests {
    use crate::block_device::BlockDevice;
    use crate::Device;
    use block_client::testing::FakeBlockClient;
    use fuchsia_zircon::Status;

    #[fuchsia::test]
    async fn test_lifecycle() {
        let device = BlockDevice::new(Box::new(FakeBlockClient::new(1024, 1024)), false)
            .await
            .expect("new failed");

        {
            let _buf = device.allocate_buffer(8192).await;
        }

        device.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_write_buffer() {
        let device = BlockDevice::new(Box::new(FakeBlockClient::new(1024, 1024)), false)
            .await
            .expect("new failed");

        {
            let mut buf1 = device.allocate_buffer(8192).await;
            let mut buf2 = device.allocate_buffer(1024).await;
            buf1.as_mut_slice().fill(0xaa as u8);
            buf2.as_mut_slice().fill(0xbb as u8);
            device.write(65536, buf1.as_ref()).await.expect("Write failed");
            device.write(65536 + 8192, buf2.as_ref()).await.expect("Write failed");
        }
        {
            let mut buf = device.allocate_buffer(8192 + 1024).await;
            device.read(65536, buf.as_mut()).await.expect("Read failed");
            assert_eq!(buf.as_slice()[..8192], vec![0xaa as u8; 8192]);
            assert_eq!(buf.as_slice()[8192..], vec![0xbb as u8; 1024]);
        }

        device.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_only() {
        let device = BlockDevice::new(Box::new(FakeBlockClient::new(1024, 1024)), true)
            .await
            .expect("new failed");
        let mut buf1 = device.allocate_buffer(8192).await;
        buf1.as_mut_slice().fill(0xaa as u8);
        let err = device.write(65536, buf1.as_ref()).await.expect_err("Write succeeded");
        assert_eq!(err.root_cause().downcast_ref::<Status>().unwrap(), &Status::ACCESS_DENIED);
    }
}
