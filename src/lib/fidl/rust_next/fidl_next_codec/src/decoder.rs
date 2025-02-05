// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The core [`Decoder`] trait.

use core::mem::take;

use crate::{Chunk, Decode, DecodeError, Owned, Slot, CHUNK_SIZE};

/// A decoder for FIDL handles (internal).
pub trait InternalHandleDecoder {
    /// Takes the next `count` handles from the decoder.
    ///
    /// This method exposes details about Fuchsia resources that plain old FIDL shouldn't need to
    /// know about. Do not use this method outside of this crate.
    #[doc(hidden)]
    fn __internal_take_handles(&mut self, count: usize) -> Result<(), DecodeError>;

    /// Returns the number of handles remaining in the decoder.
    ///
    /// This method exposes details about Fuchsia resources that plain old FIDL shouldn't need to
    /// know about. Do not use this method outside of this crate.
    #[doc(hidden)]
    fn __internal_handles_remaining(&self) -> usize;
}

/// A decoder for FIDL messages.
pub trait Decoder<'buf>: InternalHandleDecoder {
    /// Takes a slice of `Chunk`s from the decoder.
    ///
    /// Returns `Err` if the decoder doesn't have enough chunks left.
    fn take_chunks(&mut self, count: usize) -> Result<&'buf mut [Chunk], DecodeError>;

    /// Finishes decoding.
    ///
    /// Returns `Err` if the decoder did not finish successfully.
    fn finish(&mut self) -> Result<(), DecodeError>;
}

impl<T: InternalHandleDecoder> InternalHandleDecoder for &mut T {
    fn __internal_take_handles(&mut self, count: usize) -> Result<(), DecodeError> {
        T::__internal_take_handles(self, count)
    }

    fn __internal_handles_remaining(&self) -> usize {
        T::__internal_handles_remaining(self)
    }
}

impl<'buf, T: Decoder<'buf>> Decoder<'buf> for &mut T {
    fn take_chunks(&mut self, count: usize) -> Result<&'buf mut [Chunk], DecodeError> {
        T::take_chunks(self, count)
    }

    fn finish(&mut self) -> Result<(), DecodeError> {
        T::finish(self)
    }
}

impl InternalHandleDecoder for &mut [Chunk] {
    fn __internal_take_handles(&mut self, _: usize) -> Result<(), DecodeError> {
        Err(DecodeError::InsufficientHandles)
    }

    fn __internal_handles_remaining(&self) -> usize {
        0
    }
}

impl<'buf> Decoder<'buf> for &'buf mut [Chunk] {
    fn take_chunks(&mut self, count: usize) -> Result<&'buf mut [Chunk], DecodeError> {
        if count > self.len() {
            return Err(DecodeError::InsufficientData);
        }

        let chunks = take(self);
        let (prefix, suffix) = unsafe { chunks.split_at_mut_unchecked(count) };
        *self = suffix;
        Ok(prefix)
    }

    fn finish(&mut self) -> Result<(), DecodeError> {
        if !self.is_empty() {
            return Err(DecodeError::ExtraBytes { num_extra: self.len() * CHUNK_SIZE });
        }

        Ok(())
    }
}

/// Extension methods for [`Decoder`].
pub trait DecoderExt<'buf> {
    /// Takes enough chunks for a `T`, returning a `Slot` of the taken value.
    fn take_slot<T>(&mut self) -> Result<Slot<'buf, T>, DecodeError>;

    /// Takes enough chunks for a slice of `T`, returning a `Slot` of the taken slice.
    fn take_slice_slot<T>(&mut self, len: usize) -> Result<Slot<'buf, [T]>, DecodeError>;

    /// Decodes a `T` and returns an `Owned` pointer to it.
    ///
    /// Returns `Err` if decoding failed.
    fn decode_next<T: Decode<Self>>(&mut self) -> Result<Owned<'buf, T>, DecodeError>;

    /// Decodes a slice of `T` and returns an `Owned` pointer to it.
    ///
    /// Returns `Err` if decoding failed.
    fn decode_next_slice<T: Decode<Self>>(
        &mut self,
        len: usize,
    ) -> Result<Owned<'buf, [T]>, DecodeError>;

    /// Finishes the decoder by decoding a `T`.
    ///
    /// On success, returns `Ok` of an `Owned` pointer to the decoded value. Returns `Err` if the
    /// decoder did not finish successfully.
    fn decode_last<T: Decode<Self>>(&mut self) -> Result<Owned<'buf, T>, DecodeError>;

    /// Finishes the decoder by decoding a slice of `T`.
    ///
    /// On success, returns `Ok` of an `Owned` pointer to the decoded slice. Returns `Err` if the
    /// decoder did not finish successfully.
    fn decode_last_slice<T: Decode<Self>>(
        &mut self,
        len: usize,
    ) -> Result<Owned<'buf, [T]>, DecodeError>;
}

impl<'buf, D: Decoder<'buf> + ?Sized> DecoderExt<'buf> for D {
    fn take_slot<T>(&mut self) -> Result<Slot<'buf, T>, DecodeError> {
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

    fn take_slice_slot<T>(&mut self, len: usize) -> Result<Slot<'buf, [T]>, DecodeError> {
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

    fn decode_next<T: Decode<Self>>(&mut self) -> Result<Owned<'buf, T>, DecodeError> {
        let mut slot = self.take_slot::<T>()?;
        T::decode(slot.as_mut(), self)?;
        unsafe { Ok(Owned::new_unchecked(slot.as_mut_ptr())) }
    }

    fn decode_next_slice<T: Decode<Self>>(
        &mut self,
        len: usize,
    ) -> Result<Owned<'buf, [T]>, DecodeError> {
        let mut slot = self.take_slice_slot::<T>(len)?;
        for i in 0..len {
            T::decode(slot.index(i), self)?;
        }
        unsafe { Ok(Owned::new_unchecked(slot.as_mut_ptr())) }
    }

    fn decode_last<T: Decode<Self>>(&mut self) -> Result<Owned<'buf, T>, DecodeError> {
        let result = self.decode_next()?;
        self.finish()?;
        Ok(result)
    }

    fn decode_last_slice<T: Decode<Self>>(
        &mut self,
        len: usize,
    ) -> Result<Owned<'buf, [T]>, DecodeError> {
        let result = self.decode_next_slice(len)?;
        self.finish()?;
        Ok(result)
    }
}
