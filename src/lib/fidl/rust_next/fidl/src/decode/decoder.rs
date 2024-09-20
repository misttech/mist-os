// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::take;
use std::collections::VecDeque;

use crate::decode::{Error, Owned};
use crate::{Chunk, Decode, Handle, Slot, CHUNK_SIZE};

/// A FIDL decoder.
pub struct Decoder<'buf> {
    chunks: &'buf mut [Chunk],
    handles: VecDeque<Handle>,
}

impl<'buf> Decoder<'buf> {
    /// Returns a new decoder for the given chunks and handles.
    pub fn new(chunks: &'buf mut [Chunk], handles: Vec<Handle>) -> Self {
        Self { chunks, handles: VecDeque::from(handles) }
    }

    /// Takes a slice of `Chunk`s from the decoder.
    ///
    /// Returns `Err` if the decoder doesn't have enough chunks left.
    pub fn take_chunks(&mut self, count: usize) -> Result<&'buf mut [Chunk], Error> {
        if count > self.chunks.len() {
            return Err(Error::InsufficientData);
        }

        let chunks = take(&mut self.chunks);
        let (prefix, suffix) = unsafe { chunks.split_at_mut_unchecked(count) };
        self.chunks = suffix;
        Ok(prefix)
    }

    /// Takes enough chunks for a `T`, returning a `Slot` of the taken value.
    pub fn take_slot<T>(&mut self) -> Result<Slot<'buf, T>, Error> {
        // TODO: might be able to move this into a const for guaranteed const
        // eval
        assert!(
            align_of::<T>() <= CHUNK_SIZE,
            "attempted to take a slot for a type with an alignment higher \
             than {}",
            CHUNK_SIZE,
        );

        let count = size_of::<T>().div_ceil(CHUNK_SIZE);
        let chunks = self.take_chunks(count)?;
        // SAFETY: `result` is at least 8-aligned and points to at least enough
        // bytes for a `T`.
        unsafe { Ok(Slot::new_unchecked(chunks.as_mut_ptr().cast())) }
    }

    /// Takes enough chunks for a slice of `T`, returning a `Slot` of the taken slice.
    pub fn take_slice_slot<T>(&mut self, len: usize) -> Result<Slot<'buf, [T]>, Error> {
        assert!(
            align_of::<T>() <= CHUNK_SIZE,
            "attempted to take a slice slot for a type with an alignment \
             higher than {}",
            CHUNK_SIZE,
        );

        let count = (size_of::<T>() * len).div_ceil(CHUNK_SIZE);
        let chunks = self.take_chunks(count)?;
        // SAFETY: `result` is at least 8-aligned and points to at least enough
        // bytes for a slice of `T` of length `len`.
        unsafe { Ok(Slot::new_slice_unchecked(chunks.as_mut_ptr().cast(), len)) }
    }

    /// Takes the next handle from the decoder.
    ///
    /// Returns `Err` if the decoder doesn't have any handles left.
    pub fn take_handle(&mut self) -> Result<Handle, Error> {
        self.handles.pop_front().ok_or(Error::InsufficientHandles)
    }

    /// Returns the number of handles remaining in the decoder.
    pub fn handles_remaining(&self) -> usize {
        self.handles.len()
    }

    /// Decodes a `T` and returns an `Owned` pointer to it.
    ///
    /// Returns `Err` if decoding failed.
    pub fn decode_next<T>(&mut self) -> Result<Owned<'buf, T>, Error>
    where
        T: Decode<'buf>,
    {
        let mut slot = self.take_slot::<T>()?;
        T::decode(slot.as_mut(), self)?;
        unsafe { Ok(Owned::new_unchecked(slot.as_mut_ptr())) }
    }

    /// Decodes a slice of `T` and returns an `Owned` pointer to it.
    ///
    /// Returns `Err` if decoding failed.
    pub fn decode_next_slice<T: Decode<'buf>>(
        &mut self,
        len: usize,
    ) -> Result<Owned<'buf, [T]>, Error> {
        let mut slot = self.take_slice_slot::<T>(len)?;
        for i in 0..len {
            T::decode(slot.index(i), self)?;
        }
        unsafe { Ok(Owned::new_unchecked(slot.as_mut_ptr())) }
    }

    /// Finishes decoding.
    ///
    /// Returns `Err` if not all chunks or handles were used.
    pub fn finish(self) -> Result<(), Error> {
        if !self.handles.is_empty() {
            return Err(Error::ExtraHandles { num_extra: self.handles.len() });
        }

        if !self.chunks.is_empty() {
            return Err(Error::ExtraBytes { num_extra: self.chunks.len() * CHUNK_SIZE });
        }

        Ok(())
    }

    /// Decodes the entirety of the decoder's data as a `T`.
    ///
    /// Returns `Err` if decoding failed or not all chunks or handles were used.
    pub fn decode<T: Decode<'buf>>(mut self) -> Result<Owned<'buf, T>, Error> {
        let owned = self.decode_next::<T>()?;
        self.finish()?;
        Ok(owned)
    }
}
