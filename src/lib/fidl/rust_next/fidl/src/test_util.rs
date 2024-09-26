// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::decode::{Decoder, Owned};
use crate::encode::Encoder;
use crate::{Chunk, Decode, Encode, Handle};

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

pub fn assert_encoded<T: Encode>(mut value: T, chunks: &[Chunk], handles: &[Handle]) {
    let mut encoder = Encoder::new();
    encoder.encode(&mut value).unwrap();
    let (encoded_chunks, encoded_handles) = encoder.finish();
    assert_eq!(encoded_chunks, chunks, "encoded chunks did not match");
    assert_eq!(encoded_handles, handles, "encoded handles did not match");
}

pub fn assert_decoded<'buf, T: Decode<'buf>>(
    chunks: &'buf mut [Chunk],
    handles: Vec<Handle>,
    f: impl FnOnce(Owned<'buf, T>),
) {
    let mut decoder = Decoder::new(chunks, handles);
    let value = decoder.decode_next::<T>().expect("failed to decode");
    f(value)
}
