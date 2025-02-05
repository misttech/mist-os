// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::mem::replace;

use munge::munge;

use super::raw::RawWireVector;
use crate::{
    Decode, DecodeError, Decoder, Encodable, EncodableOption, Encode, EncodeError, EncodeOption,
    Encoder, EncoderExt as _, Slot, TakeFrom, WireVector,
};

/// An optional FIDL vector
#[repr(transparent)]
pub struct WireOptionalVector<'buf, T> {
    raw: RawWireVector<'buf, T>,
}

impl<T> Drop for WireOptionalVector<'_, T> {
    fn drop(&mut self) {
        if self.is_some() {
            unsafe {
                self.raw.as_slice_ptr().drop_in_place();
            }
        }
    }
}

impl<'buf, T> WireOptionalVector<'buf, T> {
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

    /// Takes the vector out of the option, if any.
    pub fn take(&mut self) -> Option<WireVector<'buf, T>> {
        if self.is_some() {
            Some(unsafe {
                WireVector::new_unchecked(replace(&mut self.raw, RawWireVector::null()))
            })
        } else {
            None
        }
    }

    /// Gets a reference to the vector, if any.
    pub fn as_ref(&self) -> Option<&WireVector<'buf, T>> {
        if self.is_some() {
            Some(unsafe { &*(self as *const Self).cast() })
        } else {
            None
        }
    }

    /// Gets a mutable reference to the vector, if any.
    pub fn as_mut(&mut self) -> Option<&mut WireVector<'buf, T>> {
        if self.is_some() {
            Some(unsafe { &mut *(self as *mut Self).cast() })
        } else {
            None
        }
    }
}

impl<T> Default for WireOptionalVector<'_, T> {
    fn default() -> Self {
        Self { raw: RawWireVector::null() }
    }
}

impl<T: fmt::Debug> fmt::Debug for WireOptionalVector<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<'buf, D: Decoder<'buf> + ?Sized, T: Decode<D>> Decode<D>
    for WireOptionalVector<'buf, T>
{
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
    type EncodedOption<'buf> = WireOptionalVector<'buf, T::Encoded<'buf>>;
}

impl<E: Encoder + ?Sized, T: Encode<E>> EncodeOption<E> for Vec<T> {
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        slot: Slot<'_, Self::EncodedOption<'_>>,
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

impl<T: TakeFrom<WT>, WT> TakeFrom<WireOptionalVector<'_, WT>> for Option<Vec<T>> {
    fn take_from(from: &mut WireOptionalVector<'_, WT>) -> Self {
        from.as_mut().map(Vec::take_from)
    }
}
