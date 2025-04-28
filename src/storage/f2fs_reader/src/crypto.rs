// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
pub mod direntry;
mod hkdf;

use crate::xattr;
use aes::cipher::inout::InOutBuf;
use aes::cipher::{BlockDecrypt, BlockDecryptMut, BlockEncrypt, KeyInit, KeyIvInit};
use aes::Block;
use anyhow::{anyhow, ensure, Error};
use siphasher::sip::SipHasher;
use std::hash::Hasher;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

const POLICY_FLAGS_PAD_16: u8 = 0x02;
const POLICY_FLAGS_INO_LBLK_32: u8 = 0x10;
const SUPPORTED_POLICY_FLAGS: u8 = POLICY_FLAGS_PAD_16 | POLICY_FLAGS_INO_LBLK_32;

const ENCRYPTION_MODE_AES_256_XTS: u8 = 1;
const ENCRYPTION_MODE_AES_256_CTS: u8 = 4;

const NAME_XATTR_CRYPTO_CONTEXT: &[u8] = b"c";

// Fscrypt tacks a prefix onto the 'info' field in HKDF used for different purposes.
// This prefix is built from one of the following context.
const HKDF_CONTEXT_PER_FILE_ENC_KEY: u8 = 2;
const HKDF_CONTEXT_DIRHASH_KEY: u8 = 5;
const HKDF_CONTEXT_IV_INO_LBLK_32_KEY: u8 = 6;
const HKDF_CONTEXT_INODE_HASH_KEY: u8 = 7;

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
        // We also only support standard key derivation and INO_LBLK_32, no INO_LBLK_64 or DIRECT.
        ensure!(this.flags & !SUPPORTED_POLICY_FLAGS == 0, "Unsupported flags in crypto context");
        // This controls the data unit size used for encryption blocks. We only support the default.
        ensure!(this.log2_data_unit_size == 0, "Unsupported custom DUN size");
        Ok(Some(this))
    }
}

pub struct PerFileDecryptor {
    // For file contents (XTS)
    xts_key1: aes::Aes256,
    xts_key2: aes::Aes256,
    // For file names (CTS)
    cts_key: [u8; 32],
    // For seeding SipHasher for directory entry hashes
    dirhash_key: [u8; 16],

    // For INO_LBLK_32 policy
    ino_hash_key: Option<[u8; 16]>,
}

impl PerFileDecryptor {
    pub(super) fn new(main_key: &[u8; 64], context: Context, uuid: &[u8; 16]) -> Self {
        if context.flags & POLICY_FLAGS_INO_LBLK_32 != 0 {
            // To support eMMC inline crypto hardware (and hardware wrapped keys), the lblk_32
            // policy creates a shared key from the main key and filesystem UUID. In this mode the
            // inode is hashed and mixed into the tweak. The nonce is still used for dirhash.
            let mut hdkf_info = [0; 17];
            hdkf_info[1..17].copy_from_slice(uuid);
            hdkf_info[0] = ENCRYPTION_MODE_AES_256_XTS;
            let xts_key =
                hkdf::fscrypt_hkdf::<64>(main_key, &hdkf_info, HKDF_CONTEXT_IV_INO_LBLK_32_KEY);
            hdkf_info[0] = ENCRYPTION_MODE_AES_256_CTS;
            let cts_key =
                hkdf::fscrypt_hkdf::<32>(main_key, &hdkf_info, HKDF_CONTEXT_IV_INO_LBLK_32_KEY);
            let dirhash_key =
                hkdf::fscrypt_hkdf::<16>(main_key, &context.nonce, HKDF_CONTEXT_DIRHASH_KEY);
            let ino_hash_key =
                Some(hkdf::fscrypt_hkdf::<16>(main_key, &[], HKDF_CONTEXT_INODE_HASH_KEY));
            Self {
                xts_key1: aes::Aes256::new((&xts_key[..32]).into()),
                xts_key2: aes::Aes256::new((&xts_key[32..]).into()),
                cts_key,
                dirhash_key,
                ino_hash_key,
            }
        } else {
            // The default policy creates a unique key for each file using the main key and a
            // 16-byte nonce.
            let key =
                hkdf::fscrypt_hkdf::<64>(main_key, &context.nonce, HKDF_CONTEXT_PER_FILE_ENC_KEY);
            let dirhash_key =
                hkdf::fscrypt_hkdf::<16>(main_key, &context.nonce, HKDF_CONTEXT_DIRHASH_KEY);
            let cts_key: [u8; 32] = key[..32].try_into().unwrap();
            Self {
                xts_key1: aes::Aes256::new((&key[..32]).into()),
                xts_key2: aes::Aes256::new((&key[32..]).into()),
                cts_key,
                dirhash_key,
                ino_hash_key: None,
            }
        }
    }

    pub fn decrypt_data(&self, ino: u32, block_num: u32, buffer: &mut [u8]) {
        assert_eq!((buffer.as_ptr() as usize) % 16, 0, "Require 16-byte aligned buffers");
        assert_eq!(buffer.len() % 16, 0, "Require buffters be multiple of 16-bytes");
        // TODO(b/406351838): Migrate to share implementation with fxfs-crypto?
        let key1 = self.xts_key1.clone();
        let key2 = self.xts_key2.clone();
        let mut tweak: u128 = if let Some(ino_hash_key) = self.ino_hash_key {
            let mut hasher = SipHasher::new_with_key(&ino_hash_key);
            let ino64 = ino as u64;
            hasher.write(ino64.as_bytes());
            hasher.finish() as u32 + block_num
        } else {
            block_num
        } as u128;
        key2.encrypt_block(tweak.as_mut_bytes().into());
        for chunk in buffer.chunks_exact_mut(16) {
            *u128::mut_from_bytes(chunk).unwrap() ^= tweak;
            key1.decrypt_block(chunk.into());
            *u128::mut_from_bytes(chunk).unwrap() ^= tweak;
            tweak = (tweak << 1) ^ (if tweak >> 127 != 0 { 0x87 } else { 0 });
        }
    }

    /// Decrypt a filename (from a dentry or symlink).
    pub fn decrypt_filename_data(&self, ino: u32, data: &mut [u8]) {
        let mut iv = [0u8; 16];
        if let Some(ino_hash_key) = self.ino_hash_key {
            let mut hasher = SipHasher::new_with_key(&ino_hash_key);
            hasher.write((ino as u64).as_bytes());
            iv[..4].copy_from_slice(&hasher.finish().as_bytes()[..4]);
        }
        // AES-256-CTS is used for filename encryption and symlinks but because we
        // require POLICY_FLAGS_PAD_16, we never actually steal any ciphertext and
        // so CTS is equivalent to swapping the last two blocks and using CBC instead.
        let mut cbc = cbc::Decryptor::<aes::Aes256>::new((&self.cts_key).into(), (&iv).into());
        let inout: InOutBuf<'_, '_, u8> = data.into();
        let (mut blocks, tail): (InOutBuf<'_, '_, Block>, _) = inout.into_chunks();
        debug_assert_eq!(tail.len(), 0);
        let mut chunks = blocks.get_out();
        if chunks.len() >= 2 {
            chunks.swap(chunks.len() - 1, chunks.len() - 2);
        }
        cbc.decrypt_blocks_mut(&mut chunks);
    }

    pub fn dirhash_key(&self) -> &[u8; 16] {
        &self.dirhash_key
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
