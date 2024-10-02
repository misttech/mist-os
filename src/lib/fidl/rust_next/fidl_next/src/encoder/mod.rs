// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The core [`Encoder`] trait and a basic implementation of it.

mod basic;

pub use self::basic::*;

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::slice::from_mut;

use crate::encode::EncodeError;
use crate::{Encode, Slot};

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

/// Extension methods for [`Encoder`].
pub trait EncoderExt {
    /// Pre-allocates space for a slice of elements.
    fn preallocate<T>(&mut self, len: usize) -> Preallocated<'_, Self, T>;

    // TODO: rename to encode_next_slice and encode_next

    /// Encodes a slice of elements.
    ///
    /// Returns `Err` if encoding failed.
    fn encode_slice<T: Encode<Self>>(&mut self, values: &mut [T]) -> Result<(), EncodeError>;

    /// Encodes a value.
    ///
    /// Returns `Err` if encoding failed.
    fn encode<T: Encode<Self>>(&mut self, value: &mut T) -> Result<(), EncodeError>;
}

impl<E: Encoder + ?Sized> EncoderExt for E {
    fn preallocate<T>(&mut self, len: usize) -> Preallocated<'_, Self, T> {
        let pos = self.bytes_written();
        let len_bytes = len * size_of::<T>();

        // Zero out the next `count` bytes
        self.reserve(len_bytes);

        Preallocated { encoder: self, pos, end: pos + len_bytes, _phantom: PhantomData }
    }

    fn encode_slice<T: Encode<Self>>(&mut self, values: &mut [T]) -> Result<(), EncodeError> {
        let mut slots = self.preallocate::<T::Encoded<'_>>(values.len());

        let mut backing = MaybeUninit::<T::Encoded<'_>>::uninit();
        for value in values {
            let mut slot = Slot::new(&mut backing);
            value.encode(slots.encoder, slot.as_mut())?;
            slots.write_next(slot);
        }

        Ok(())
    }

    fn encode<T: Encode<Self>>(&mut self, value: &mut T) -> Result<(), EncodeError> {
        self.encode_slice(from_mut(value))
    }
}

/// A pre-allocated slice of elements
pub struct Preallocated<'a, E: ?Sized, T> {
    /// The encoder.
    pub encoder: &'a mut E,
    pos: usize,
    end: usize,
    _phantom: PhantomData<T>,
}

impl<'a, E: Encoder + ?Sized, T> Preallocated<'a, E, T> {
    /// Writes into the next pre-allocated slot in the encoder.
    pub fn write_next(&mut self, slot: Slot<'_, T>) {
        assert_ne!(self.pos, self.end, "attemped to write more slots than preallocated",);

        self.encoder.rewrite(self.pos, slot.as_bytes());
        self.pos += size_of::<T>();
    }
}
