// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ptr::copy_nonoverlapping;
use core::slice::from_mut;

use super::{Encode, Error};
use crate::{Chunk, Handle, Slot, CHUNK_SIZE};

/// An encoder for FIDL messages.
#[derive(Default)]
pub struct Encoder {
    chunks: Vec<Chunk>,
    handles: Vec<Handle>,
}

impl Encoder {
    /// Returns a new encoder.
    pub fn new() -> Self {
        Self { chunks: Vec::new(), handles: Vec::new() }
    }

    /// Returns the number of bytes written to the encoder.
    pub fn byte_count(&self) -> usize {
        self.chunks.len() * CHUNK_SIZE
    }

    /// Returns the number of handles written to the encoder.
    pub fn handle_count(&self) -> usize {
        self.handles.len()
    }

    /// Writes a slice of chunks to the encoder.
    pub fn write_chunks(&mut self, chunks: &[Chunk]) {
        self.chunks.extend(chunks);
    }

    /// Writes a slice of bytes to the encoder.
    ///
    /// Additional bytes are written to pad the written data to a multiple of [`CHUNK_SIZE`].
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        let count = bytes.len().div_ceil(CHUNK_SIZE);
        self.chunks.reserve(count);

        let reserved = unsafe { self.chunks.as_mut_ptr().add(self.chunks.len()).cast::<u8>() };

        // Copy bytes into reserved region
        unsafe {
            bytes.as_ptr().copy_to_nonoverlapping(reserved, bytes.len());
        }

        // Zero out any trailing bytes
        let trailing = count * CHUNK_SIZE - bytes.len();
        unsafe {
            reserved.add(bytes.len()).write_bytes(0, trailing);
        }

        // Set the new length
        unsafe {
            self.chunks.set_len(self.chunks.len() + count);
        }
    }

    /// Pre-allocates space for a slice of elements.
    pub fn preallocate<T>(&mut self, len: usize) -> Preallocated<'_, T> {
        let start = self.chunks.len();
        let count = (size_of::<T>() * len).div_ceil(CHUNK_SIZE);

        // Zero out the next `count` chunks
        self.chunks.reserve(count);
        let rewrite_ptr = unsafe { self.chunks.as_mut_ptr().add(start) };
        unsafe {
            rewrite_ptr.write_bytes(0, count);
        }
        unsafe {
            self.chunks.set_len(start + count);
        }

        Preallocated {
            encoder: self,
            offset: start * CHUNK_SIZE,
            end: start * CHUNK_SIZE + len * size_of::<T>(),
            _phantom: PhantomData,
        }
    }

    /// Encodes a slice of elements.
    ///
    /// Returns `Err` if encoding failed.
    pub fn encode_slice<T: Encode>(&mut self, values: &mut [T]) -> Result<(), Error> {
        let mut slots = self.preallocate::<T::Encoded<'_>>(values.len());

        let mut backing = MaybeUninit::<T::Encoded<'_>>::uninit();
        for value in values {
            let mut slot = Slot::new(&mut backing);
            value.encode(slots.encoder, slot.as_mut())?;
            slots.write_next(slot);
        }

        Ok(())
    }

    /// Encodes a value.
    ///
    /// Returns `Err` if encoding failed.
    pub fn encode<T: Encode>(&mut self, value: &mut T) -> Result<(), Error> {
        self.encode_slice(from_mut(value))
    }

    /// Pushes a handle into the encoder.
    pub fn push_handle(&mut self, handle: Handle) {
        self.handles.push(handle);
    }

    /// Finishes encoding, returning the encoded chunks and handles.
    pub fn finish(self) -> (Vec<Chunk>, Vec<Handle>) {
        (self.chunks, self.handles)
    }
}

/// A pre-allocated slice of elements
pub struct Preallocated<'a, T> {
    /// The encoder.
    pub encoder: &'a mut Encoder,
    offset: usize,
    end: usize,
    _phantom: PhantomData<T>,
}

impl<'a, T> Preallocated<'a, T> {
    /// Writes into the next pre-allocated slot in the encoder.
    pub fn write_next(&mut self, mut slot: Slot<'_, T>) {
        assert_ne!(self.offset, self.end, "attemped to write more slots than preallocated",);

        unsafe {
            copy_nonoverlapping(
                slot.as_mut_ptr(),
                self.encoder.chunks.as_mut_ptr().cast::<u8>().add(self.offset).cast::<T>(),
                1,
            );
        }
        self.offset += size_of::<T>();
    }
}
