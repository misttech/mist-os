// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::str::from_utf8;

use munge::munge;

use crate::{
    Decode, DecodeError, Decoder, EncodableOption, EncodeError, EncodeOption, Encoder, Slot,
    TakeFrom, WireOptionalVector, WireString, WireVector,
};

/// An optional FIDL string
#[repr(transparent)]
pub struct WireOptionalString {
    vec: WireOptionalVector<u8>,
}

impl WireOptionalString {
    /// Encodes that a string is present in a slot.
    pub fn encode_present(slot: Slot<'_, Self>, len: u64) {
        munge!(let Self { vec } = slot);
        WireOptionalVector::encode_present(vec, len);
    }

    /// Encodes that a string is absent in a slot.
    pub fn encode_absent(slot: Slot<'_, Self>) {
        munge!(let Self { vec } = slot);
        WireOptionalVector::encode_absent(vec);
    }

    /// Returns whether the optional string is present.
    pub fn is_some(&self) -> bool {
        self.vec.is_some()
    }

    /// Returns whether the optional string is absent.
    pub fn is_none(&self) -> bool {
        self.vec.is_none()
    }

    /// Returns a reference to the underlying string, if any.
    pub fn as_ref(&self) -> Option<&WireString> {
        self.vec.as_ref().map(|vec| unsafe { &*(vec as *const WireVector<u8>).cast() })
    }
}

impl fmt::Debug for WireOptionalString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<'buf, D: Decoder<'buf> + ?Sized> Decode<D> for WireOptionalString {
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { mut vec } = slot);

        WireOptionalVector::decode(vec.as_mut(), decoder)?;
        let vec = unsafe { vec.deref_unchecked() };
        if let Some(bytes) = vec.as_ref() {
            from_utf8(bytes)?;
        }

        Ok(())
    }
}

impl EncodableOption for String {
    type EncodedOption<'buf> = WireOptionalString;
}

impl<E: Encoder + ?Sized> EncodeOption<E> for String {
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        slot: Slot<'_, Self::EncodedOption<'_>>,
    ) -> Result<(), EncodeError> {
        if let Some(string) = this {
            encoder.write(string.as_bytes());
            WireOptionalString::encode_present(slot, string.len() as u64);
        } else {
            WireOptionalString::encode_absent(slot);
        }

        Ok(())
    }
}

impl TakeFrom<WireOptionalString> for Option<String> {
    fn take_from(from: &WireOptionalString) -> Self {
        from.as_ref().map(String::take_from)
    }
}
