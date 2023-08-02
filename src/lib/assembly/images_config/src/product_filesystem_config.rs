// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// The product options for configuring the filesystem.
/// The options include which filesystems to build and how, but do not contain constraints derived
/// from the board or partition size.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProductFilesystemConfig {
    /// The filename to use for the zbi and vbmeta.
    #[serde(default)]
    pub image_name: ImageName,

    /// Make fshost watch for NAND devices.
    #[serde(default)]
    pub watch_for_nand: bool,

    /// If format_minfs_on_corruption is true (the default), fshost formats
    /// minfs partition on finding it corrupted.  Set to false to keep the
    /// devices in a corrupted state which might be of help to debug issues.
    #[serde(default)]
    pub format_data_on_corruption: FormatDataOnCorruption,

    /// Which volume to build to hold the filesystems.
    pub volume: VolumeConfig,
}

/// The filename to use for the zbi and vbmeta.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImageName(pub String);
impl Default for ImageName {
    fn default() -> Self {
        Self("fuchsia".to_string())
    }
}

/// Whether for format the data filesystem when a corruption is detected.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FormatDataOnCorruption(pub bool);
impl Default for FormatDataOnCorruption {
    fn default() -> Self {
        Self(true)
    }
}

/// Which volume should be built to hold data and blobs for this product.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum VolumeConfig {
    /// No volume.
    #[serde(rename = "none")]
    NoVolume,
    /// A fxfs volume.
    #[serde(rename = "fxfs")]
    #[default]
    Fxfs,
    /// A fvm volume.
    #[serde(rename = "fvm")]
    Fvm(FvmVolumeConfig),
}

/// A FVM volume.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FvmVolumeConfig {
    /// If specified, a data filesystem will be built for this product.
    #[serde(default)]
    pub data: Option<DataFvmVolumeConfig>,

    /// If specified, a blob filesystem will be built for this product.
    #[serde(default)]
    pub blob: Option<BlobFvmVolumeConfig>,

    /// If specified, bytes will be reserved in the fvm for this product.
    #[serde(default)]
    pub reserved: Option<ReservedFvmVolumeConfig>,
}

/// Configuration options for a data filesystem.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DataFvmVolumeConfig {
    /// If true, will enable content-detection for partition format, supporting
    /// both minfs and fxfs filesystems. A special "fs_switch" file can be
    /// written to the root directory containing the string "minfs", "fxfs" or
    /// "toggle" to trigger a migration from the current format to the specified
    /// format. (The "toggle" option will migrate back and forth at each boot.)
    #[serde(default)]
    pub use_disk_based_minfs_migration: bool,

    /// Set to one of "minfs", "fxfs", "f2fs" (unstable).
    /// If set to anything other than "minfs", any existing minfs partition will be
    /// migrated in-place to the specified format when fshost mounts it.
    /// Set by products
    #[serde(default)]
    pub data_filesystem_format: DataFilesystemFormat,
}

/// The data format to use inside the fvm.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum DataFilesystemFormat {
    /// A fxfs filesystem for persisting data.
    #[default]
    Fxfs,

    /// A minfs filesystem for persisting data.
    Minfs,
}

/// Configuration options for a blob filesystem.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlobFvmVolumeConfig {
    /// The format blobfs should store blobs in.
    #[serde(default)]
    pub blob_layout: BlobfsLayout,
}

/// The internal layout of blobfs.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum BlobfsLayout {
    /// A more compact layout than DeprecatedPadded.
    #[serde(rename = "compact")]
    #[default]
    Compact,

    /// A layout that is deprecated, but kept for compatibility reasons.
    #[serde(rename = "deprecated_padded")]
    DeprecatedPadded,
}

/// Configuration options for reserving space in the fvm.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReservedFvmVolumeConfig {
    /// The number of bytes to reserve in the fvm.
    pub reserved_bytes: u64,
}
