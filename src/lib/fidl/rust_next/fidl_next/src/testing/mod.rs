// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod transport;

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

pub fn assert_encoded<T: Encode<Vec<Chunk>>>(mut value: T, chunks: &[Chunk]) {
    let mut encoded_chunks = Vec::new();
    encoded_chunks.encode_next(&mut value).unwrap();
    assert_eq!(encoded_chunks, chunks, "encoded chunks did not match");
}

pub fn assert_decoded<'buf, T: Decode<&'buf mut [Chunk]>>(
    mut chunks: &'buf mut [Chunk],
    f: impl FnOnce(Owned<'buf, T>),
) {
    let value = chunks.decode_next::<T>().expect("failed to decode");
    f(value)
}
