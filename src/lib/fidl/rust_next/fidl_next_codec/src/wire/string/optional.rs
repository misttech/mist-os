// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::mem::MaybeUninit;
use core::str::from_utf8;

use munge::munge;

use crate::{
    Decode, DecodeError, Decoder, EncodableOption, EncodeError, EncodeOption, EncodeOptionRef,
    Encoder, Slot, TakeFrom, WireOptionalVector, WireString, WireVector, ZeroPadding,
};

/// An optional FIDL string
#[repr(transparent)]
pub struct WireOptionalString {
    vec: WireOptionalVector<u8>,
}

unsafe impl ZeroPadding for WireOptionalString {
    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        munge!(let Self { vec } = out);
        WireOptionalVector::<u8>::zero_padding(vec);
    }
}

impl WireOptionalString {
    /// Encodes that a string is present in a slot.
    #[inline]
    pub fn encode_present(out: &mut MaybeUninit<Self>, len: u64) {
        munge!(let Self { vec } = out);
        WireOptionalVector::encode_present(vec, len);
    }

    /// Encodes that a string is absent in a slot.
    #[inline]
    pub fn encode_absent(out: &mut MaybeUninit<Self>) {
        munge!(let Self { vec } = out);
        WireOptionalVector::encode_absent(vec);
    }

    /// Returns whether the optional string is present.
    #[inline]
    pub fn is_some(&self) -> bool {
        self.vec.is_some()
    }

    /// Returns whether the optional string is absent.
    #[inline]
    pub fn is_none(&self) -> bool {
        self.vec.is_none()
    }

    /// Returns a reference to the underlying string, if any.
    #[inline]
    pub fn as_ref(&self) -> Option<&WireString> {
        self.vec.as_ref().map(|vec| unsafe { &*(vec as *const WireVector<u8>).cast() })
    }
}

impl fmt::Debug for WireOptionalString {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<D: Decoder + ?Sized> Decode<D> for WireOptionalString {
    #[inline]
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { mut vec } = slot);

        unsafe {
            WireOptionalVector::decode_raw(vec.as_mut(), decoder)?;
        }
        let vec = unsafe { vec.deref_unchecked() };
        if let Some(bytes) = vec.as_ref() {
            // Check if the string is valid ASCII (fast path)
            if !bytes.as_slice().is_ascii() {
                // Fall back to checking if the string is valid UTF-8 (slow path)
                // We're using `from_utf8` more like an `is_utf8` here.
                let _ = from_utf8(bytes)?;
            }
        }

        Ok(())
    }
}

impl EncodableOption for String {
    type EncodedOption = WireOptionalString;
}

unsafe impl<E: Encoder + ?Sized> EncodeOption<E> for String {
    #[inline]
    fn encode_option(
        this: Option<Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        <&str>::encode_option(this.as_deref(), encoder, out)
    }
}

unsafe impl<E: Encoder + ?Sized> EncodeOptionRef<E> for String {
    #[inline]
    fn encode_option_ref(
        this: Option<&Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        <&str>::encode_option(this.map(String::as_str), encoder, out)
    }
}

impl EncodableOption for &str {
    type EncodedOption = WireOptionalString;
}

unsafe impl<E: Encoder + ?Sized> EncodeOption<E> for &str {
    #[inline]
    fn encode_option(
        this: Option<Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        if let Some(string) = this {
            encoder.write(string.as_bytes());
            WireOptionalString::encode_present(out, string.len() as u64);
        } else {
            WireOptionalString::encode_absent(out);
        }
        Ok(())
    }
}

impl TakeFrom<WireOptionalString> for Option<String> {
    #[inline]
    fn take_from(from: &WireOptionalString) -> Self {
        from.as_ref().map(String::take_from)
    }
}
