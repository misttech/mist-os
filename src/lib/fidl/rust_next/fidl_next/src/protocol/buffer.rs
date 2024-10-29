// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::protocol::{
    ProtocolError, Transport, WireMessageHeader, FLAG_0_WIRE_FORMAT_V2_BIT, MAGIC_NUMBER,
};
use crate::{
    u32_le, u64_le, Decode, DecodeError, Decoder, DecoderExt as _, Encode, EncodeError, Encoder,
    EncoderExt as _, Owned,
};

/// Encodes a message into the given buffer.
pub fn encode_buffer<T: Transport, M>(
    buffer: &mut T::SendBuffer,
    txid: u32,
    ordinal: u64,
    message: &mut M,
) -> Result<(), EncodeError>
where
    for<'a> T::Encoder<'a>: Encoder,
    M: for<'a> Encode<T::Encoder<'a>>,
{
    let mut encoder = T::encoder(buffer);
    encoder.encode_next(&mut WireMessageHeader {
        txid: u32_le::from_native(txid),
        flags: [FLAG_0_WIRE_FORMAT_V2_BIT, 0, 0],
        magic_number: MAGIC_NUMBER,
        ordinal: u64_le::from_native(ordinal),
    })?;
    encoder.encode_next(message)
}

/// A transport buffer with a pre-parsed header and known ordinal.
pub struct MessageBuffer<T: Transport> {
    ordinal: u64,
    buffer: T::RecvBuffer,
}

impl<T: Transport> MessageBuffer<T> {
    /// Creates a new message buffer by parsing the given transport buffer.
    ///
    /// On success, returns the transaction ID and message buffer.
    pub fn parse_header(mut buffer: T::RecvBuffer) -> Result<(u32, Self), ProtocolError<T::Error>>
    where
        for<'a> T::Decoder<'a>: Decoder<'a>,
    {
        let (txid, ordinal) = {
            let mut decoder = T::decoder(&mut buffer);
            let header = decoder
                .decode_next::<WireMessageHeader>()
                .map_err(ProtocolError::InvalidMessageHeader)?;
            (header.txid.to_native(), header.ordinal.to_native())
        };

        Ok((txid, Self { ordinal, buffer }))
    }

    /// Returns the ordinal of the message buffer.
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Returns the underlying buffer.
    pub fn into_inner(self) -> T::RecvBuffer {
        self.buffer
    }

    /// Decodes the buffer, returning the contained message.
    pub fn decode<'a, M>(&'a mut self) -> Result<Owned<'a, M>, DecodeError>
    where
        M: Decode<T::Decoder<'a>>,
        T::Decoder<'a>: Decoder<'a>,
    {
        let mut decoder = T::decoder(&mut self.buffer);
        decoder.decode_last::<M>()
    }
}
