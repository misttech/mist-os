// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `fsverity_merkle` contains types and methods for building and working with fsverity merkle trees.

mod builder;
pub use crate::builder::MerkleTreeBuilder;

mod tree;
pub use crate::tree::MerkleTree;

mod util;
pub use crate::util::{FsVerityHasher, FsVerityHasherOptions};

pub const SHA256_SALT_PADDING: u8 = 64;
pub const SHA512_SALT_PADDING: u8 = 128;

/// Compute a merkle tree from a `&[u8]` for a particular hasher.
pub fn from_slice(slice: &[u8], hasher: FsVerityHasher) -> MerkleTree {
    let mut builder = MerkleTreeBuilder::new(hasher);
    builder.write(slice);
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_slice_sha256() {
        let file = vec![0xFF; 2105344];
        let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let mut builder = MerkleTreeBuilder::new(hasher.clone());
        builder.write(&file[..]);
        let expected = builder.finish();
        let actual = from_slice(&file[..], hasher);
        assert_eq!(expected.root(), actual.root());
    }

    #[test]
    fn test_from_slice_sha512() {
        let file = vec![0xFF; 2105344];
        let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(vec![0xFF; 8], 4096));
        let mut builder = MerkleTreeBuilder::new(hasher.clone());
        builder.write(&file[..]);
        let expected = builder.finish();
        let actual = from_slice(&file[..], hasher);
        assert_eq!(expected.root(), actual.root());
    }
}
