// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::MaybeUninit;

use fidl_next_codec::{
    Decode, DecodeError, Encodable, Encode, EncodeError, EncodeRef, Slot, WireU32, WireU64,
    ZeroPadding,
};

use zerocopy::IntoBytes;

/// A FIDL protocol message header
#[derive(Clone, Copy, Debug, IntoBytes)]
#[repr(C)]
pub struct WireMessageHeader {
    /// The transaction ID of the message header
    pub txid: WireU32,
    /// Flags
    pub flags: [u8; 3],
    /// Magic number
    pub magic_number: u8,
    /// The ordinal of the message following this header
    pub ordinal: WireU64,
}

unsafe impl ZeroPadding for WireMessageHeader {
    #[inline]
    fn zero_padding(_: &mut MaybeUninit<Self>) {
        // Wire message headers have no padding
    }
}

/// The flag 0 bit indicating that the wire format is v2.
pub const FLAG_0_WIRE_FORMAT_V2_BIT: u8 = 0b0000_0010;

/// The magic number indicating FIDL protocol compatibility.
pub const MAGIC_NUMBER: u8 = 0x01;

impl Encodable for WireMessageHeader {
    type Encoded = WireMessageHeader;
}

unsafe impl<E: ?Sized> Encode<E> for WireMessageHeader {
    #[inline]
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        self.encode_ref(encoder, out)
    }
}

unsafe impl<E: ?Sized> EncodeRef<E> for WireMessageHeader {
    #[inline]
    fn encode_ref(
        &self,
        _: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        out.write(*self);
        Ok(())
    }
}

unsafe impl<D: ?Sized> Decode<D> for WireMessageHeader {
    #[inline]
    fn decode(_: Slot<'_, Self>, _: &mut D) -> Result<(), DecodeError> {
        Ok(())
    }
}
