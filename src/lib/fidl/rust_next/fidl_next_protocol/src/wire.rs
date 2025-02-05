// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next_codec::{u32_le, u64_le, Decode, DecodeError, Encodable, Encode, EncodeError, Slot};

use zerocopy::IntoBytes;

/// A FIDL protocol message header
#[derive(Clone, Copy, Debug, IntoBytes)]
#[repr(C)]
pub struct WireMessageHeader {
    /// The transaction ID of the message header
    pub txid: u32_le,
    /// Flags
    pub flags: [u8; 3],
    /// Magic number
    pub magic_number: u8,
    /// The ordinal of the message following this header
    pub ordinal: u64_le,
}

/// The flag 0 bit indicating that the wire format is v2.
pub const FLAG_0_WIRE_FORMAT_V2_BIT: u8 = 0b0000_0010;

/// The magic number indicating FIDL protocol compatibility.
pub const MAGIC_NUMBER: u8 = 0x01;

impl Encodable for WireMessageHeader {
    type Encoded<'buf> = WireMessageHeader;
}

impl<E: ?Sized> Encode<E> for WireMessageHeader {
    #[inline]
    fn encode(
        &mut self,
        _: &mut E,
        mut slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), EncodeError> {
        slot.write(*self);
        Ok(())
    }
}

unsafe impl<D: ?Sized> Decode<D> for WireMessageHeader {
    #[inline]
    fn decode(_: Slot<'_, Self>, _: &mut D) -> Result<(), DecodeError> {
        Ok(())
    }
}
