// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Generic traits for configuration.

use fuchsia_inspect::Node;
use fuchsia_runtime::{take_startup_handle, HandleInfo, HandleType};

pub trait Config: Sized {
    /// Take the config startup handle and parse its contents.
    ///
    /// # Panics
    ///
    /// If the config startup handle was already taken or if it is not valid.
    fn take_from_startup_handle() -> Self {
        let handle_info = HandleInfo::new(HandleType::ComponentConfigVmo, 0);
        let config_vmo: zx::Vmo =
            take_startup_handle(handle_info).expect("Config VMO handle must be present.").into();
        Self::from_vmo(&config_vmo).expect("Config VMO handle must be valid.")
    }

    /// Parse `Self` from `vmo`.
    fn from_vmo(vmo: &zx::Vmo) -> Result<Self, Error> {
        let config_size = vmo.get_content_size().map_err(Error::GettingContentSize)?;
        let config_bytes = vmo.read_to_vec(0, config_size).map_err(Error::ReadingConfigBytes)?;
        Self::from_bytes(&config_bytes)
    }

    /// Parse `Self` from `bytes`.
    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>;

    /// Record config into inspect node.
    fn record_inspect(&self, inspector_node: &Node);
}

#[derive(Debug)]
pub enum Error {
    /// Failed to read the content size of the VMO.
    GettingContentSize(zx::Status),
    /// Failed to read the content of the VMO.
    ReadingConfigBytes(zx::Status),
    /// The VMO was too small for this config library.
    TooFewBytes,
    /// The VMO's config ABI checksum did not match this library's.
    ChecksumMismatch { expected_checksum: Vec<u8>, observed_checksum: Vec<u8> },
    /// Failed to parse the non-checksum bytes of the VMO as this library's FIDL type.
    Unpersist(fidl::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GettingContentSize(status) => {
                write!(f, "Failed to get content size: {status}")
            }
            Self::ReadingConfigBytes(status) => {
                write!(f, "Failed to read VMO content: {status}")
            }
            Self::TooFewBytes => {
                write!(f, "VMO content is not large enough for this config library.")
            }
            Self::ChecksumMismatch { expected_checksum, observed_checksum } => {
                write!(
                    f,
                    "ABI checksum mismatch, expected {:?}, got {:?}",
                    expected_checksum, observed_checksum,
                )
            }
            Self::Unpersist(fidl_error) => {
                write!(f, "Failed to parse contents of config VMO: {fidl_error}")
            }
        }
    }
}

impl std::error::Error for Error {
    #[allow(unused_parens, reason = "rustfmt errors without parens here")]
    fn source(&self) -> Option<(&'_ (dyn std::error::Error + 'static))> {
        match self {
            Self::GettingContentSize(ref status) | Self::ReadingConfigBytes(ref status) => {
                Some(status)
            }
            Self::TooFewBytes => None,
            Self::ChecksumMismatch { .. } => None,
            Self::Unpersist(ref fidl_error) => Some(fidl_error),
        }
    }
    fn description(&self) -> &str {
        match self {
            Self::GettingContentSize(_) => "getting content size",
            Self::ReadingConfigBytes(_) => "reading VMO contents",
            Self::TooFewBytes => "VMO contents too small",
            Self::ChecksumMismatch { .. } => "ABI checksum mismatch",
            Self::Unpersist(_) => "FIDL parsing error",
        }
    }
}
