// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::ops::Deref;
use core::str::{from_utf8, from_utf8_unchecked};

use munge::munge;

use crate::{
    Decode, DecodeError, Decoder, Encodable, Encode, EncodeError, Encoder, Slot, TakeFrom,
    WireVector,
};

/// A FIDL string
#[repr(transparent)]
pub struct WireString {
    vec: WireVector<u8>,
}

impl WireString {
    /// Encodes that a string is present in a slot.
    pub fn encode_present(slot: Slot<'_, Self>, len: u64) {
        munge!(let Self { vec } = slot);
        WireVector::encode_present(vec, len);
    }

    /// Returns the length of the string in bytes.
    pub fn len(&self) -> usize {
        self.vec.len()
    }

    /// Returns whether the string is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a reference to the underlying `str`.
    pub fn as_str(&self) -> &str {
        unsafe { from_utf8_unchecked(self.vec.as_slice()) }
    }
}

impl Deref for WireString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Debug for WireString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

unsafe impl<'buf, D: Decoder<'buf> + ?Sized> Decode<D> for WireString {
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { mut vec } = slot);

        WireVector::decode(vec.as_mut(), decoder)?;
        let vec = unsafe { vec.deref_unchecked() };
        from_utf8(vec.as_slice())?;

        Ok(())
    }
}

impl Encodable for String {
    type Encoded = WireString;
}

impl<E: Encoder + ?Sized> Encode<E> for String {
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encoder.write(self.as_bytes());
        WireString::encode_present(slot, self.len() as u64);
        Ok(())
    }
}

impl TakeFrom<WireString> for String {
    fn take_from(from: &WireString) -> Self {
        from.to_string()
    }
}
