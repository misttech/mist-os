// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::ops::{Deref, DerefMut};
use core::str::{from_utf8, from_utf8_unchecked, from_utf8_unchecked_mut};

use munge::munge;

use crate::{decode, encode, Decode, Encode, Slot, TakeFrom, WireVector};

/// A FIDL string
#[derive(Default)]
#[repr(transparent)]
pub struct WireString<'buf> {
    vec: WireVector<'buf, u8>,
}

impl<'buf> WireString<'buf> {
    /// Creates a new `WireString` from the a `WireVector` of UTF-8 bytes.
    pub(super) unsafe fn new_unchecked(vec: WireVector<'buf, u8>) -> Self {
        Self { vec }
    }

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

    /// Returns a mutable reference to the underlying `str`.
    pub fn as_mut_str(&mut self) -> &mut str {
        unsafe { from_utf8_unchecked_mut(self.vec.as_mut_slice()) }
    }
}

impl Deref for WireString<'_> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl DerefMut for WireString<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_str()
    }
}

impl fmt::Debug for WireString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

unsafe impl<'buf> Decode<'buf> for WireString<'buf> {
    fn decode(
        slot: Slot<'_, Self>,
        decoder: &mut decode::Decoder<'buf>,
    ) -> Result<(), decode::Error> {
        munge!(let Self { mut vec } = slot);

        WireVector::decode(vec.as_mut(), decoder)?;
        let vec = unsafe { vec.deref_unchecked() };
        from_utf8(vec.as_slice())?;

        Ok(())
    }
}

impl Encode for String {
    type Encoded<'buf> = WireString<'buf>;

    fn encode(
        &mut self,
        encoder: &mut encode::Encoder,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), encode::Error> {
        encoder.write_bytes(self.as_bytes());
        WireString::encode_present(slot, self.len() as u64);
        Ok(())
    }
}

impl TakeFrom<WireString<'_>> for String {
    fn take_from(from: &mut WireString<'_>) -> Self {
        from.to_string()
    }
}
