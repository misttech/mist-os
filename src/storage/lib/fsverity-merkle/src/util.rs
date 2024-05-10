// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{SHA256_SALT_PADDING, SHA512_SALT_PADDING};
use mundane::hash::{Digest, Hasher, Sha256, Sha512};
use std::fmt;

/// `FsVerityHasherOptions` contains relevant metadata for the FsVerityHasher. The `salt` is set
/// according to the FsverityMetadata struct stored in fxfs and `block_size` is that of the
/// filesystem.
#[derive(Clone)]
pub struct FsVerityHasherOptions {
    salt: Vec<u8>,
    block_size: usize,
    fsverity: bool,
}

impl FsVerityHasherOptions {
    pub fn new(salt: Vec<u8>, block_size: usize) -> Self {
        FsVerityHasherOptions { salt, block_size, fsverity: true }
    }

    pub fn new_dmverity(salt: Vec<u8>, block_size: usize) -> Self {
        FsVerityHasherOptions { salt, block_size, fsverity: false }
    }
}

/// `FsVerityHasher` is used by fsverity to construct merkle trees for verity-enabled files.
/// `FsVerityHasher` is parameterized by a salt and a block size.
#[derive(Clone)]
pub enum FsVerityHasher {
    Sha256(FsVerityHasherOptions),
    Sha512(FsVerityHasherOptions),
}

impl fmt::Debug for FsVerityHasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsVerityHasher::Sha256(metadata) => f
                .debug_struct("FsVerityHasher::Sha256")
                .field("salt", &metadata.salt)
                .field("block_size", &metadata.block_size)
                .finish(),
            FsVerityHasher::Sha512(metadata) => f
                .debug_struct("FsVerityHasher::Sha512")
                .field("salt", &metadata.salt)
                .field("block_size", &metadata.block_size)
                .finish(),
        }
    }
}

impl FsVerityHasher {
    pub fn block_size(&self) -> usize {
        match self {
            FsVerityHasher::Sha256(metadata) => metadata.block_size,
            FsVerityHasher::Sha512(metadata) => metadata.block_size,
        }
    }

    pub fn hash_size(&self) -> usize {
        match self {
            FsVerityHasher::Sha256(_) => <Sha256 as Hasher>::Digest::DIGEST_LEN,
            FsVerityHasher::Sha512(_) => <Sha512 as Hasher>::Digest::DIGEST_LEN,
        }
    }

    pub fn fsverity(&self) -> bool {
        match &self {
            FsVerityHasher::Sha256(metadata) => metadata.fsverity,
            FsVerityHasher::Sha512(metadata) => metadata.fsverity,
        }
    }

    /// Computes the MerkleTree digest from a `block` of data.
    ///
    /// A MerkleTree digest is a hash of a block of data. The block will be zero filled if its
    /// len is less than the block_size, except for when the first data block is completely empty.
    /// If `salt.len() > 0`, we prepend the block with the salt which itself is zero filled up
    /// to the padding.
    ///
    /// # Panics
    ///
    /// Panics if `block.len()` exceeds `self.block_size()`.
    pub fn hash_block(&self, block: &[u8]) -> Vec<u8> {
        match self {
            FsVerityHasher::Sha256(metadata) => {
                if block.is_empty() {
                    // Empty files have a root hash of all zeroes.
                    return vec![0; <Sha256 as Hasher>::Digest::DIGEST_LEN];
                }
                assert!(block.len() <= metadata.block_size);
                let mut hasher = Sha256::default();
                let salt_size = metadata.salt.len() as u8;

                if salt_size > 0 {
                    hasher.update(&metadata.salt);
                    if metadata.fsverity && salt_size % SHA256_SALT_PADDING != 0 {
                        hasher.update(&vec![
                            0;
                            (SHA256_SALT_PADDING - salt_size % SHA256_SALT_PADDING)
                                as usize
                        ])
                    }
                }

                hasher.update(block);
                // Zero fill block up to self.block_size(). As a special case, if the first data
                // block is completely empty, it is not zero filled.
                if block.len() != metadata.block_size {
                    hasher.update(&vec![0; metadata.block_size - block.len()]);
                }
                hasher.finish().bytes().to_vec()
            }
            FsVerityHasher::Sha512(metadata) => {
                if block.is_empty() {
                    // Empty files have a root hash of all zeroes.
                    return vec![0; <Sha512 as Hasher>::Digest::DIGEST_LEN];
                }
                assert!(block.len() <= metadata.block_size);
                let mut hasher = Sha512::default();
                let salt_size = metadata.salt.len() as u8;

                if salt_size > 0 {
                    hasher.update(&metadata.salt);
                    if metadata.fsverity && salt_size % SHA512_SALT_PADDING != 0 {
                        hasher.update(&vec![
                            0;
                            (SHA512_SALT_PADDING - salt_size % SHA512_SALT_PADDING)
                                as usize
                        ])
                    }
                }

                hasher.update(block);
                // Zero fill block up to self.block_size(). As a special case, if the first data
                // block is completely empty, it is not zero filled.
                if block.len() != metadata.block_size {
                    hasher.update(&vec![0; metadata.block_size - block.len()]);
                }
                hasher.finish().bytes().to_vec()
            }
        }
    }

    /// Computes a MerkleTree digest from a block of `hashes`.
    ///
    /// Like `hash_block`, `hash_hashes` zero fills incomplete buffers and prepends the digests
    /// with a salt, which is zero filled up to the padding.
    ///
    /// # Panics
    ///
    /// Panics if any of the following conditions are met:
    /// - `hashes.len()` is 0
    /// - `hashes.len() > self.block_size() / digest length`
    pub fn hash_hashes(&self, hashes: &[Vec<u8>]) -> Vec<u8> {
        assert_ne!(hashes.len(), 0);
        match self {
            FsVerityHasher::Sha256(metadata) => {
                assert!(
                    hashes.len() <= (metadata.block_size / <Sha256 as Hasher>::Digest::DIGEST_LEN)
                );
                let mut hasher = Sha256::default();
                let salt_size = metadata.salt.len() as u8;
                if salt_size > 0 {
                    hasher.update(&metadata.salt);
                    if metadata.fsverity && salt_size % SHA256_SALT_PADDING != 0 {
                        hasher.update(&vec![
                            0;
                            (SHA256_SALT_PADDING - salt_size % SHA256_SALT_PADDING)
                                as usize
                        ])
                    }
                }

                for hash in hashes {
                    hasher.update(hash.as_slice());
                }
                for _ in 0..((metadata.block_size / <Sha256 as Hasher>::Digest::DIGEST_LEN)
                    - hashes.len())
                {
                    hasher.update(&[0; <Sha256 as Hasher>::Digest::DIGEST_LEN]);
                }

                hasher.finish().bytes().to_vec()
            }
            FsVerityHasher::Sha512(metadata) => {
                assert!(
                    hashes.len() <= (metadata.block_size / <Sha512 as Hasher>::Digest::DIGEST_LEN)
                );

                let mut hasher = Sha512::default();
                let salt_size = metadata.salt.len() as u8;
                if salt_size > 0 {
                    hasher.update(&metadata.salt);
                    if metadata.fsverity && salt_size % SHA512_SALT_PADDING != 0 {
                        hasher.update(&vec![
                            0;
                            (SHA512_SALT_PADDING - salt_size % SHA512_SALT_PADDING)
                                as usize
                        ])
                    }
                }

                for hash in hashes {
                    hasher.update(hash.as_slice());
                }
                for _ in 0..((metadata.block_size / <Sha512 as Hasher>::Digest::DIGEST_LEN)
                    - hashes.len())
                {
                    hasher.update(&[0; <Sha512 as Hasher>::Digest::DIGEST_LEN]);
                }

                hasher.finish().bytes().to_vec()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;

    #[test]
    fn test_hash_block_empty_sha256() {
        let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let block = [];
        let hash = hasher.hash_block(&block[..]);
        assert_eq!(hash, [0; 32]);
    }

    #[test]
    fn test_hash_block_empty_sha512() {
        let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let block = [];
        let hash = hasher.hash_block(&block[..]);
        assert_eq!(hash, [0; 64]);
    }

    #[test]
    fn test_hash_block_partial_block_sha256() {
        let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let block = vec![0xFF; hasher.block_size()];
        let mut block2: Vec<u8> = vec![0xFF; hasher.block_size() / 2];
        block2.append(&mut vec![0; hasher.block_size() / 2]);
        let hash = hasher.hash_block(&block[..]);
        let expected = hasher.hash_block(&block[..]);
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_block_partial_block_sha512() {
        let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let block = vec![0xFF; hasher.block_size()];
        let mut block2: Vec<u8> = vec![0xFF; hasher.block_size() / 2];
        block2.append(&mut vec![0; hasher.block_size() / 2]);
        let hash = hasher.hash_block(&block[..]);
        let expected = hasher.hash_block(&block[..]);
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_block_single_sha256() {
        let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let block = vec![0xFF; hasher.block_size()];
        let hash = hasher.hash_block(&block[..]);
        // Root hash of file size 4096 = block_size
        let expected: [u8; 32] =
            FromHex::from_hex("207f18729b037894447f948b81f63abe68007d0cd7c99a4ae0a3e323c52013a5")
                .unwrap();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_block_single_sha512() {
        let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let block = vec![0xFF; hasher.block_size()];
        let hash = hasher.hash_block(&block[..]);
        // Root hash of file size 4096 = block_size
        let expected: [u8; 64] = FromHex::from_hex("96d217a5f593384eb266b4bb2574b93c145ff1fd5ca89af52af6d4a14d2ce5200b2ddad30771c7cbcd139688e1a3847da7fd681490690adc945c3776154c42f6").unwrap();
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_hash_hashes_full_block_sha256() {
        let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let mut leafs = Vec::new();
        {
            let block = vec![0xFF; hasher.block_size()];
            for _i in 0..hasher.block_size() / hasher.hash_size() {
                leafs.push(hasher.hash_block(&block));
            }
        }
        let root = hasher.hash_hashes(&leafs);
        // Root hash of file size 524288 = block_size * (block_size / hash_size) = 4096 * (4096 / 32)
        let expected: [u8; 32] =
            FromHex::from_hex("827c28168aba953cf74706d4f3e776bd8892f6edf7b25d89645409f24108fb0b")
                .unwrap();
        assert_eq!(root, expected);
    }

    #[test]
    fn test_hash_hashes_full_block_sha512() {
        let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let mut leafs = Vec::new();
        {
            let block = vec![0xFF; hasher.block_size()];
            for _i in 0..hasher.block_size() / hasher.hash_size() {
                leafs.push(hasher.hash_block(&block));
            }
        }
        let root = hasher.hash_hashes(&leafs);
        // Root hash of file size 262144 = block_size * (block_size / hash_size) = 4096 * (4096 / 64)
        let expected: [u8; 64] = FromHex::from_hex("17d1728518330e0d48951ba43908ea7ad73ea018597643aabba9af2e43dea70468ba54fa09f9c7d02b1c240bd8009d1abd49c05559815a3b73ce31c5c26f93ba").unwrap();
        assert_eq!(root, expected);
    }

    #[test]
    fn test_hash_hashes_zero_pad_same_length_sha256() {
        let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let data_hash = hasher.hash_block(&vec![0xFF; hasher.block_size()]);
        let zero_hash = vec![0; 32];
        let hash_of_single_hash = hasher.hash_hashes(&[data_hash.clone()]);
        let hash_of_single_hash_and_zero_hash = hasher.hash_hashes(&[data_hash, zero_hash]);
        assert_eq!(hash_of_single_hash, hash_of_single_hash_and_zero_hash);
    }

    #[test]
    fn test_hash_hashes_zero_pad_same_length_sha512() {
        let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let data_hash = hasher.hash_block(&vec![0xFF; hasher.block_size()]);
        let zero_hash = vec![0; 64];
        let hash_of_single_hash = hasher.hash_hashes(&[data_hash.clone()]);
        let hash_of_single_hash_and_zero_hash = hasher.hash_hashes(&[data_hash, zero_hash]);
        assert_eq!(hash_of_single_hash, hash_of_single_hash_and_zero_hash);
    }
}
