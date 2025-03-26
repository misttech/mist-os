// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use hmac::Mac;

/// An fscrypt compatible implementation of HKDF (HKDF-extract + HKDF-expand)
/// This is just regular HKDF but with 'info' prefixed.
/// `context` is an fscrypt special.
pub fn fscrypt_hkdf<const L: usize>(
    initial_key_material: &[u8],
    info: &[u8],
    context: u8,
) -> [u8; L] {
    let mut out = [0u8; L];
    let mut fscrypt_info = Vec::new();
    fscrypt_info.extend_from_slice(b"fscrypt\0");
    fscrypt_info.push(context);
    debug_assert_eq!(fscrypt_info.len(), 9);
    fscrypt_info.extend_from_slice(info);
    hkdf::<L>(initial_key_material, &fscrypt_info, &mut out);
    out
}

/// Standard HKDF implementation. See https://datatracker.ietf.org/doc/html/rfc5869
/// Note that we assume an all-zero seed for PRK.
/// `initial_key_material` is the data being hashed.
/// `info` is optional context (can be zero length string)
/// `out` is populated with the result.
fn hkdf<const L: usize>(initial_key_material: &[u8], info: &[u8], out: &mut [u8; L]) {
    const HASH_LEN: usize = 64;
    // HKDF-extract
    let mut hmac = hmac::Hmac::<sha2::Sha512>::new_from_slice(&[0; HASH_LEN]).unwrap();
    hmac.update(initial_key_material);
    let prk = hmac.finalize().into_bytes();
    // HKDF-expand
    let n = (L + HASH_LEN - 1) / HASH_LEN;
    let mut t: Vec<Vec<u8>> = Vec::new();
    t.push(vec![]);
    for i in 0..n {
        let mut payload = Vec::new();
        payload.extend_from_slice(&t[i]);
        payload.extend_from_slice(info);
        payload.push((i + 1) as u8);
        let mut hmac = hmac::Hmac::<sha2::Sha512>::new_from_slice(&prk).unwrap();
        hmac.update(&payload[..]);
        let val = hmac.finalize().into_bytes();
        t.push(val.to_vec());
    }
    let mut result = Vec::new();
    for slice in t {
        result.extend_from_slice(&slice);
    }
    out.copy_from_slice(&result[..L]);
}
