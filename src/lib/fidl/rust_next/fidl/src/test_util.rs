// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::decoder::BasicDecoder;
use crate::encoder::BasicEncoder;
use crate::{Chunk, Decode, DecoderExt as _, Encode, EncoderExt as _, Owned};

macro_rules! chunks {
    ($(
        $b0:literal $b1:literal $b2:literal $b3:literal
        $b4:literal $b5:literal $b6:literal $b7:literal
    )*) => {
        [
            $($crate::Chunk::from_native(u64::from_le_bytes([
                $b0, $b1, $b2, $b3, $b4, $b5, $b6, $b7,
            ])),)*
        ]
    }
}

pub fn assert_encoded<T: Encode<BasicEncoder>>(mut value: T, chunks: &[Chunk]) {
    let mut encoder = BasicEncoder::new();
    encoder.encode(&mut value).unwrap();
    let encoded_chunks = encoder.finish();
    assert_eq!(encoded_chunks, chunks, "encoded chunks did not match");
}

pub fn assert_decoded<'buf, T: Decode<BasicDecoder<'buf>>>(
    chunks: &'buf mut [Chunk],
    f: impl FnOnce(Owned<'buf, T>),
) {
    let mut decoder = BasicDecoder::new(chunks);
    let value = decoder.decode_next::<T>().expect("failed to decode");
    f(value)
}
