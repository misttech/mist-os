// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::mem::needs_drop;
use core::ops::Deref;
use core::ptr::NonNull;

use munge::munge;

use super::raw::RawWireVector;
use crate::{
    Decode, DecodeError, Decoder, DecoderExt as _, Encodable, Encode, EncodeError, Encoder,
    EncoderExt as _, Slot, TakeFrom, WirePointer,
};

/// A FIDL vector
#[repr(transparent)]
pub struct WireVector<T> {
    raw: RawWireVector<T>,
}

impl<T> Drop for WireVector<T> {
    fn drop(&mut self) {
        if needs_drop::<T>() {
            unsafe {
                self.raw.as_slice_ptr().drop_in_place();
            }
        }
    }
}

impl<T> WireVector<T> {
    /// Encodes that a vector is present in a slot.
    pub fn encode_present(slot: Slot<'_, Self>, len: u64) {
        munge!(let Self { raw } = slot);
        RawWireVector::encode_present(raw, len);
    }

    /// Returns the length of the vector in elements.
    pub fn len(&self) -> usize {
        self.raw.len() as usize
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

    /// Decodes a wire vector which contains raw data.
    ///
    /// # Safety
    ///
    /// The elements of the wire vecot rmust not need to be individually decoded, and must always be
    /// valid.
    pub unsafe fn decode_raw<D>(
        mut slot: Slot<'_, Self>,
        mut decoder: &mut D,
    ) -> Result<(), DecodeError>
    where
        D: Decoder + ?Sized,
        T: Decode<D>,
    {
        munge!(let Self { raw: RawWireVector { len, mut ptr } } = slot.as_mut());

        let len = len.to_native();
        if !WirePointer::is_encoded_present(ptr.as_mut())? {
            return Err(DecodeError::RequiredValueAbsent);
        }

        let mut slice = decoder.take_slice_slot::<T>(len as usize)?;
        WirePointer::set_decoded(ptr, slice.as_mut_ptr().cast());

        Ok(())
    }
}

impl<T> Deref for WireVector<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: fmt::Debug> fmt::Debug for WireVector<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

unsafe impl<D: Decoder + ?Sized, T: Decode<D>> Decode<D> for WireVector<T> {
    fn decode(mut slot: Slot<'_, Self>, mut decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { raw: RawWireVector { len, mut ptr } } = slot.as_mut());

        let len = len.to_native();
        if !WirePointer::is_encoded_present(ptr.as_mut())? {
            return Err(DecodeError::RequiredValueAbsent);
        }

        let slice = decoder.decode_next_slice::<T>(len as usize)?;
        WirePointer::set_decoded(ptr, slice.into_raw().cast());

        Ok(())
    }
}

impl<T: Encodable> Encodable for Vec<T> {
    type Encoded = WireVector<T::Encoded>;
}

impl<E: Encoder + ?Sized, T: Encode<E>> Encode<E> for Vec<T> {
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encoder.encode_next_slice(self.as_mut_slice())?;
        WireVector::encode_present(slot, self.len() as u64);
        Ok(())
    }
}

impl<T: TakeFrom<WT>, WT> TakeFrom<WireVector<WT>> for Vec<T> {
    fn take_from(from: &WireVector<WT>) -> Self {
        let mut result = Vec::with_capacity(from.len());
        for item in from.as_slice() {
            result.push(T::take_from(item));
        }
        result
    }
}
