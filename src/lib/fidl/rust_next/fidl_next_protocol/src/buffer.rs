// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next_codec::{u32_le, u64_le, DecodeError, DecoderExt as _, EncodeError, EncoderExt as _};

use crate::{Transport, WireMessageHeader, FLAG_0_WIRE_FORMAT_V2_BIT, MAGIC_NUMBER};

/// Encodes a message into the given buffer.
pub fn encode_header<T: Transport>(
    buffer: &mut T::SendBuffer,
    txid: u32,
    ordinal: u64,
) -> Result<(), EncodeError> {
    buffer.encode_next(&mut WireMessageHeader {
        txid: u32_le::from_native(txid),
        flags: [FLAG_0_WIRE_FORMAT_V2_BIT, 0, 0],
        magic_number: MAGIC_NUMBER,
        ordinal: u64_le::from_native(ordinal),
    })
}

/// Parses the transaction ID and ordinal from the given buffer.
pub fn decode_header<T: Transport>(buffer: &mut T::RecvBuffer) -> Result<(u32, u64), DecodeError> {
    let (txid, ordinal) = {
        let mut decoder = T::decoder(buffer);
        let header = decoder.decode_next::<WireMessageHeader>()?;
        (header.txid.to_native(), header.ordinal.to_native())
    };

    Ok((txid, ordinal))
}
