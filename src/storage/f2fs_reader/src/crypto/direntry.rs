// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use siphasher::sip::SipHasher;
use std::hash::Hasher;

// We have to match the hash_code implementation used by f2fs to migrate direntry without
// decrypting them.

// See: https://en.wikipedia.org/wiki/Tiny_Encryption_Algorithm
fn tea(input: &[u32; 4], buf: &mut [u32; 2]) {
    const DELTA: u32 = 0x9e3779b9;
    let mut sum = 0u32;
    let mut v = buf.clone();
    for _ in 0..16 {
        sum = sum.wrapping_add(DELTA);
        v[0] = v[0].wrapping_add(
            (v[1] << 4).wrapping_add(input[0])
                ^ v[1].wrapping_add(sum)
                ^ (v[1] >> 5).wrapping_add(input[1]),
        );
        v[1] = v[1].wrapping_add(
            (v[0] << 4).wrapping_add(input[2])
                ^ v[0].wrapping_add(sum)
                ^ (v[0] >> 5).wrapping_add(input[3]),
        );
    }
    buf[0] = buf[0].wrapping_add(v[0]);
    buf[1] = buf[1].wrapping_add(v[1]);
}

fn str_hash(input: &[u8], padding: u32, out: &mut [u32; 4]) {
    debug_assert!(input.len() <= out.len() * 4);
    *out = [padding; 4];
    let mut out_ix = 0;
    let mut v = padding;
    for i in 0..input.len() {
        v = (input[i] as u32).wrapping_add(v << 8);
        if i % 4 == 3 {
            out[out_ix] = v;
            out_ix += 1;
            v = padding;
        }
    }
    if out_ix < 4 {
        out[out_ix] = v;
    }
}

/// This is the function used unless both casefolding + encryption are enabled.
pub fn tea_hash_filename(name: &[u8]) -> u32 {
    let mut buf = [0x67452301, 0xefcdab89];
    let mut len = name.len() as u32;
    name.chunks(16).for_each(|chunk| {
        let mut k = [0; 4];
        let padding = len | (len << 8) | (len << 16) | (len << 24);
        len -= 16;
        str_hash(chunk, padding, &mut k);
        tea(&k, &mut buf);
    });
    buf[0]
}

// A stronger hash function is used if casefold + FBE are used together.
// Nb: If encryption is used without casefolding, the hash_code is based on the encrypted filename.
pub fn casefold_encrypt_hash_filename(name: &[u8], dirhash_key: &[u8; 16]) -> u32 {
    let mut hasher = SipHasher::new_with_key(dirhash_key);
    hasher.write(name);
    hasher.finish() as u32
}
