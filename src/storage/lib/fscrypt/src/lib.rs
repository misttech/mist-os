// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
pub mod direntry;
pub mod hkdf;
pub mod proxy_filename;

use anyhow::{anyhow, ensure, Error};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const POLICY_FLAGS_PAD_16: u8 = 0x02;
pub const POLICY_FLAGS_INO_LBLK_32: u8 = 0x10;
const SUPPORTED_POLICY_FLAGS: u8 = POLICY_FLAGS_PAD_16 | POLICY_FLAGS_INO_LBLK_32;

pub const ENCRYPTION_MODE_AES_256_XTS: u8 = 1;
pub const ENCRYPTION_MODE_AES_256_CTS: u8 = 4;

/// An encryption context is written as an xattr to the directory root of each FBE hierarchy.
/// For f2fs, this is stored with index 9 and name "c".
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Immutable, KnownLayout, FromBytes, IntoBytes, Unaligned)]
pub struct Context {
    pub version: u8,                   // = 2
    pub contents_encryption_mode: u8,  // = ENCRYPTION_MODE_AES_256_XTS
    pub filenames_encryption_mode: u8, // = ENCRYPTION_MODE_AES_256_CTS
    pub flags: u8,                     // = POLICY_FLAGS_*
    pub log2_data_unit_size: u8,       // = 0
    _reserved: [u8; 3],
    pub main_key_identifier: [u8; 16],
    pub nonce: [u8; 16],
}

impl Context {
    pub fn try_from_bytes(raw_context: &[u8]) -> Result<Option<Self>, Error> {
        let this = Context::read_from_bytes(raw_context)
            .map_err(|_| anyhow!("Bad sized crypto context"))?;
        ensure!(this.version == 2, "Bad version number in crypto context");
        ensure!(
            this.contents_encryption_mode == ENCRYPTION_MODE_AES_256_XTS,
            "Unsupported contents_encryption_mode",
        );
        ensure!(
            this.filenames_encryption_mode == ENCRYPTION_MODE_AES_256_CTS,
            "Unsupported filenames_encryption_mode"
        );
        // We assume 16 byte zero padding.
        // We also only support standard key derivation and INO_LBLK_32, no INO_LBLK_64 or DIRECT.
        ensure!(this.flags & !SUPPORTED_POLICY_FLAGS == 0, "Unsupported flags in crypto context");
        // This controls the data unit size used for encryption blocks. We only support the default.
        ensure!(this.log2_data_unit_size == 0, "Unsupported custom DUN size");
        Ok(Some(this))
    }
}

/// Returns the identifier for a given main key.
pub fn main_key_to_identifier(main_key: &[u8; 64]) -> [u8; 16] {
    hkdf::fscrypt_hkdf::<16>(main_key, &[], 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_main_key_to_identifier() {
        // Nb: Hard coded test vector from an fscrypt instance.
        let key_digest = "dc34d175ba21b27e2e92829b0dc12666ce8bfbcbae387014c6bb0d8b7678dafa6466bd7565b1a5999cd3f8a39a470528fa6816768e6985f0b10804af7d657810";
        let key: [u8; 64] = hex::decode(&key_digest).unwrap().try_into().unwrap();
        assert_eq!(hex::encode(main_key_to_identifier(&key)), "fc7f69a149f89a7529374cf9e96a6d13");
    }
}
