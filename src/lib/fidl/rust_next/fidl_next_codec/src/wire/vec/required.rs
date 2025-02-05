// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

use munge::munge;

use super::raw::RawWireVector;
use crate::{
    decode, encode, Decode, Decoder, Encodable, Encode, Encoder, EncoderExt as _, Slot, TakeFrom,
};

/// A FIDL vector
#[repr(transparent)]
pub struct WireVector<'buf, T> {
    raw: RawWireVector<'buf, T>,
}

impl<T> Drop for WireVector<'_, T> {
    fn drop(&mut self) {
        unsafe {
            self.raw.as_slice_ptr().drop_in_place();
        }
    }
}

impl<'buf, T> WireVector<'buf, T> {
    /// Creates a new `WireVector` from a `RawWireVector`.
    pub(super) unsafe fn new_unchecked(raw: RawWireVector<'buf, T>) -> Self {
        Self { raw }
    }

    /// Encodes that a vector is present in a slot.
    pub fn encode_present(slot: Slot<'_, Self>, len: u64) {
        munge!(let Self { raw } = slot);
        RawWireVector::encode_present(raw, len);
    }

    /// Returns the length of the vector in elements.
    pub fn len(&self) -> usize {
        self.raw.len().try_into().unwrap()
    }

    /// Returns whether the vector is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a pointer to the elements of the vector.
    fn as_slice_ptr(&self) -> NonNull<[T]> {
        unsafe { NonNull::new_unchecked(self.raw.as_slice_ptr()) }
    }

    /// Returns a slice of the elements of the vector.
    pub fn as_slice(&self) -> &[T] {
        unsafe { self.as_slice_ptr().as_ref() }
    }

    /// Returns a mutable slice of the elements of the vector.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { self.as_slice_ptr().as_mut() }
    }
}

impl<T> Default for WireVector<'_, T> {
    fn default() -> Self {
        Self { raw: RawWireVector::dangling() }
    }
}

impl<T> Deref for WireVector<'_, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> DerefMut for WireVector<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T: fmt::Debug> fmt::Debug for WireVector<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

unsafe impl<'buf, D: Decoder<'buf> + ?Sized, T: Decode<D>> Decode<D> for WireVector<'buf, T> {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), decode::DecodeError> {
        munge!(let Self { raw } = slot.as_mut());
        RawWireVector::decode(raw, decoder)?;

        let this = unsafe { slot.deref_unchecked() };
        if this.raw.as_ptr().is_null() {
            return Err(decode::DecodeError::RequiredValueAbsent);
        }

        Ok(())
    }
}

impl<T: Encodable> Encodable for Vec<T> {
    type Encoded<'buf> = WireVector<'buf, T::Encoded<'buf>>;
}

impl<E: Encoder + ?Sized, T: Encode<E>> Encode<E> for Vec<T> {
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), encode::EncodeError> {
        encoder.encode_next_slice(self.as_mut_slice())?;
        WireVector::encode_present(slot, self.len() as u64);
        Ok(())
    }
}

impl<T: TakeFrom<WT>, WT> TakeFrom<WireVector<'_, WT>> for Vec<T> {
    fn take_from(from: &mut WireVector<'_, WT>) -> Self {
        let mut result = Vec::with_capacity(from.len());
        for item in from.as_mut_slice() {
            result.push(T::take_from(item));
        }
        result
    }
}
