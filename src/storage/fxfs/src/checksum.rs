// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::errors::FxfsError,
    anyhow::Error,
    byteorder::{ByteOrder, LittleEndian},
    fprint::TypeFingerprint,
    serde::{Deserialize, Serialize},
    static_assertions::assert_cfg,
    zerocopy::{AsBytes as _, FromBytes as _},
};

/// For the foreseeable future, Fxfs will use 64-bit checksums.
pub type Checksum = u64;

/// Generates a Fletcher64 checksum of |buf| seeded by |previous|.
///
/// All logfile blocks are covered by a fletcher64 checksum as the last 8 bytes in a block.
///
/// We also use this checksum for integrity validation of potentially out-of-order writes
/// during Journal replay.
pub fn fletcher64(buf: &[u8], previous: Checksum) -> Checksum {
    assert!(buf.len() % 4 == 0);
    let mut lo = previous as u32;
    let mut hi = (previous >> 32) as u32;
    for chunk in buf.chunks(4) {
        lo = lo.wrapping_add(LittleEndian::read_u32(chunk));
        hi = hi.wrapping_add(lo);
    }
    (hi as u64) << 32 | lo as u64
}

/// A vector of fletcher64 checksums, one per block.
/// These are stored as a flat array of bytes for efficient deserialization.
pub type Checksums = ChecksumsV38;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct ChecksumsV38 {
    sums: Vec<u8>,
}

impl Checksums {
    pub fn fletcher(checksums: Vec<Checksum>) -> Self {
        assert_cfg!(target_endian = "little");
        let checksums_as_u8: &[u8] = &*checksums.as_bytes();
        Self { sums: checksums_as_u8.to_owned() }
    }

    pub fn len(&self) -> usize {
        self.sums.len() / std::mem::size_of::<Checksum>()
    }

    pub fn maybe_as_ref(&self) -> Result<&[Checksum], Error> {
        assert_cfg!(target_endian = "little");
        Checksum::slice_from(&self.sums).ok_or(FxfsError::Inconsistent.into())
    }

    pub fn offset_by(&self, amount: usize) -> Self {
        Checksums { sums: self.sums[amount * std::mem::size_of::<Checksum>()..].to_vec() }
    }

    pub fn shrunk(&self, len: usize) -> Self {
        Checksums { sums: self.sums[..len * std::mem::size_of::<Checksum>()].to_vec() }
    }
}

#[derive(Debug, Serialize, Deserialize, TypeFingerprint)]
pub enum ChecksumsV37 {
    None,
    Fletcher(Vec<u8>),
}

impl ChecksumsV37 {
    pub fn fletcher(checksums: Vec<Checksum>) -> Self {
        assert_cfg!(target_endian = "little");
        let checksums_as_u8: &[u8] = &*checksums.as_bytes();
        Self::Fletcher(checksums_as_u8.to_owned())
    }

    pub fn migrate(self) -> Option<Checksums> {
        match self {
            Self::None => None,
            Self::Fletcher(sums) => Some(Checksums { sums }),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, TypeFingerprint)]
pub enum ChecksumsV32 {
    None,
    Fletcher(Vec<u64>),
}

impl From<ChecksumsV32> for ChecksumsV37 {
    fn from(checksums: ChecksumsV32) -> Self {
        match checksums {
            ChecksumsV32::None => Self::None,
            ChecksumsV32::Fletcher(sums) => Self::fletcher(sums),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{checksum::Checksums, errors::FxfsError};

    #[test]
    fn checksum_encoding_idempotent() {
        let mut checksums = vec![0xabu64 << 56, 0x11002200u64, u64::MAX, 0];
        checksums.reserve_exact(5);

        let encoded = Checksums::fletcher(checksums.clone());
        let decoded = encoded.maybe_as_ref().unwrap();

        assert_eq!(decoded, &checksums[..]);
    }

    #[test]
    fn deserialize_invalid_checksum() {
        let bad = Checksums { sums: vec![0, 1, 2, 3, 4, 5, 6] };
        let res = bad.maybe_as_ref().expect_err("deserialization should fail");
        assert!(FxfsError::Inconsistent.matches(&res));
    }
}
