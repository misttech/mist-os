// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use fidl_fuchsia_hardware_audio as fhaudio;
use fuchsia_audio::Format;
use fuchsia_runtime::vmar_root_self;
use fuchsia_zircon::{self as zx};
use thiserror::Error;

// TODO(b/317991807) Remove #[async_trait] when supported by compiler.
#[async_trait]
pub trait RingBuffer {
    /// Starts the ring buffer.
    ///
    /// Returns the CLOCK_MONOTONIC time at which it started.
    async fn start(&self) -> Result<zx::Time, anyhow::Error>;

    /// Stops the ring buffer.
    async fn stop(&self) -> Result<(), anyhow::Error>;

    /// Returns a references to the [VmoBuffer] that contains this ring buffer's data.
    fn vmo_buffer(&self) -> &VmoBuffer;

    /// Returns the number of bytes allocated to the producer.
    fn producer_bytes(&self) -> u64;

    /// Returns the number of bytes allocated to the consumer.
    fn consumer_bytes(&self) -> u64;
}

/// A [RingBuffer] backed by the `fuchsia.hardware.audio.RingBuffer` protocol.
pub struct HardwareRingBuffer {
    proxy: fhaudio::RingBufferProxy,
    pub vmo_buffer: VmoBuffer,
    pub driver_transfer_bytes: u64,
}

impl HardwareRingBuffer {
    pub async fn new(
        proxy: fhaudio::RingBufferProxy,
        format: Format,
    ) -> Result<Self, anyhow::Error> {
        // Request at least 100ms worth of frames.
        let min_frames = format.frames_per_second / 10;

        let (num_frames, vmo) = proxy
            .get_vmo(min_frames, 0 /* ring buffer notifications unused */)
            .await
            .context("Failed to call GetVmo")?
            .map_err(|e| anyhow!("Couldn't receive vmo from ring buffer: {:?}", e))?;

        let vmo_buffer = VmoBuffer::new(vmo, num_frames as u64, format)?;

        let driver_transfer_bytes = proxy
            .get_properties()
            .await?
            .driver_transfer_bytes
            .ok_or(anyhow::anyhow!("driver transfer bytes unavailable"))?
            as u64;

        Ok(Self { proxy, vmo_buffer, driver_transfer_bytes })
    }
}

#[async_trait]
impl RingBuffer for HardwareRingBuffer {
    async fn start(&self) -> Result<zx::Time, anyhow::Error> {
        let start_time = self.proxy.start().await?;
        Ok(zx::Time::from_nanos(start_time))
    }

    async fn stop(&self) -> Result<(), anyhow::Error> {
        self.proxy.stop().await?;
        Ok(())
    }

    fn vmo_buffer(&self) -> &VmoBuffer {
        &self.vmo_buffer
    }

    fn producer_bytes(&self) -> u64 {
        self.driver_transfer_bytes
    }

    fn consumer_bytes(&self) -> u64 {
        self.driver_transfer_bytes
    }
}

#[derive(Error, Debug)]
pub enum VmoBufferError {
    #[error("VMO is too small ({vmo_size_bytes} bytes) to hold ring buffer data ({data_size_bytes} bytes)")]
    VmoTooSmall { data_size_bytes: u64, vmo_size_bytes: u64 },

    #[error("Buffer size is invalid; contains incomplete frames")]
    BufferIncompleteFrames,

    #[error("Failed to memory map VMO: {}", .0)]
    VmoMap(#[source] zx::Status),

    #[error("Failed to get VMO size: {}", .0)]
    VmoGetSize(#[source] zx::Status),

    #[error("Failed to flush VMO memory cache: {}", .0)]
    VmoFlushCache(#[source] zx::Status),

    #[error("Failed to read from VMO: {}", .0)]
    VmoRead(#[source] zx::Status),

    #[error("Failed to write to VMO: {}", .0)]
    VmoWrite(#[source] zx::Status),
}

/// A VMO-backed ring buffer that contains frames of audio.
pub struct VmoBuffer {
    /// VMO that contains `num_frames` of audio in `format`.
    vmo: zx::Vmo,

    /// Size of the VMO, in bytes.
    vmo_size_bytes: u64,

    /// Number of frames in the VMO.
    num_frames: u64,

    /// Format of each frame.
    format: Format,

    /// Base address of the memory-mapped `vmo`.
    base_address: usize,
}

impl VmoBuffer {
    pub fn new(vmo: zx::Vmo, num_frames: u64, format: Format) -> Result<Self, VmoBufferError> {
        // Hardware might not use all bytes in vmo. Only want to use to frames hardware will read/write from.
        let data_size_bytes = num_frames * format.bytes_per_frame() as u64;
        let vmo_size_bytes = vmo
            .get_size()
            .map_err(|status| VmoBufferError::VmoGetSize(zx::Status::from(status)))?;

        if data_size_bytes > vmo_size_bytes {
            return Err(VmoBufferError::VmoTooSmall { data_size_bytes, vmo_size_bytes });
        }

        let base_address = vmar_root_self()
            .map(
                0,
                &vmo,
                0,
                vmo_size_bytes as usize,
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .map_err(|status| VmoBufferError::VmoMap(zx::Status::from(status)))?;

        Ok(Self { vmo, vmo_size_bytes, base_address, num_frames, format })
    }

    /// Returns the size of the buffer in bytes.
    ///
    /// This may be less than the size of the backing VMO.
    pub fn data_size_bytes(&self) -> u64 {
        self.num_frames * self.format.bytes_per_frame() as u64
    }

    /// Writes all frames from `buf` to the ring buffer at position `frame`.
    pub fn write_to_frame(&self, frame: u64, buf: &[u8]) -> Result<(), VmoBufferError> {
        if buf.len() % self.format.bytes_per_frame() as usize != 0 {
            return Err(VmoBufferError::BufferIncompleteFrames);
        }
        let frame_offset = frame % self.num_frames;
        let byte_offset = frame_offset as usize * self.format.bytes_per_frame() as usize;
        let num_frames_in_buf = buf.len() as u64 / self.format.bytes_per_frame() as u64;

        // Check whether buffer can be written continuously or needs to be split into
        // two writes, one to the end of the buffer and one starting from the beginning.
        if (frame_offset + num_frames_in_buf) <= self.num_frames {
            self.vmo.write(&buf[..], byte_offset as u64).map_err(VmoBufferError::VmoWrite)?;
            // Flush cache so that hardware reads most recent write.
            self.flush_cache(byte_offset, buf.len()).map_err(VmoBufferError::VmoFlushCache)?;
        } else {
            let frames_to_write_until_end = self.num_frames - frame_offset;
            let bytes_until_buffer_end =
                frames_to_write_until_end as usize * self.format.bytes_per_frame() as usize;

            self.vmo
                .write(&buf[..bytes_until_buffer_end], byte_offset as u64)
                .map_err(VmoBufferError::VmoWrite)?;
            // Flush cache so that hardware reads most recent write.
            self.flush_cache(byte_offset, bytes_until_buffer_end)
                .map_err(VmoBufferError::VmoFlushCache)?;

            // Write what remains to the beginning of the buffer.
            self.vmo.write(&buf[bytes_until_buffer_end..], 0).map_err(VmoBufferError::VmoWrite)?;
            self.flush_cache(0, buf.len() - bytes_until_buffer_end)
                .map_err(VmoBufferError::VmoFlushCache)?;
        }
        Ok(())
    }

    /// Reads frames from the ring buffer into `buf` starting at position `frame`.
    pub fn read_from_frame(&self, frame: u64, buf: &mut [u8]) -> Result<(), VmoBufferError> {
        if buf.len() % self.format.bytes_per_frame() as usize != 0 {
            return Err(VmoBufferError::BufferIncompleteFrames);
        }
        let frame_offset = frame % self.num_frames;
        let byte_offset = frame_offset as usize * self.format.bytes_per_frame() as usize;
        let num_frames_in_buf = buf.len() as u64 / self.format.bytes_per_frame() as u64;

        // Check whether buffer can be read continuously or needs to be split into
        // two reads, one to the end of the buffer and one starting from the beginning.
        if (frame_offset + num_frames_in_buf) <= self.num_frames {
            // Flush cache so we read the hardware's most recent write.
            self.flush_cache(byte_offset as usize, buf.len())
                .map_err(VmoBufferError::VmoFlushCache)?;
            self.vmo.read(buf, byte_offset as u64).map_err(VmoBufferError::VmoRead)?;
        } else {
            let frames_to_write_until_end = self.num_frames - frame_offset;
            let bytes_until_buffer_end =
                frames_to_write_until_end as usize * self.format.bytes_per_frame() as usize;

            // Flush cache so we read the hardware's most recent write.
            self.flush_cache(byte_offset, bytes_until_buffer_end)
                .map_err(VmoBufferError::VmoFlushCache)?;
            self.vmo
                .read(&mut buf[..bytes_until_buffer_end], byte_offset as u64)
                .map_err(VmoBufferError::VmoRead)?;

            self.flush_cache(0, buf.len() - bytes_until_buffer_end)
                .map_err(VmoBufferError::VmoFlushCache)?;
            self.vmo
                .read(&mut buf[bytes_until_buffer_end..], 0)
                .map_err(VmoBufferError::VmoRead)?;
        }
        Ok(())
    }

    /// Flush the cache for a portion of the memory mapped VMO.
    // TODO(https://fxbug.dev/328478694): Remove flush_cache once VMOs are created without caching
    fn flush_cache(&self, offset_bytes: usize, size_bytes: usize) -> Result<(), zx::Status> {
        assert!(offset_bytes + size_bytes <= self.vmo_size_bytes as usize);
        let status = unsafe {
            // SAFETY: The range was asserted above to be within the
            // mapped region of the VMO.
            zx::sys::zx_cache_flush(
                (self.base_address + offset_bytes) as *mut u8,
                size_bytes,
                zx::sys::ZX_CACHE_FLUSH_DATA,
            )
        };
        zx::Status::ok(status)
    }
}

impl Drop for VmoBuffer {
    fn drop(&mut self) {
        // SAFETY: `base_address` and `vmo_size_bytes` are private to self,
        // so no other code can observe that this mapping has been removed.
        unsafe {
            vmar_root_self().unmap(self.base_address, self.vmo_size_bytes as usize).unwrap();
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use fuchsia_audio::format::SampleType;

    #[test]
    fn vmobuffer_vmo_too_small() {
        let format =
            Format { frames_per_second: 48000, sample_type: SampleType::Uint8, channels: 2 };

        // VMO size is rounded up to the system page size.
        let page_size = zx::system_get_page_size() as u64;
        let num_frames = page_size + 1;
        let vmo = zx::Vmo::create(page_size).unwrap();

        assert_matches!(
            VmoBuffer::new(vmo, num_frames, format).err(),
            Some(VmoBufferError::VmoTooSmall { .. })
        )
    }

    #[test]
    fn vmobuffer_read_write() {
        let format =
            Format { frames_per_second: 48000, sample_type: SampleType::Uint8, channels: 2 };
        const NUM_FRAMES_VMO: u64 = 10;
        const NUM_FRAMES_BUF: u64 = 5;
        const SAMPLE: u8 = 42;

        let vmo_size = format.bytes_per_frame() as u64 * NUM_FRAMES_VMO;
        let buf_size = format.bytes_per_frame() as u64 * NUM_FRAMES_BUF;

        let vmo = zx::Vmo::create(vmo_size).unwrap();

        // Buffer used to read from the VmoBuffer.
        let mut in_buf = vec![0; buf_size as usize];
        // Buffer used to write to from the VmoBuffer.
        let out_buf = vec![SAMPLE; buf_size as usize];

        let vmo_buffer = VmoBuffer::new(vmo, NUM_FRAMES_VMO, format).unwrap();

        // Write the buffer to the VmoBuffer, starting on the second frame (zero based).
        vmo_buffer.write_to_frame(1, &out_buf).unwrap();

        // Read back from the VmoBuffer.
        vmo_buffer.read_from_frame(1, &mut in_buf).unwrap();

        assert_eq!(in_buf, out_buf);
    }

    #[test]
    fn vmobuffer_read_write_wrapping() {
        let format =
            Format { frames_per_second: 48000, sample_type: SampleType::Uint8, channels: 2 };
        let page_size = zx::system_get_page_size() as u64;
        let num_frames_vmo: u64 = page_size;
        let num_frames_buf: u64 = page_size / 2;
        const SAMPLE: u8 = 42;

        let vmo_size = format.bytes_per_frame() as u64 * num_frames_vmo;
        let buf_size = format.bytes_per_frame() as u64 * num_frames_buf;

        let vmo = zx::Vmo::create(vmo_size).unwrap();

        // Buffer used to read from the VmoBuffer.
        let mut in_buf = vec![0; buf_size as usize];
        // Buffer used to write to from the VmoBuffer.
        let out_buf = vec![SAMPLE; buf_size as usize];

        let vmo_buffer = VmoBuffer::new(vmo, num_frames_vmo, format).unwrap();

        // Write and read at the last frame to ensure the operations wrap to the beginning.
        let frame = num_frames_vmo - 1;
        assert!(frame + num_frames_buf > num_frames_vmo);

        // Write the buffer to the VmoBuffer, starting on the second frame (zero based).
        vmo_buffer.write_to_frame(frame, &out_buf).unwrap();

        // Read back from the VmoBuffer.
        vmo_buffer.read_from_frame(frame, &mut in_buf).unwrap();

        assert_eq!(in_buf, out_buf);
    }
}
