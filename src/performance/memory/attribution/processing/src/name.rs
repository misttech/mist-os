// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bstr::BStr;
use serde::Serialize;
use zerocopy::{FromBytes, Immutable, KnownLayout};
use zx_types::ZX_MAX_NAME_LEN;

// Performance note: rust string is made of a struct and a heap allocation which uses 3 and 2 64bit
// values for struct fields and heap block header for a total of 40 bytes. Hence it is always more
// efficient to use ZXName.

// TODO(b/411121120): remove this struct when zx::Name is usable on host.

/// Zircon resource name with a maximum length of `ZX_MAX_NAME_LEN - 1`.
#[derive(
    Eq, Hash, FromBytes, Immutable, KnownLayout, PartialEq, PartialOrd, Ord, Serialize, Clone,
)]
pub struct ZXName([u8; ZX_MAX_NAME_LEN]);

#[derive(Debug, PartialEq)]
pub enum Error {
    InvalidArgument,
}

impl ZXName {
    fn as_bstr(&self) -> &BStr {
        BStr::new(match self.0.iter().position(|&b| b == 0) {
            Some(index) => &self.0[..index],
            None => &self.0[..],
        })
    }

    pub const fn try_from_bytes(b: &[u8]) -> Result<Self, Error> {
        if b.len() >= ZX_MAX_NAME_LEN {
            return Err(Error::InvalidArgument);
        }

        let mut inner = [0u8; ZX_MAX_NAME_LEN];
        let mut i = 0;
        while i < b.len() {
            if b[i] == 0 {
                return Err(Error::InvalidArgument);
            }
            inner[i] = b[i];
            i += 1;
        }

        Ok(Self(inner))
    }

    pub fn from_string_lossy(s: &str) -> Self {
        Self::from_bytes_lossy(s.as_bytes())
    }

    #[inline]
    pub const fn from_bytes_lossy(b: &[u8]) -> Self {
        let to_copy = if b.len() <= ZX_MAX_NAME_LEN - 1 { b.len() } else { ZX_MAX_NAME_LEN - 1 };

        let mut inner = [0u8; ZX_MAX_NAME_LEN];
        let mut source_idx = 0;
        let mut dest_idx = 0;
        while source_idx < to_copy {
            if b[source_idx] != 0 {
                inner[dest_idx] = b[source_idx];
                dest_idx += 1;
            }
            source_idx += 1;
        }

        Self(inner)
    }
}

impl std::ops::Deref for ZXName {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::fmt::Display for ZXName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Escapes non utf-8 sequences.
        std::fmt::Display::fmt(self.as_bstr(), f)
    }
}

impl std::fmt::Debug for ZXName {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_bstr(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_name() {
        for empty in [
            &ZXName::try_from_bytes(b"").unwrap(),
            &ZXName::from_bytes_lossy(b""),
            &ZXName::from_string_lossy(""),
            ZXName::ref_from_bytes(&[0u8; ZX_MAX_NAME_LEN]).unwrap(),
        ] {
            assert_eq!(0, empty.as_bstr().len());
            assert_eq!(empty, empty);
            assert_eq!("", empty.to_string());
        }
    }

    #[test]
    fn just_fit() {
        let data = "abcdefghijklmnopqrstuvwxyz01234";
        for name in [
            &ZXName::try_from_bytes(data.as_bytes()).unwrap(),
            &ZXName::from_bytes_lossy(data.as_bytes()),
            &ZXName::from_string_lossy(data),
            ZXName::ref_from_bytes(b"abcdefghijklmnopqrstuvwxyz01234\0").unwrap(),
        ] {
            assert_eq!("abcdefghijklmnopqrstuvwxyz01234", name.to_string());
            assert_eq!(ZX_MAX_NAME_LEN - 1, name.to_string().len());
        }
    }

    #[test]
    fn too_long() {
        let data = "abcdefghijklmnopqrstuvwxyz012345";
        assert_eq!(Result::Err(Error::InvalidArgument), ZXName::try_from_bytes(data.as_bytes()));

        for name in [ZXName::from_bytes_lossy(data.as_bytes()), ZXName::from_string_lossy(data)] {
            assert_eq!("abcdefghijklmnopqrstuvwxyz01234", name.to_string());
            assert_eq!(ZX_MAX_NAME_LEN - 1, name.to_string().len());
        }
    }

    #[test]
    fn zero_inside() {
        let data = b"abc\0def\0\0\0";
        assert_eq!(Err(Error::InvalidArgument), ZXName::try_from_bytes(data));
        assert_eq!("abcdef", ZXName::from_bytes_lossy(data).to_string());
    }

    #[test]
    fn not_utf8() {
        let data: [u8; 2] = [0xff, 0xff];
        assert_eq!("\u{FFFD}\u{FFFD}", ZXName::from_bytes_lossy(&data).to_string());
    }
}
