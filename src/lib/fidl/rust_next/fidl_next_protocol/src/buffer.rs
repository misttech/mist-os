// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next_codec::{
    DecodeError, DecoderExt as _, EncodeError, EncoderExt as _, WireU32, WireU64,
};

use crate::{Transport, WireMessageHeader, FLAG_0_WIRE_FORMAT_V2_BIT, MAGIC_NUMBER};

/// Encodes a message into the given buffer.
pub fn encode_header<T: Transport>(
    buffer: &mut T::SendBuffer,
    txid: u32,
    ordinal: u64,
) -> Result<(), EncodeError> {
    buffer.encode_next(WireMessageHeader {
        txid: WireU32(txid),
        flags: [FLAG_0_WIRE_FORMAT_V2_BIT, 0, 0],
        magic_number: MAGIC_NUMBER,
        ordinal: WireU64(ordinal),
    })
}

/// Parses the transaction ID and ordinal from the given buffer.
pub fn decode_header<T: Transport>(
    mut buffer: &mut T::RecvBuffer,
) -> Result<(u32, u64), DecodeError> {
    let (txid, ordinal) = {
        let header = buffer.decode_prefix::<WireMessageHeader>()?;
        (*header.txid, *header.ordinal)
    };

    Ok((txid, ordinal))
}
