// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_hardware_audio as fhaudio;
use format_utils::Format;
use fuchsia_runtime::vmar_root_self;
use fuchsia_zircon::{self as zx};

// Wrapper for reading and writing to specific frames in audio driver ring buffers.

pub struct RingBuffer {
    vmo: zx::Vmo,
    base_address: usize,
    ring_buffer_proxy: fhaudio::RingBufferProxy,
    format: Format,
    pub num_frames: u64,
    pub driver_bytes: u64,
}

impl RingBuffer {
    pub async fn new(
        requested_format: &Format,
        ring_buffer_client: fhaudio::RingBufferProxy,
    ) -> Result<Self, Error> {
        let res = ring_buffer_client
            .get_vmo(
                requested_format.frames_per_second / 10,
                0, /* ring buffer notifications unused */
            )
            .await?;

        let (num_frames_in_rb, vmo) = match res {
            Ok((num_frames, vmo)) => (num_frames as u64, vmo),
            Err(e) => return Err(anyhow!("Couldn't receive vmo from ring buffer: {:?}", e)),
        };

        // Hardware might not use all bytes in vmo. Only want to use to frames hardware will read/write from.
        let bytes_in_rb = num_frames_in_rb as u64 * requested_format.bytes_per_frame() as u64;
        let bytes_in_vmo = vmo.get_size()?;

        if bytes_in_rb > bytes_in_vmo {
            println!("Bad ring buffer size returned by audio driver! \n (kernel size = {} bytes, driver size = {} bytes. ", bytes_in_vmo, bytes_in_rb);
            return Err(anyhow!("Bad ring buffer size returned by audio driver! \n (kernel size = {} bytes, driver size = {} bytes. ", bytes_in_vmo, bytes_in_rb));
        }

        let driver_bytes = ring_buffer_client
            .get_properties()
            .await?
            .driver_transfer_bytes
            .ok_or(anyhow!("driver transfer bytes unavailable"))? as u64;

        let base_address = vmar_root_self()
            .map(
                0,
                &vmo,
                0,
                vmo.get_size().map_err(|e| anyhow!("Failed to get vmo size: {}", e))? as usize,
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .map_err(|e| anyhow!("Failed to mmap vmo: {}", e))?;
        Ok(Self {
            vmo,
            base_address,
            ring_buffer_proxy: ring_buffer_client,
            num_frames: num_frames_in_rb,
            format: requested_format.to_owned(),
            driver_bytes,
        })
    }

    pub async fn stop(&self) -> Result<(), Error> {
        self.ring_buffer_proxy.stop().await?;
        Ok(())
    }

    pub async fn start(&self) -> Result<i64, Error> {
        let t_zero = self.ring_buffer_proxy.start().await?;
        Ok(t_zero)
    }

    pub fn write_to_frame(&self, frame: u64, buf: &mut Vec<u8>) -> Result<(), Error> {
        if buf.len() % self.format.bytes_per_frame() as usize != 0 {
            return Err(anyhow!("Must pass buffer with complete frames."));
        }
        let frame_offset = frame % self.num_frames;
        let byte_offset = frame_offset * self.format.bytes_per_frame() as u64;
        let num_frames_in_buf = buf.len() as u64 / self.format.bytes_per_frame() as u64;

        // Check whether buffer can be written continuously or needs to be split into
        // two writes, one to the end of the buffer and one starting from the beginning.
        if (frame_offset + num_frames_in_buf) <= self.num_frames {
            self.vmo.write(&buf[..], byte_offset)?;

            // Flush cache so that hardware reads most recent write.
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                let res = zx::sys::zx_cache_flush(
                    (self.base_address as u64 + byte_offset) as *mut u8,
                    buf.len(),
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                );
                if res != zx::sys::ZX_OK {
                    return Err(anyhow!("Call to flush cache failed with status {}.", res));
                }
            }
        } else {
            let frames_to_write_until_end = self.num_frames - frame_offset;
            let bytes_until_buffer_end =
                (frames_to_write_until_end * self.format.bytes_per_frame() as u64) as usize;

            self.vmo.write(&buf[..bytes_until_buffer_end], byte_offset)?;
            // Flush cache so that hardware reads most recent write.
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                let res = zx::sys::zx_cache_flush(
                    (self.base_address as u64 + byte_offset) as *mut u8,
                    bytes_until_buffer_end,
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                );
                if res != zx::sys::ZX_OK {
                    return Err(anyhow!("Call to flush cache failed with status {}.", res));
                }
            }

            // Write what remains to the beginning of the buffer.
            self.vmo.write(&buf[bytes_until_buffer_end..], 0)?;
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // we apply remainder operator of buffer size.
                let res = zx::sys::zx_cache_flush(
                    self.base_address as *mut u8,
                    buf.len() - bytes_until_buffer_end as usize,
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                );
                if res != zx::sys::ZX_OK {
                    return Err(anyhow!("Call to flush cache failed with status {}.", res));
                }
            }
        }
        Ok(())
    }
    pub fn read_from_frame(&self, frame: u64, buf: &mut [u8]) -> Result<(), Error> {
        if buf.len() % self.format.bytes_per_frame() as usize != 0 {
            return Err(anyhow!("Must pass buffer with complete frames."));
        }
        let frame_offset = frame % self.num_frames;
        let byte_offset = frame_offset * self.format.bytes_per_frame() as u64;
        let num_frames_in_buf = buf.len() as u64 / self.format.bytes_per_frame() as u64;

        // Check whether buffer can be read continuously or needs to be split into
        // two reads, one to the end of the buffer and one starting from the beginning.
        if (frame_offset + num_frames_in_buf) <= self.num_frames {
            // Flush cache so we read the hardware's most recent write.
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                let res = zx::sys::zx_cache_flush(
                    (self.base_address as u64 + byte_offset) as *mut u8,
                    buf.len(),
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                );
                if res != zx::sys::ZX_OK {
                    return Err(anyhow!("Call to flush cache failed with status {}.", res));
                }
            }
            self.vmo.read(buf, byte_offset)?;
        } else {
            let frames_to_write_until_end = self.num_frames - frame_offset;
            let bytes_until_buffer_end =
                (frames_to_write_until_end * self.format.bytes_per_frame() as u64) as usize;

            // Flush cache so we read the hardware's most recent write.
            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                let res = zx::sys::zx_cache_flush(
                    (self.base_address as u64 + byte_offset) as *mut u8,
                    bytes_until_buffer_end,
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                );
                if res != zx::sys::ZX_OK {
                    return Err(anyhow!("Call to flush cache failed with status {}.", res));
                }
            }
            self.vmo.read(&mut buf[..bytes_until_buffer_end], byte_offset)?;

            unsafe {
                // SAFETY: The flushed range is guaranteed to be in-bounds of the VMO since
                // frame_offset + num_frames_in_buf <= self.num_frames.
                let res = zx::sys::zx_cache_flush(
                    self.base_address as *mut u8,
                    buf.len() - bytes_until_buffer_end as usize,
                    zx::sys::ZX_CACHE_FLUSH_DATA,
                );
                if res != zx::sys::ZX_OK {
                    return Err(anyhow!("Call to flush cache failed with status {}.", res));
                }
            }
            self.vmo.read(&mut buf[bytes_until_buffer_end..], 0)?;
        }
        Ok(())
    }
}

impl Drop for RingBuffer {
    fn drop(&mut self) {
        // Safety:
        //
        // base_address is private to self, so no other code can observe that this mapping
        // has been removed.
        unsafe {
            vmar_root_self()
                .unmap(self.base_address, self.vmo.get_size().unwrap() as usize)
                .unwrap();
        }
    }
}
