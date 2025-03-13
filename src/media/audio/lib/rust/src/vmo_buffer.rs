// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Format;
use fuchsia_runtime::vmar_root_self;

use thiserror::Error;

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
        // Ensure that the VMO is big enough to hold `num_frames` of audio in the given `format`.
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
                // TODO(b/356700720): Don't try to map read-only VMOs with `PERM_WRITE`.
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .map_err(|status| VmoBufferError::VmoMap(zx::Status::from(status)))?;

        log::debug!(
            "format {:?} num frames {} data_size_bytes {}",
            format,
            num_frames,
            data_size_bytes
        );

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

        let frames_per_mili = self.format.frames_per_second / 1000;
        let mili_elapsed = frame / frames_per_mili as u64;
        let seconds = mili_elapsed as f64 / 1000.0;

        // Check whether the buffer can be written to contiguously or if the write needs to be
        // split into two: one until the end of the buffer and one starting from the beginning.
        if (frame_offset + num_frames_in_buf) <= self.num_frames {
            self.vmo.write(&buf[..], byte_offset as u64).map_err(VmoBufferError::VmoWrite)?;
            // Flush cache so that hardware reads most recent write.
            self.flush_cache(byte_offset, buf.len()).map_err(VmoBufferError::VmoFlushCache)?;
            log::debug!(
                "frame {} wrote starting from position {} Time {}s",
                frame,
                byte_offset,
                seconds
            );
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

            if buf[bytes_until_buffer_end..].len() > self.vmo_size_bytes as usize {
                log::error!("Remainder of write buffer is too big for the vmo.");
            }

            // Write what remains to the beginning of the buffer.
            self.vmo.write(&buf[bytes_until_buffer_end..], 0).map_err(VmoBufferError::VmoWrite)?;
            self.flush_cache(0, buf.len() - bytes_until_buffer_end)
                .map_err(VmoBufferError::VmoFlushCache)?;

            log::debug!(
                "frame {} wrote starting from position {}  (with looparound) Time {}s",
                frame,
                byte_offset,
                seconds
            );
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

        let frames_per_mili = self.format.frames_per_second / 1000; // 96
        let mili_elapsed = frame / frames_per_mili as u64;
        let seconds = mili_elapsed as f64 / 1000.0;

        // Check whether the buffer can be read from contiguously or if the read needs to be
        // split into two: one until the end of the buffer and one starting from the beginning.
        if (frame_offset + num_frames_in_buf) <= self.num_frames {
            // Flush and invalidate cache so we read the hardware's most recent write.
            log::debug!(
                "frame {} reading starting from position {} Time {}s",
                frame,
                byte_offset,
                seconds
            );
            self.flush_invalidate_cache(byte_offset as usize, buf.len())
                .map_err(VmoBufferError::VmoFlushCache)?;
            self.vmo.read(buf, byte_offset as u64).map_err(VmoBufferError::VmoRead)?;
        } else {
            let frames_to_write_until_end = self.num_frames - frame_offset;
            let bytes_until_buffer_end =
                frames_to_write_until_end as usize * self.format.bytes_per_frame() as usize;

            log::debug!(
                "frame {} reading starting from position {}  (with looparound) Time {}s",
                frame,
                byte_offset,
                seconds
            );
            // Flush and invalidate cache so we read the hardware's most recent write.
            self.flush_invalidate_cache(byte_offset, bytes_until_buffer_end)
                .map_err(VmoBufferError::VmoFlushCache)?;
            self.vmo
                .read(&mut buf[..bytes_until_buffer_end], byte_offset as u64)
                .map_err(VmoBufferError::VmoRead)?;

            if buf[bytes_until_buffer_end..].len() > self.vmo_size_bytes as usize {
                log::error!("Remainder of read buffer is too big for the vmo.");
            }

            self.flush_invalidate_cache(0, buf.len() - bytes_until_buffer_end)
                .map_err(VmoBufferError::VmoFlushCache)?;
            self.vmo
                .read(&mut buf[bytes_until_buffer_end..], 0)
                .map_err(VmoBufferError::VmoRead)?;
        }
        Ok(())
    }

    /// Flush the cache for a portion of the memory-mapped VMO.
    // TODO(https://fxbug.dev/328478694): Remove these methods once VMOs are created without caching
    fn flush_cache(&self, offset_bytes: usize, size_bytes: usize) -> Result<(), zx::Status> {
        assert!(offset_bytes + size_bytes <= self.vmo_size_bytes as usize);
        let status = unsafe {
            // SAFETY: The range was asserted above to be within the mapped region of the VMO.
            zx::sys::zx_cache_flush(
                (self.base_address + offset_bytes) as *mut u8,
                size_bytes,
                zx::sys::ZX_CACHE_FLUSH_DATA,
            )
        };
        zx::Status::ok(status)
    }

    /// Flush and invalidate cache for a portion of the memory-mapped VMO.
    // TODO(https://fxbug.dev/328478694): Remove these methods once VMOs are created without caching
    fn flush_invalidate_cache(
        &self,
        offset_bytes: usize,
        size_bytes: usize,
    ) -> Result<(), zx::Status> {
        assert!(offset_bytes + size_bytes <= self.vmo_size_bytes as usize);
        let status = unsafe {
            // SAFETY: The range was asserted above to be within the mapped region of the VMO.
            zx::sys::zx_cache_flush(
                (self.base_address + offset_bytes) as *mut u8,
                size_bytes,
                zx::sys::ZX_CACHE_FLUSH_DATA | zx::sys::ZX_CACHE_FLUSH_INVALIDATE,
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

#[cfg(test)]
mod test {
    use super::*;
    use crate::format::SampleType;
    use assert_matches::assert_matches;

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

        // Write the buffer to the VmoBuffer, starting on the last frame.
        vmo_buffer.write_to_frame(frame, &out_buf).unwrap();

        // Read back from the VmoBuffer.
        vmo_buffer.read_from_frame(frame, &mut in_buf).unwrap();

        assert_eq!(in_buf, out_buf);
    }
}
