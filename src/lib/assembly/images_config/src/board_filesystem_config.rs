// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// The board options for configuring the filesystem.
/// The options include those derived from the partition table and what the bootloader expects.
/// A board developer can specify options for many different filesystems, and let the product
/// choose which filesystem to actually create.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct BoardFilesystemConfig {
    /// Required board configuration for a zbi. All assemblies must produce a ZBI.
    #[serde(default)]
    pub zbi: Zbi,

    /// Board configuration for a vbmeta if necessary. The bootloader determines whether a vbmeta
    /// is necessary, therefore this is an optional board-level argument. fxfs and fvm below are
    /// chosen by the product, therefore those variables are always available for all boards.
    #[serde(default)]
    #[file_relative_paths]
    pub vbmeta: Option<VBMeta>,

    /// Board configuration for a fxfs if requested by the product. If the product does not
    /// request a fxfs, then these values are ignored.
    #[serde(default)]
    pub fxfs: Fxfs,

    /// Board configuration for a fvm if requested by the product. If the product does not
    /// request a fvm, then these values are ignored.
    #[serde(default)]
    pub fvm: Fvm,

    /// Permit multiple GPT devices to be matched.
    #[serde(default)]
    pub gpt_all: bool,
}

/// Parameters describing how to generate the ZBI.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Zbi {
    /// The compression format for the ZBI.
    #[serde(default)]
    pub compression: ZbiCompression,

    /// An optional script to post-process the ZBI.
    /// This is often used to prepare the ZBI for flashing/updating.
    #[serde(default)]
    pub postprocessing_script: Option<PostProcessingScript>,
}

/// The compression format for the ZBI.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum ZbiCompression {
    /// zstd compression.
    #[serde(rename = "zstd")]
    #[default]
    ZStd,

    /// zstd.max compression.
    #[serde(rename = "zstd.max")]
    ZStdMax,
}

/// A script to process the ZBI after it is constructed.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessingScript {
    /// The path to the script on host.
    /// This script _musts_ take the following arguments:
    ///   -z <path to ZBI>
    ///   -o <output path>
    ///   -B <build directory, relative to script's source directory>
    pub path: Utf8PathBuf,

    /// Additional arguments to pass to the script after the above arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// The parameters describing how to create a VBMeta image.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct VBMeta {
    /// Path on host to the key for signing VBMeta.
    pub key: FileRelativePathBuf,

    /// Path on host to the key metadata to add to the VBMeta.
    pub key_metadata: FileRelativePathBuf,

    /// Optional descriptors to add to the VBMeta image.
    #[serde(default)]
    pub additional_descriptors: Vec<VBMetaDescriptor>,
}

/// The parameters of a VBMeta descriptor to add to a VBMeta image.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VBMetaDescriptor {
    /// Name of the partition.
    pub name: String,

    /// Size of the partition in bytes.
    pub size: u64,

    /// Custom VBMeta flags to add.
    pub flags: u32,

    /// Minimum AVB version to add.
    pub min_avb_version: String,
}

/// The parameters describing how to create an Fxfs image.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fxfs {
    /// If `target_size` bytes is set, the raw image will be set to exactly this
    /// size (and an error is returned if the contents exceed that size).  If
    /// unset (or 0), the image will be truncated to twice the size of its
    /// contents, which is a heuristic that gives us roughly enough space for
    /// normal usage of the image.
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

/// The parameters describing how to create a FVM image.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fvm {
    /// Slice size (in bytes) for an FVM if requested by a product. If the product does not
    /// request a minfs, then these values are ignored.
    #[serde(default)]
    pub slice_size: FvmSliceSize,

    /// If provided, the standard fvm will be truncated to the specified length.
    #[serde(default)]
    pub truncate_to_length: Option<u64>,

    /// Board configuration for a blobfs if requested by a product. If the product does not
    /// request a blobfs, then these values are ignored.
    #[serde(default)]
    pub blobfs: Blobfs,

    /// Board configuration for a minfs if requested by the product. If the product does not
    /// request a minfs, then these values are ignored.
    #[serde(default)]
    pub minfs: Minfs,

    /// If specified, a sparse fvm will be built with the supplied configuration.
    #[serde(default)]
    pub sparse_output: Option<SparseFvmConfig>,

    /// If specified, a nand fvm will be built with the supplied configuration.
    #[serde(default)]
    pub nand_output: Option<NandFvmConfig>,

    /// If specified, a fvm will be built with the supplied configuration that is optimized for
    /// fastboot flashing.
    #[serde(default)]
    pub fastboot_output: Option<FastbootFvmConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FvmSliceSize(pub u64);
impl Default for FvmSliceSize {
    fn default() -> Self {
        Self(8388608)
    }
}

/// Configuration for building a Blobfs volume.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Blobfs {
    /// The maximum number of bytes we can place in blobfs during assembly.
    /// This value must be smaller than `maximum_bytes` which is the absolute maximum amount
    /// of space we can use in blobfs at runtime.
    #[serde(default)]
    pub size_checker_maximum_bytes: Option<u64>,

    /// Maximum number of bytes blobfs can consume at build time.
    /// This value is used by the fvm tool to preallocate space.
    /// Most boards should avoid setting this value, and set maximum_bytes instead.
    #[serde(default)]
    pub build_time_maximum_bytes: Option<u64>,

    /// Maximum number of bytes blobfs can consume at runtime.
    /// This value is placed in fshost config to enforce size budgets at runtime.
    #[serde(default)]
    pub maximum_bytes: Option<u64>,

    /// Minimum number of inodes to reserve in blobfs.
    #[serde(default)]
    pub minimum_inodes: Option<u64>,

    /// Minimum number of bytes to reserve for blob data.
    #[serde(default)]
    pub minimum_data_bytes: Option<u64>,
}

/// Configuration for building an Minfs volume.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Minfs {
    /// Maximum number of bytes minfs can consume at runtime.
    #[serde(default)]
    pub maximum_bytes: Option<u64>,
}

/// A FVM that is compressed sparse.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SparseFvmConfig {
    /// The maximum size the FVM can expand to at runtime.
    /// This sets the amount of slice metadata to allocate during construction,
    /// which cannot be modified at runtime.
    #[serde(default)]
    pub max_disk_size: Option<u64>,
}

/// A FVM prepared for a Nand partition.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NandFvmConfig {
    /// The maximum size the FVM can expand to at runtime.
    /// This sets the amount of slice metadata to allocate during construction,
    /// which cannot be modified at runtime.
    #[serde(default)]
    pub max_disk_size: Option<u64>,

    /// Whether to compress the FVM.
    #[serde(default)]
    pub compress: bool,

    /// The number of blocks.
    pub block_count: u64,

    /// The out of bound size.
    pub oob_size: u64,

    /// Page size as perceived by the FTL.
    pub page_size: u64,

    /// Number of pages per erase block unit.
    pub pages_per_block: u64,
}

/// A FVM prepared for fastboot flashing.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FastbootFvmConfig {
    /// Whether to compress the FVM.
    #[serde(default)]
    pub compress: bool,

    /// Truncate the file to this length.
    #[serde(default)]
    pub truncate_to_length: Option<u64>,
}
