// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Chunk, Decode, Decoded, DecoderExt as _, Encode, EncoderExt as _};

pub fn assert_encoded<T: Encode<Vec<Chunk>>>(value: T, chunks: &[Chunk]) {
    let mut encoded_chunks = Vec::new();
    encoded_chunks.encode_next(value).unwrap();
    assert_eq!(encoded_chunks, chunks, "encoded chunks did not match");
}

pub fn assert_decoded<T: for<'a> Decode<&'a mut [Chunk]>>(
    mut chunks: &mut [Chunk],
    f: impl FnOnce(Decoded<T, &mut [Chunk]>),
) {
    let value = (&mut chunks).decode::<T>().expect("failed to decode");
    f(value)
}
