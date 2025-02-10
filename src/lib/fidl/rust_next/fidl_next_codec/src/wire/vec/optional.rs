// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;

use munge::munge;

use super::raw::RawWireVector;
use crate::{
    Decode, DecodeError, Decoder, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    Encoder, EncoderExt as _, Slot, TakeFrom, WireVector,
};

/// An optional FIDL vector
#[repr(transparent)]
pub struct WireOptionalVector<T> {
    raw: RawWireVector<T>,
}

impl<T> Drop for WireOptionalVector<T> {
    fn drop(&mut self) {
        if self.is_some() {
            unsafe {
                self.raw.as_slice_ptr().drop_in_place();
            }
        }
    }
}

impl<T> WireOptionalVector<T> {
    /// Encodes that a vector is present in a slot.
    pub fn encode_present(slot: Slot<'_, Self>, len: u64) {
        munge!(let Self { raw } = slot);
        RawWireVector::encode_present(raw, len);
    }

    /// Encodes that a vector is absent in a slot.
    pub fn encode_absent(slot: Slot<'_, Self>) {
        munge!(let Self { raw } = slot);
        RawWireVector::encode_absent(raw);
    }

    /// Returns whether the vector is present.
    pub fn is_some(&self) -> bool {
        !self.raw.as_ptr().is_null()
    }

    /// Returns whether the vector is absent.
    pub fn is_none(&self) -> bool {
        !self.is_some()
    }

    /// Gets a reference to the vector, if any.
    pub fn as_ref(&self) -> Option<&WireVector<T>> {
        if self.is_some() {
            Some(unsafe { &*(self as *const Self).cast() })
        } else {
            None
        }
    }

    /// Decodes a wire vector which contains raw data.
    ///
    /// # Safety
    ///
    /// The elements of the wire vecot rmust not need to be individually decoded, and must always be
    /// valid.
    pub unsafe fn decode_raw<D>(
        mut slot: Slot<'_, Self>,
        decoder: &mut D,
    ) -> Result<(), DecodeError>
    where
        D: Decoder + ?Sized,
        T: Decode<D>,
    {
        munge!(let Self { raw } = slot.as_mut());
        unsafe {
            RawWireVector::decode_raw(raw, decoder)?;
        }

        let this = unsafe { slot.deref_unchecked() };
        if this.raw.as_ptr().is_null() && this.raw.len() != 0 {
            return Err(DecodeError::InvalidOptionalSize(this.raw.len()));
        }

        Ok(())
    }
}

impl<T: fmt::Debug> fmt::Debug for WireOptionalVector<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<D: Decoder + ?Sized, T: Decode<D>> Decode<D> for WireOptionalVector<T> {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { raw } = slot.as_mut());
        RawWireVector::decode(raw, decoder)?;

        let this = unsafe { slot.deref_unchecked() };
        if this.raw.as_ptr().is_null() && this.raw.len() != 0 {
            return Err(DecodeError::InvalidOptionalSize(this.raw.len()));
        }

        Ok(())
    }
}

impl<T: Encodable> EncodableOption for Vec<T> {
    type EncodedOption = WireOptionalVector<T::Encoded>;
}

impl<E: Encoder + ?Sized, T: Encode<E>> EncodeOption<E> for Vec<T> {
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        slot: Slot<'_, Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        if let Some(vec) = this {
            encoder.encode_next_slice(vec.as_mut_slice())?;
            WireOptionalVector::encode_present(slot, vec.len() as u64);
        } else {
            WireOptionalVector::encode_absent(slot);
        }

        Ok(())
    }
}

impl<T: TakeFrom<WT>, WT> TakeFrom<WireOptionalVector<WT>> for Option<Vec<T>> {
    fn take_from(from: &WireOptionalVector<WT>) -> Self {
        from.as_ref().map(Vec::take_from)
    }
}
