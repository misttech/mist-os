// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Trait implemented by an Inspect container that can be read from.
pub trait ReadableBlockContainer: ReadBytes + Sized {
    type BackingStorage: ReadBytes;
    type Error: std::fmt::Debug;

    /// Creates a read-only inspect container using the given backing buffer for storage.
    fn read_only(storage: &Self::BackingStorage) -> Result<Self, Self::Error>
    where
        Self: Sized;
}

/// Trait implemented by an Inspect container that cane bread from.
pub trait ReadBytes {
    /// Reads N bytes at the given offset and stores them in dst where N is the size of dst.
    fn read_at(&self, offset: usize, dst: &mut [u8]);

    /// Returns the size of the container.
    fn len(&self) -> usize;

    /// Read bytes from the buffer at an offset 0.
    #[inline]
    fn read(&self, dst: &mut [u8]) {
        self.read_at(0, dst);
    }

    #[inline]
    fn get_u64(&self, offset: usize) -> Option<u64> {
        if offset + 8 > self.len() {
            return None;
        }
        let mut dst = [0u8; 8];
        self.read_at(offset, &mut dst);
        Some(u64::from_le_bytes(dst))
    }
}

/// Trait implemented by container to which bytes can be written.
// TODO(miguelfrde): in a follow-up allow WriteBytes to not depend on ReadBytes.
pub trait WriteBytes: ReadBytes {
    /// Writes the given `bytes` at the given `offset` in the container.
    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> usize;

    /// Writes the given `bytes` at offset 0 in the container.
    #[inline]
    fn write(&mut self, src: &[u8]) -> usize {
        self.write_at(0, src)
    }

    #[inline]
    fn with_u64_mut<F>(&mut self, offset: usize, cb: F)
    where
        F: FnOnce(&mut u64) -> (),
    {
        if let Some(mut value) = self.get_u64(offset) {
            cb(&mut value);
            self.write_at(offset, &value.to_le_bytes());
        }
    }
}

/// Trait implemented by an Inspect container that can be written to.
pub trait WritableBlockContainer: ReadableBlockContainer + WriteBytes {
    /// Creates a block container from which it's possible to read and write.
    fn read_and_write(
        size: usize,
    ) -> Result<
        (Self, <Self as ReadableBlockContainer>::BackingStorage),
        <Self as ReadableBlockContainer>::Error,
    >;
}

impl ReadBytes for Vec<u8> {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        self.as_slice().read_at(offset, dst)
    }

    /// The number of bytes in the buffer.
    #[inline]
    fn len(&self) -> usize {
        self.as_slice().len()
    }
}

impl ReadBytes for [u8] {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        if offset >= self.len() {
            return;
        }
        let upper_bound = std::cmp::min(self.len(), dst.len() + offset);
        let bytes_read = upper_bound - offset;
        dst[..bytes_read].copy_from_slice(&self[offset..upper_bound]);
    }

    /// The number of bytes in the buffer.
    #[inline]
    fn len(&self) -> usize {
        <[u8]>::len(&self)
    }
}

impl<const N: usize> ReadBytes for [u8; N] {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        self.as_slice().read_at(offset, dst)
    }

    /// The number of bytes in the buffer.
    #[inline]
    fn len(&self) -> usize {
        self.as_slice().len()
    }
}

/// Trait implemented by an Inspect container that can be written to.
impl<const N: usize> WriteBytes for [u8; N] {
    #[inline]
    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> usize {
        if offset >= self.len() {
            return 0;
        }
        let upper_bound = std::cmp::min(self.len(), bytes.len() + offset);
        let bytes_written = upper_bound - offset;
        self[offset..upper_bound].copy_from_slice(&bytes[..bytes_written]);
        bytes_written
    }
}
