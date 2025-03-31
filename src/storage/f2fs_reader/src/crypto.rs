// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
pub mod direntry;
mod hkdf;

use crate::xattr;
use aes::cipher::inout::InOutBuf;
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use aes::Block;
use anyhow::{anyhow, ensure, Error};
use base64::engine::general_purpose::URL_SAFE;
use base64::Engine as _;
use sha2::Digest;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

const POLICY_FLAGS_PAD_16: u8 = 2;
const ENCRYPTION_MODE_AES_256_XTS: u8 = 1;
const ENCRYPTION_MODE_AES_256_CTS: u8 = 4;

const NAME_XATTR_CRYPTO_CONTEXT: &[u8] = b"c";

// Fscrypt tacks a prefix onto the 'info' field in HKDF used for different purposes.
// This prefix is built from one of the following context.
const HKDF_CONTEXT_PER_FILE_ENC_KEY: u8 = 2;
const HKDF_CONTEXT_DIRHASH_KEY: u8 = 5;

/// An encryption context is written as an xattr to the directory root of each FBE hierarchy.
/// For f2fs, this is stored with index 9 and name "c".
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Immutable, KnownLayout, FromBytes, IntoBytes, Unaligned)]
pub struct Context {
    pub version: u8,                   // = 2
    pub contents_encryption_mode: u8,  // = ENCRYPTION_MODE_AES_256_XTS
    pub filenames_encryption_mode: u8, // = ENCRYPTION_MODE_AES_256_CTS
    pub flags: u8,                     // = POLICY_FLAGS_PAD_16
    pub log2_data_unit_size: u8,       // = 0
    _reserved: [u8; 3],
    pub main_key_identifier: [u8; 16],
    pub nonce: [u8; 16],
}

impl Context {
    pub fn read_from_xattr(xattr: &Vec<xattr::XattrEntry>) -> Result<Option<Self>, Error> {
        let raw_context = if let Some(content) = xattr.iter().find(|entry| {
            entry.index == xattr::Index::Encryption && *entry.name == *NAME_XATTR_CRYPTO_CONTEXT
        }) {
            &content.value
        } else {
            // No crypto context present.
            return Ok(None);
        };

        let this = Context::read_from_bytes(raw_context.as_ref())
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
        // We also only support standard key derivation, so no INO_LBLK_64 or INO_LBLK_32 flags.
        ensure!(this.flags == POLICY_FLAGS_PAD_16, "Unsupported flags");
        // This controls the data unit size used for encryption blocks. We only support the default.
        ensure!(this.log2_data_unit_size == 0, "Unsupported custom DUN size");
        Ok(Some(this))
    }
}

pub struct PerFileDecryptor {
    // For file contents (XTS)
    // TODO(b/406351838): Implement XTS support.
    // For file names (CTS)
    cbc: cbc::Decryptor<aes::Aes256>,
    // For seeding SipHasher for directory entry hashes
    dirhash_key: [u8; 16],
}

impl PerFileDecryptor {
    pub(super) fn new(main_key: &[u8; 64], nonce: &[u8; 16]) -> Self {
        // Derive the FBE key from the main key via HKDF with nonce.
        let key = hkdf::fscrypt_hkdf::<64>(main_key, nonce, HKDF_CONTEXT_PER_FILE_ENC_KEY);
        let dirhash_key = hkdf::fscrypt_hkdf::<16>(main_key, nonce, HKDF_CONTEXT_DIRHASH_KEY);
        Self {
            cbc: cbc::Decryptor::<aes::Aes256>::new((&key[..32]).into(), (&[0u8; 16]).into()),
            dirhash_key,
        }
    }

    // TODO(b/406351838): Implement XTS method for data blocks.

    /// Decrypt a filename (from a dentry or symlink).
    pub fn decrypt_filename_data(&self, data: &mut [u8]) {
        // AES-256-CTS is used for filename encryption and symlinks but because we
        // require POLICY_FLAGS_PAD_16, we never actually steal any ciphertext and
        // so CTS is equivalent to swapping the last two blocks and using CBC instead.
        let inout: InOutBuf<'_, '_, u8> = data.into();
        let (mut blocks, tail): (InOutBuf<'_, '_, Block>, _) = inout.into_chunks();
        debug_assert_eq!(tail.len(), 0);
        let mut chunks = blocks.get_out();
        if chunks.len() >= 2 {
            chunks.swap(chunks.len() - 1, chunks.len() - 2);
        }
        self.cbc.clone().decrypt_blocks_mut(&mut chunks);
    }

    pub fn dirhash_key(&self) -> &[u8; 16] {
        &self.dirhash_key
    }
}

/// Returns the identifier for a given main key.
pub fn main_key_to_identifier(main_key: &[u8; 64]) -> [u8; 16] {
    hkdf::fscrypt_hkdf::<16>(main_key, &[], 1)
}

/// A proxy filename is used when we don't have the keys to decrypt the actual filename.
///
/// When working with locked directories, we encode both Dentry and user provided
/// filenames using this struct before comparing them.
///
/// The hash code is encoded directly, allowing an index in some cases, even when entries
/// are encrypted.
///
/// Encrypted filenames are unprintable so we base64 encode but base64 encoding uses 4 bytes for
/// every 3 bytes of input long filenames cannot be represented like this without exceeding the
/// NAME_LEN limit (255).
///
/// As a work around to keep uniqueness, for filenames longer than 149 bytes, we use the filename
/// prefix and calculate the sha256 of the full encrypted name.  This produces a filename that is
/// under the limit and very likely to remain unique.
///
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, FromBytes, Immutable, KnownLayout, IntoBytes, Unaligned)]
pub struct ProxyFilename {
    hash_code: [u32; 2],
    filename: [u8; 149],
    sha256: [u8; 32],
}

impl ProxyFilename {
    pub fn new(hash_code: [u32; 2], raw_filename: &[u8]) -> Self {
        let mut filename = [0u8; 149];
        let mut sha256 = [0u8; 32];
        if raw_filename.len() < filename.len() {
            filename[..raw_filename.len()].copy_from_slice(raw_filename);
        } else {
            let len = filename.len();
            filename.copy_from_slice(&raw_filename[..len]);
            sha256 = sha2::Sha256::digest(raw_filename).into();
        }
        Self { hash_code, filename, sha256 }
    }
}

impl Into<String> for ProxyFilename {
    fn into(self) -> String {
        let mut bytes = self.as_bytes().to_vec();
        while let Some(0) = bytes.last() {
            bytes.pop();
        }
        URL_SAFE.encode(bytes)
    }
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
