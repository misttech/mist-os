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
}

/// Trait implemented by an Inspect container that can be written to.
pub trait WritableBlockContainer: ReadableBlockContainer + ReadBytes + WriteBytes {
    /// Creates a block container from which it's possible to read and write.
    fn read_and_write(
        size: usize,
    ) -> Result<
        (Self, <Self as ReadableBlockContainer>::BackingStorage),
        <Self as ReadableBlockContainer>::Error,
    >;
}

pub trait WriteBytes {
    /// Writes the given `bytes` at the given `offset` in the container.
    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> usize;

    /// Writes the given `bytes` at offset 0 in the container.
    #[inline]
    fn write(&mut self, src: &[u8]) -> usize {
        self.write_at(0, src)
    }
}

/// Implemented to compare two inspect containers for equality, not for content equality, but just
/// whether or not they are backed by the same reference.
pub trait PtrEq<RHS = Self> {
    /// Returns true if the other container is the same.
    fn ptr_eq(&self, other: &RHS) -> bool;
}

impl ReadBytes for &[u8] {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        if offset >= self.len() {
            return;
        }
        let upper_bound = std::cmp::min(self.len(), dst.len() + offset);
        let bytes_read = upper_bound - offset;
        dst[..bytes_read].clone_from_slice(&self[offset..upper_bound]);
    }

    /// The number of bytes in the buffer.
    #[inline]
    fn len(&self) -> usize {
        <[u8]>::len(&self)
    }
}
