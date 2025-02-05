// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The core [`Encoder`] trait.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::slice::from_mut;

use crate::encode::EncodeError;
use crate::{Chunk, Encode, Slot, CHUNK_SIZE};

/// An encoder for FIDL messages.
pub trait Encoder {
    /// Returns the number of bytes written to the encoder.
    fn bytes_written(&self) -> usize;

    /// Writes zeroed bytes to the end of the encoder.
    ///
    /// Additional bytes are written to pad the written data to a multiple of [`CHUNK_SIZE`].
    fn reserve(&mut self, len: usize);

    /// Copies bytes to the end of the encoder.
    ///
    /// Additional bytes are written to pad the written data to a multiple of [`CHUNK_SIZE`].
    fn write(&mut self, bytes: &[u8]);

    /// Rewrites bytes at a position in the encoder.
    fn rewrite(&mut self, pos: usize, bytes: &[u8]);

    /// Returns the number of handles written to the encoder.
    ///
    /// This method exposes details about Fuchsia resources that plain old FIDL shouldn't need to
    /// know about. Do not use this method outside of this crate.
    #[doc(hidden)]
    fn __internal_handle_count(&self) -> usize;
}

impl<T: Encoder> Encoder for &mut T {
    fn bytes_written(&self) -> usize {
        T::bytes_written(self)
    }

    fn reserve(&mut self, len: usize) {
        T::reserve(self, len)
    }

    fn write(&mut self, bytes: &[u8]) {
        T::write(self, bytes)
    }

    fn rewrite(&mut self, pos: usize, bytes: &[u8]) {
        T::rewrite(self, pos, bytes)
    }

    fn __internal_handle_count(&self) -> usize {
        T::__internal_handle_count(self)
    }
}

impl Encoder for Vec<Chunk> {
    fn bytes_written(&self) -> usize {
        self.len() * CHUNK_SIZE
    }

    fn reserve(&mut self, len: usize) {
        let count = len.div_ceil(CHUNK_SIZE);
        self.reserve(count);
        let ptr = unsafe { self.as_mut_ptr().add(self.len()) };
        unsafe {
            ptr.write_bytes(0, count);
        }
        unsafe {
            self.set_len(self.len() + count);
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let count = bytes.len().div_ceil(CHUNK_SIZE);
        self.reserve(count);
        let ptr = unsafe { self.as_mut_ptr().add(self.len()).cast::<u8>() };

        // Copy all the bytes
        unsafe {
            ptr.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        }

        // Zero out any trailing bytes
        let trailing = count * CHUNK_SIZE - bytes.len();
        unsafe {
            ptr.add(bytes.len()).write_bytes(0, trailing);
        }

        // Set the new length
        unsafe {
            self.set_len(self.len() + count);
        }
    }

    fn rewrite(&mut self, pos: usize, bytes: &[u8]) {
        assert!(pos + bytes.len() <= self.bytes_written());

        let ptr = unsafe { self.as_mut_ptr().cast::<u8>().add(pos) };
        unsafe {
            ptr.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        }
    }

    fn __internal_handle_count(&self) -> usize {
        0
    }
}

/// Extension methods for [`Encoder`].
pub trait EncoderExt {
    /// Pre-allocates space for a slice of elements.
    fn preallocate<T>(&mut self, len: usize) -> Preallocated<'_, Self, T>;

    /// Encodes a slice of elements.
    ///
    /// Returns `Err` if encoding failed.
    fn encode_next_slice<T: Encode<Self>>(&mut self, values: &mut [T]) -> Result<(), EncodeError>;

    /// Encodes a value.
    ///
    /// Returns `Err` if encoding failed.
    fn encode_next<T: Encode<Self>>(&mut self, value: &mut T) -> Result<(), EncodeError>;
}

impl<E: Encoder + ?Sized> EncoderExt for E {
    fn preallocate<T>(&mut self, len: usize) -> Preallocated<'_, Self, T> {
        let pos = self.bytes_written();

        // Zero out the next `count` bytes
        self.reserve(len * size_of::<T>());

        Preallocated { encoder: self, pos, remaining: len, _phantom: PhantomData }
    }

    fn encode_next_slice<T: Encode<Self>>(&mut self, values: &mut [T]) -> Result<(), EncodeError> {
        let mut slots = self.preallocate::<T::Encoded<'_>>(values.len());

        let mut backing = MaybeUninit::<T::Encoded<'_>>::uninit();
        for value in values {
            let mut slot = Slot::new(&mut backing);
            value.encode(slots.encoder, slot.as_mut())?;
            slots.write_next(slot);
        }

        Ok(())
    }

    fn encode_next<T: Encode<Self>>(&mut self, value: &mut T) -> Result<(), EncodeError> {
        self.encode_next_slice(from_mut(value))
    }
}

/// A pre-allocated slice of elements
pub struct Preallocated<'a, E: ?Sized, T> {
    /// The encoder.
    pub encoder: &'a mut E,
    pos: usize,
    remaining: usize,
    _phantom: PhantomData<T>,
}

impl<E: Encoder + ?Sized, T> Preallocated<'_, E, T> {
    /// Writes into the next pre-allocated slot in the encoder.
    pub fn write_next(&mut self, slot: Slot<'_, T>) {
        assert!(self.remaining > 0, "attemped to write more slots than preallocated");

        self.encoder.rewrite(self.pos, slot.as_bytes());
        self.pos += size_of::<T>();
        self.remaining -= 1;
    }
}
