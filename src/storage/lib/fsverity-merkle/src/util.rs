// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{SHA256_SALT_PADDING, SHA512_SALT_PADDING};
use mundane::hash::{Digest, Hasher, Sha256, Sha256Digest, Sha512, Sha512Digest};

/// `Sha256Struct` impls the HasherTrait with type Sha256. The `salt` is set according to the
/// FsverityMetadata struct stored in fxfs and `block_size` is that of the filesystem.
/// TODO(b/309127106): Refactor Sha256Struct and Sha512Struct to use an enum + common struct.
#[derive(Clone)]
pub struct Sha256Struct {
    salt: Vec<u8>,
    block_size: usize,
}

impl Sha256Struct {
    pub fn new(salt: Vec<u8>, block_size: usize) -> Self {
        Sha256Struct { salt, block_size }
    }
}

/// `Sha512Struct` impls the HasherTrait with type Sha512. The `salt` is set according to the
/// FsverityMetadata struct stored in fxfs and `block_size` is that of the filesystem.
/// TODO(b/309127106): Refactor Sha256Struct and Sha512Struct to use an enum + common struct.
#[derive(Clone)]
pub struct Sha512Struct {
    salt: Vec<u8>,
    block_size: usize,
}

impl Sha512Struct {
    pub fn new(salt: Vec<u8>, block_size: usize) -> Self {
        Sha512Struct { salt, block_size }
    }
}

pub trait HasherTrait<H: Hasher> {
    /// Returns the block size of the filesystem.
    fn block_size(&self) -> usize;

    /// Returns the size of MerkleTree digest for this Hasher.
    fn hash_size(&self) -> usize;

    /// Computes a MerkleTree digest from a `block` of data.
    fn hash_block(&self, block: &[u8]) -> H::Digest;

    /// Computes a MerkleTree digest from a block of `hashes`.
    fn hash_hashes(&self, hashes: &[H::Digest]) -> H::Digest;
}

impl HasherTrait<Sha256> for Sha256Struct {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn hash_size(&self) -> usize {
        <Sha256 as Hasher>::Digest::DIGEST_LEN
    }

    /// Computes the MerkleTree digest from a `block` of data.
    ///
    /// For `Sha256Struct`, a MerkleTree Digest is a SHA-256 hash of a block of data. The block
    /// will be zero filled if its len is less than self.block_size, except for when the first data
    /// block is completely empty. If self.salt.len() > 0, we prepend the block with self.salt
    /// which itself is zero filled up to SHA256_SALT_PADDING.
    ///
    /// # Panics
    ///
    /// Panics if `block.len()` exceeds `self.block_size`.
    fn hash_block(&self, block: &[u8]) -> <Sha256 as Hasher>::Digest {
        if block.is_empty() {
            // Empty files have a root hash of all zeroes.
            return Sha256Digest::from_bytes([0; <Sha256 as Hasher>::Digest::DIGEST_LEN]);
        }
        assert!(block.len() <= self.block_size);
        let mut hasher = Sha256::default();
        let salt_size = self.salt.len() as u8;

        if salt_size > 0 {
            hasher.update(&self.salt);
            if salt_size % SHA256_SALT_PADDING != 0 {
                hasher.update(&vec![
                    0;
                    (SHA256_SALT_PADDING - salt_size % SHA256_SALT_PADDING) as usize
                ])
            }
        }

        hasher.update(block);
        // Zero fill block up to self.block_size(). As a special case, if the first data block is
        // completely empty, it is not zero filled.
        if block.len() != self.block_size {
            hasher.update(&vec![0; self.block_size - block.len()]);
        }
        hasher.finish()
    }

    /// Computes a MerkleTree digest from a block of SHA-256 `hashes`.
    ///
    /// Like `hash_block`, `hash_hashes` zero fills incomplete buffers and prepends the digests
    /// with a salt, which is zero filled up to SHA256_SALT_PADDING.
    ///
    /// # Panics
    ///
    /// Panics if any of the following conditions are met:
    /// - `hashes.len()` is 0
    /// - `hashes.len() > self.block_size / SHA-256 digest length`
    fn hash_hashes(&self, hashes: &[<Sha256 as Hasher>::Digest]) -> <Sha256 as Hasher>::Digest {
        assert_ne!(hashes.len(), 0);
        assert!(hashes.len() <= (self.block_size / <Sha256 as Hasher>::Digest::DIGEST_LEN));

        let mut hasher = Sha256::default();
        let salt_size = self.salt.len() as u8;
        if salt_size > 0 {
            hasher.update(&self.salt);
            if salt_size % SHA256_SALT_PADDING != 0 {
                hasher.update(&vec![
                    0;
                    (SHA256_SALT_PADDING - salt_size % SHA256_SALT_PADDING) as usize
                ])
            }
        }

        for hash in hashes.iter() {
            hasher.update(&hash.bytes());
        }
        for _ in 0..((self.block_size / <Sha256 as Hasher>::Digest::DIGEST_LEN) - hashes.len()) {
            hasher.update(&[0; <Sha256 as Hasher>::Digest::DIGEST_LEN]);
        }

        hasher.finish()
    }
}

impl HasherTrait<Sha512> for Sha512Struct {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn hash_size(&self) -> usize {
        <Sha512 as Hasher>::Digest::DIGEST_LEN
    }

    /// Computes the MerkleTree digest from a `block` of data.
    ///
    /// For `Sha512Struct`, a MerkleTree Digest is a SHA-512 hash of a block of data. The block
    /// will be zero filled if its len is less than self.block_size, except for when the first data
    /// block is completely empty. If self.salt.len() > 0, we prepend the block with self.salt
    /// which itself is zero filled up to SHA512_SALT_PADDING.
    ///
    /// # Panics
    ///
    /// Panics if `block.len()` exceeds `self.block_size`.
    fn hash_block(&self, block: &[u8]) -> <Sha512 as Hasher>::Digest {
        if block.is_empty() {
            // Empty files have a root hash of all zeroes.
            return Sha512Digest::from_bytes([0; <Sha512 as Hasher>::Digest::DIGEST_LEN]);
        }

        assert!(block.len() <= self.block_size);
        let mut hasher = Sha512::default();
        let salt_size = self.salt.len() as u8;
        if !block.is_empty() && salt_size > 0 {
            hasher.update(&self.salt);
            if salt_size % SHA512_SALT_PADDING != 0 {
                hasher.update(&vec![
                    0;
                    (SHA512_SALT_PADDING - salt_size % SHA512_SALT_PADDING) as usize
                ])
            }
        }

        hasher.update(block);
        // Zero fill block up to self.block_size(). As a special case, if the first data block is
        // completely empty, it is not zero filled.
        if block.len() != self.block_size {
            hasher.update(&vec![0; self.block_size - block.len()]);
        }
        hasher.finish()
    }

    /// Computes a MerkleTree digest from a block of SHA-512 `hashes`.
    ///
    /// Like `hash_block`, `hash_hashes` zero fills incomplete buffers and prepends the digests
    /// with a salt, which is zero filled up to SHA512_SALT_PADDING.
    ///
    /// # Panics
    ///
    /// Panics if any of the following conditions are met:
    /// - `hashes.len()` is 0
    /// - `hashes.len() > self.block_size / SHA-256 digest length`
    fn hash_hashes(&self, hashes: &[<Sha512 as Hasher>::Digest]) -> <Sha512 as Hasher>::Digest {
        assert_ne!(hashes.len(), 0);
        assert!(hashes.len() <= (self.block_size / <Sha512 as Hasher>::Digest::DIGEST_LEN));

        let mut hasher = Sha512::default();
        let salt_size = self.salt.len() as u8;
        if salt_size > 0 {
            hasher.update(&self.salt);
            if salt_size % SHA512_SALT_PADDING != 0 {
                hasher.update(&vec![
                    0;
                    (SHA512_SALT_PADDING - salt_size % SHA512_SALT_PADDING) as usize
                ])
            }
        }

        for hash in hashes.iter() {
            hasher.update(&hash.bytes());
        }
        for _ in 0..((self.block_size / <Sha512 as Hasher>::Digest::DIGEST_LEN) - hashes.len()) {
            hasher.update(&[0; <Sha512 as Hasher>::Digest::DIGEST_LEN]);
        }

        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;

    #[test]
    fn test_hash_block_empty_sha256() {
        let hasher = Sha256Struct::new(vec![0xFF; 8], 4096);
        let block = [];
        let hash = hasher.hash_block(&block[..]).bytes();
        assert_eq!(hash, [0; 32]);
    }

    #[test]
    fn test_hash_block_empty_sha512() {
        let hasher = Sha512Struct::new(vec![0xFF; 8], 4096);
        let block = [];
        let hash = hasher.hash_block(&block[..]).bytes();
        assert_eq!(hash, [0; 64]);
    }

    #[test]
    fn test_hash_block_partial_block_sha256() {
        let hasher = Sha256Struct::new(vec![0xFF; 8], 4096);
        let block = vec![0xFF; hasher.block_size()];
        let mut block2: Vec<u8> = vec![0xFF; hasher.block_size() / 2];
        block2.append(&mut vec![0; hasher.block_size() / 2]);
        let hash = hasher.hash_block(&block[..]).bytes();
        let expected = hasher.hash_block(&block[..]).bytes();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_block_partial_block_sha512() {
        let hasher = Sha512Struct::new(vec![0xFF; 8], 4096);
        let block = vec![0xFF; hasher.block_size()];
        let mut block2: Vec<u8> = vec![0xFF; hasher.block_size() / 2];
        block2.append(&mut vec![0; hasher.block_size() / 2]);
        let hash = hasher.hash_block(&block[..]).bytes();
        let expected = hasher.hash_block(&block[..]).bytes();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_block_single_sha256() {
        let hasher = Sha256Struct::new(vec![0xFF; 8], 4096);
        let block = vec![0xFF; hasher.block_size()];
        let hash = hasher.hash_block(&block[..]).bytes();
        // Root hash of file size 4096 = block_size
        let expected: [u8; 32] =
            FromHex::from_hex("207f18729b037894447f948b81f63abe68007d0cd7c99a4ae0a3e323c52013a5")
                .unwrap();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_block_single_sha512() {
        let hasher = Sha512Struct::new(vec![0xFF; 8], 4096);
        let block = vec![0xFF; hasher.block_size()];
        let hash = hasher.hash_block(&block[..]).bytes();
        // Root hash of file size 4096 = block_size
        let expected: [u8; 64] = FromHex::from_hex("96d217a5f593384eb266b4bb2574b93c145ff1fd5ca89af52af6d4a14d2ce5200b2ddad30771c7cbcd139688e1a3847da7fd681490690adc945c3776154c42f6").unwrap();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_hashes_full_block_sha256() {
        let hasher = Sha256Struct::new(vec![0xFF; 8], 4096);
        let mut leafs = Vec::new();
        {
            let block = vec![0xFF; hasher.block_size()];
            for _i in 0..hasher.block_size() / hasher.hash_size() {
                leafs.push(hasher.hash_block(&block));
            }
        }
        let root = hasher.hash_hashes(&leafs).bytes();
        // Root hash of file size 524288 = block_size * (block_size / hash_size) = 4096 * (4096 / 32)
        let expected: [u8; 32] =
            FromHex::from_hex("827c28168aba953cf74706d4f3e776bd8892f6edf7b25d89645409f24108fb0b")
                .unwrap();
        assert_eq!(root, expected);
    }

    #[test]
    fn test_hash_hashes_full_block_sha512() {
        let hasher = Sha512Struct::new(vec![0xFF; 8], 4096);
        let mut leafs = Vec::new();
        {
            let block = vec![0xFF; hasher.block_size()];
            for _i in 0..hasher.block_size() / hasher.hash_size() {
                leafs.push(hasher.hash_block(&block));
            }
        }
        let root = hasher.hash_hashes(&leafs).bytes();
        // Root hash of file size 262144 = block_size * (block_size / hash_size) = 4096 * (4096 / 64)
        let expected: [u8; 64] = FromHex::from_hex("17d1728518330e0d48951ba43908ea7ad73ea018597643aabba9af2e43dea70468ba54fa09f9c7d02b1c240bd8009d1abd49c05559815a3b73ce31c5c26f93ba").unwrap();
        assert_eq!(root, expected);
    }

    #[test]
    fn test_hash_hashes_zero_pad_same_length_sha256() {
        let hasher = Sha256Struct::new(vec![0xFF; 8], 4096);
        let data_hash = hasher.hash_block(&vec![0xFF; hasher.block_size()]).bytes();
        let zero_hash = Sha256Digest::from_bytes([0; 32]);
        let hash_of_single_hash = hasher.hash_hashes(&[Sha256Digest::from_bytes(data_hash)]);
        let hash_of_single_hash_and_zero_hash =
            hasher.hash_hashes(&[Sha256Digest::from_bytes(data_hash), zero_hash]);
        assert_eq!(hash_of_single_hash, hash_of_single_hash_and_zero_hash);
    }

    #[test]
    fn test_hash_hashes_zero_pad_same_length_sha512() {
        let hasher = Sha512Struct::new(vec![0xFF; 8], 4096);
        let data_hash = hasher.hash_block(&vec![0xFF; hasher.block_size()]).bytes();
        let zero_hash = Sha512Digest::from_bytes([0; 64]);
        let hash_of_single_hash = hasher.hash_hashes(&[Sha512Digest::from_bytes(data_hash)]);
        let hash_of_single_hash_and_zero_hash =
            hasher.hash_hashes(&[Sha512Digest::from_bytes(data_hash), zero_hash]);
        assert_eq!(hash_of_single_hash, hash_of_single_hash_and_zero_hash);
    }
}
