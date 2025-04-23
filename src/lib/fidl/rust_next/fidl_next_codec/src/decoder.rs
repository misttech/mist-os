// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The core [`Decoder`] trait.

use core::mem::take;
use core::ptr::NonNull;
use core::slice;

use crate::{Chunk, Decode, DecodeError, Decoded, Owned, Slot, CHUNK_SIZE};

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
///
/// # Safety
///
/// Pointers returned from `take_chunks` must:
///
/// - Point to `count` initialized `Chunk`s
/// - Be valid for reads and writes
/// - Remain valid until the decoder is dropped
///
/// The decoder **may be moved** without invalidating the returned pointers.
pub unsafe trait Decoder: InternalHandleDecoder {
    /// Takes a slice of `Chunk`s from the decoder, returning a pointer to them.
    ///
    /// Returns `Err` if the decoder doesn't have enough chunks left.
    fn take_chunks_raw(&mut self, count: usize) -> Result<NonNull<Chunk>, DecodeError>;

    /// Commits to any decoding operations which are in progress.
    ///
    /// Resources like handles may be taken from a decoder during decoding. However, decoding may
    /// fail after those resources are taken but before decoding completes. To ensure that resources
    /// are always dropped, taken resources are still considered owned by the decoder until `commit`
    /// is called. After `commit`, ownership of those resources is transferred to the decoded data.
    fn commit(&mut self);

    /// Verifies that decoding finished cleanly, with no leftover chunks or resources.
    fn finish(&self) -> Result<(), DecodeError>;
}

impl InternalHandleDecoder for &mut [Chunk] {
    #[inline]
    fn __internal_take_handles(&mut self, _: usize) -> Result<(), DecodeError> {
        Err(DecodeError::InsufficientHandles)
    }

    #[inline]
    fn __internal_handles_remaining(&self) -> usize {
        0
    }
}

unsafe impl Decoder for &mut [Chunk] {
    #[inline]
    fn take_chunks_raw(&mut self, count: usize) -> Result<NonNull<Chunk>, DecodeError> {
        if count > self.len() {
            return Err(DecodeError::InsufficientData);
        }

        let chunks = take(self);
        let (prefix, suffix) = unsafe { chunks.split_at_mut_unchecked(count) };
        *self = suffix;
        unsafe { Ok(NonNull::new_unchecked(prefix.as_mut_ptr())) }
    }

    #[inline]
    fn commit(&mut self) {
        // No resources to take, so commit is a no-op
    }

    #[inline]
    fn finish(&self) -> Result<(), DecodeError> {
        if !self.is_empty() {
            return Err(DecodeError::ExtraBytes { num_extra: self.len() * CHUNK_SIZE });
        }

        Ok(())
    }
}

/// Extension methods for [`Decoder`].
pub trait DecoderExt {
    /// Takes a slice of `Chunk`s from the decoder.
    fn take_chunks<'buf>(
        self: &mut &'buf mut Self,
        count: usize,
    ) -> Result<&'buf mut [Chunk], DecodeError>;

    /// Takes enough chunks for a `T`, returning a `Slot` of the taken value.
    fn take_slot<'buf, T>(self: &mut &'buf mut Self) -> Result<Slot<'buf, T>, DecodeError>;

    /// Takes enough chunks for a slice of `T`, returning a `Slot` of the taken slice.
    fn take_slice_slot<'buf, T>(
        self: &mut &'buf mut Self,
        len: usize,
    ) -> Result<Slot<'buf, [T]>, DecodeError>;

    /// Decodes an `Owned` value from the decoder without finishing it.
    ///
    /// On success, returns `Ok` of an `Owned` value. Returns `Err` if decoding failed.
    fn decode_prefix<'buf, T: Decode<Self>>(
        self: &mut &'buf mut Self,
    ) -> Result<Owned<'buf, T>, DecodeError>;

    /// Decodes an `Owned` slice from the decoder without finishing it.
    ///
    /// On success, returns `Ok` of an `Owned` slice. Returns `Err` if decoding failed.
    fn decode_slice_prefix<'buf, T: Decode<Self>>(
        self: &mut &'buf mut Self,
        len: usize,
    ) -> Result<Owned<'buf, [T]>, DecodeError>;

    /// Decodes a value from the decoder and finishes it.
    ///
    /// On success, returns `Ok` of a `Decoded` value with the decoder. Returns `Err` if decoding
    /// failed or the decoder finished with an error.
    fn decode<T>(self) -> Result<Decoded<T, Self>, DecodeError>
    where
        T: Decode<Self>,
        Self: Sized;

    /// Decodes a slice from the decoder and finishes it.
    ///
    /// On success, returns `Ok` of a `Decoded` slice with the decoder. Returns `Err` if decoding
    /// failed or the decoder finished with an error.
    fn decode_slice<T>(self, len: usize) -> Result<Decoded<[T], Self>, DecodeError>
    where
        T: Decode<Self>,
        Self: Sized;
}

impl<D: Decoder + ?Sized> DecoderExt for D {
    fn take_chunks<'buf>(
        self: &mut &'buf mut Self,
        count: usize,
    ) -> Result<&'buf mut [Chunk], DecodeError> {
        self.take_chunks_raw(count).map(|p| unsafe { slice::from_raw_parts_mut(p.as_ptr(), count) })
    }

    fn take_slot<'buf, T>(self: &mut &'buf mut Self) -> Result<Slot<'buf, T>, DecodeError> {
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

    fn take_slice_slot<'buf, T>(
        self: &mut &'buf mut Self,
        len: usize,
    ) -> Result<Slot<'buf, [T]>, DecodeError> {
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

    fn decode_prefix<'buf, T: Decode<Self>>(
        self: &mut &'buf mut Self,
    ) -> Result<Owned<'buf, T>, DecodeError> {
        let mut slot = self.take_slot::<T>()?;
        T::decode(slot.as_mut(), self)?;
        self.commit();
        // SAFETY: `slot` decoded successfully and the decoder was committed. `slot` now points to a
        // valid `T` within the decoder.
        unsafe { Ok(Owned::new_unchecked(slot.as_mut_ptr())) }
    }

    fn decode_slice_prefix<'buf, T: Decode<Self>>(
        self: &mut &'buf mut Self,
        len: usize,
    ) -> Result<Owned<'buf, [T]>, DecodeError> {
        let mut slot = self.take_slice_slot::<T>(len)?;
        for i in 0..len {
            T::decode(slot.index(i), self)?;
        }
        self.commit();
        // SAFETY: `slot` decoded successfully and the decoder was committed. `slot` now points to a
        // valid `[T]` within the decoder.
        unsafe { Ok(Owned::new_unchecked(slot.as_mut_ptr())) }
    }

    fn decode<T>(mut self) -> Result<Decoded<T, Self>, DecodeError>
    where
        T: Decode<Self>,
        Self: Sized,
    {
        let mut decoder = &mut self;
        let result = decoder.decode_prefix::<T>()?;
        decoder.finish()?;
        // SAFETY: `result` points to an owned `T` contained within `decoder`.
        unsafe { Ok(Decoded::new_unchecked(result.into_raw(), self)) }
    }

    fn decode_slice<T: Decode<Self>>(
        mut self,
        len: usize,
    ) -> Result<Decoded<[T], Self>, DecodeError>
    where
        Self: Sized,
    {
        let mut decoder = &mut self;
        let result = decoder.decode_slice_prefix::<T>(len)?;
        decoder.finish()?;
        // SAFETY: `result` points to an owned `[T]` contained within `decoder`.
        unsafe { Ok(Decoded::new_unchecked(result.into_raw(), self)) }
    }
}
