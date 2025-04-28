// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::str::{from_utf8, from_utf8_unchecked};

use munge::munge;

use crate::{
    Decode, DecodeError, Decoder, Encodable, Encode, EncodeError, EncodeRef, Encoder, Slot,
    TakeFrom, WireVector, ZeroPadding,
};

/// A FIDL string
#[repr(transparent)]
pub struct WireString {
    vec: WireVector<u8>,
}

unsafe impl ZeroPadding for WireString {
    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        munge!(let Self { vec } = out);
        WireVector::<u8>::zero_padding(vec);
    }
}

impl WireString {
    /// Encodes that a string is present in a slot.
    #[inline]
    pub fn encode_present(out: &mut MaybeUninit<Self>, len: u64) {
        munge!(let Self { vec } = out);
        WireVector::encode_present(vec, len);
    }

    /// Returns the length of the string in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.vec.len()
    }

    /// Returns whether the string is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a reference to the underlying `str`.
    #[inline]
    pub fn as_str(&self) -> &str {
        unsafe { from_utf8_unchecked(self.vec.as_slice()) }
    }
}

impl Deref for WireString {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Debug for WireString {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

unsafe impl<D: Decoder + ?Sized> Decode<D> for WireString {
    #[inline]
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { mut vec } = slot);

        unsafe {
            WireVector::decode_raw(vec.as_mut(), decoder)?;
        }
        let vec = unsafe { vec.deref_unchecked() };

        // Check if the string is valid ASCII (fast path)
        if !vec.as_slice().is_ascii() {
            // Fall back to checking if the string is valid UTF-8 (slow path)
            // We're using `from_utf8` more like an `is_utf8` here.
            let _ = from_utf8(vec.as_slice())?;
        }

        Ok(())
    }
}

impl Encodable for String {
    type Encoded = WireString;
}

unsafe impl<E: Encoder + ?Sized> Encode<E> for String {
    #[inline]
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        self.as_str().encode(encoder, out)
    }
}

unsafe impl<E: Encoder + ?Sized> EncodeRef<E> for String {
    #[inline]
    fn encode_ref(
        &self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        self.as_str().encode(encoder, out)
    }
}

impl Encodable for &str {
    type Encoded = WireString;
}

unsafe impl<E: Encoder + ?Sized> Encode<E> for &str {
    #[inline]
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encoder.write(self.as_bytes());
        WireString::encode_present(out, self.len() as u64);
        Ok(())
    }
}

impl TakeFrom<WireString> for String {
    #[inline]
    fn take_from(from: &WireString) -> Self {
        from.to_string()
    }
}
