// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use assembly_util::read_config;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

/// The configuration file specifying where the generated images should be placed when flashing of
/// OTAing. This file lists the partitions used in three different flashing configurations:
///   fuchsia      - primary images in A/B, recovery in R, bootloaders, bootstrap
///   fuchsia_only - primary images in A/B, recovery in R, bootloaders
///   recovery     - recovery in A/B/R, bootloaders
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionsConfig {
    /// Partitions that are only flashed in "fuchsia" configurations.
    #[serde(default)]
    pub bootstrap_partitions: Vec<BootstrapPartition>,

    /// Partitions designated for bootloaders, which are not slot-specific.
    pub bootloader_partitions: Vec<BootloaderPartition>,

    /// Non-bootloader partitions, which are slot-specific.
    pub partitions: Vec<Partition>,

    /// The name of the hardware to assert before flashing images to partitions.
    pub hardware_revision: String,

    /// Zip files containing the fastboot unlock credentials.
    #[serde(default)]
    pub unlock_credentials: Vec<Utf8PathBuf>,
}

impl PartitionsConfig {
    /// Load a PartitionsConfig from a partitions_config.json file on disk,
    /// rebasing its paths appropriately.
    pub fn try_load_from(path: impl AsRef<Utf8Path>) -> Result<Self> {
        // Deserialize JSON into PartitionsConfig.
        let path = path.as_ref();
        let mut config: PartitionsConfig = read_config(path)?;

        // Determine relative base_path and rebase.
        // 1. Try to strip CWD from a `canonical_base_path` (the directory
        //    containing `partitions_config.json`) to determine a relative
        //    `base_path`.
        //    a. This helps prevent us from accidentally leaking/serializing the
        //       ninja out dir by users of fields.
        //    b. Fallback to the absolute `canonical_base_path` if it's not
        //       relative to CWD (i.e. `ffx product` shouldn't fail if it's
        //       invoked from a different CWD).
        // 2. Rebase paths to be relative to `base_path`, rather than CWD.
        //    This ensures that partition configs can be packaged into a
        //    portable directory.
        //    a. Since the path of the artifact itself may be a symlink that
        //       points outside of CWD (eg: to `//prebuilt` instead of
        //       `$root_build_dir`) don't canonicalize the effective path.
        let cwd = Utf8Path::new(".").canonicalize_utf8()?;
        let canonical_base_path = path
            .parent()
            .context("Determine base path")?
            .canonicalize_utf8()
            .context("Canonicalize base_path")?;
        let base_path = canonical_base_path
            .strip_prefix(cwd.as_path())
            .map(|v| v.to_path_buf())
            .unwrap_or_else(|_| canonical_base_path);
        for cred_path in &mut config.unlock_credentials {
            *cred_path = base_path.join(&cred_path);
        }
        for bootstrap in &mut config.bootstrap_partitions {
            bootstrap.image = base_path.join(&bootstrap.image);
        }
        for bootloader in &mut config.bootloader_partitions {
            bootloader.image = base_path.join(&bootloader.image);
        }
        Ok(config)
    }
}

/// A partition to flash in "fuchsia" configurations.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct BootstrapPartition {
    /// The name of the partition known to fastboot.
    pub name: String,

    /// The path on host to the bootloader image.
    pub image: Utf8PathBuf,

    /// The condition that must be met before attempting to flash.
    pub condition: Option<BootstrapCondition>,
}

/// The fastboot variable condition that must equal the value before a bootstrap partition should
/// be flashed.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct BootstrapCondition {
    /// The name of the fastboot variable.
    pub variable: String,

    /// The expected value.
    pub value: String,
}

/// A single bootloader partition, which is not slot-specific.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct BootloaderPartition {
    /// The firmware type provided to the update system.
    /// See documentation here:
    ///     https://fuchsia.dev/fuchsia-src/concepts/packages/update_pkg
    #[serde(rename = "type")]
    pub partition_type: String,

    /// The name of the partition known to fastboot.
    /// If the name is not provided, then the partition should not be flashed.
    pub name: Option<String>,

    /// The path on host to the bootloader image.
    pub image: Utf8PathBuf,
}

/// A non-bootloader partition which
#[derive(Clone, Debug, Deserialize, PartialOrd, Ord, Eq, PartialEq, Hash, Serialize)]
#[serde(tag = "type")]
pub enum Partition {
    /// A partition prepared for the Zircon Boot Image (ZBI).
    ZBI {
        /// The partition name.
        name: String,
        /// The slot of the partition.
        slot: Slot,
        /// An optional size constraint in bytes for the partition.
        size: Option<u64>,
    },

    /// A partition prepared for the Verified Boot Metadata (VBMeta).
    VBMeta {
        /// The partition name.
        name: String,
        /// The slot of the partition.
        slot: Slot,
        /// An optional size constraint for the partition.
        size: Option<u64>,
    },

    /// A partition prepared for the Fuchsia Volume Manager (FVM).
    FVM {
        /// The partition name.
        name: String,
        /// An optional size constraint for the partition.
        size: Option<u64>,
    },

    /// A partition prepared for Fxfs.
    Fxfs {
        /// The partition name.
        name: String,
        /// An optional size constraint for the partition.
        size: Option<u64>,
    },

    /// A partition preparted for a device tree binary overlay.
    Dtbo {
        /// The partition name.
        name: String,
        /// The slot of the partition.
        slot: Slot,
        /// An optional size constraint for the partition.
        size: Option<u64>,
    },
}

impl Partition {
    /// The name of the partition.
    pub fn name(&self) -> &String {
        match &self {
            Self::ZBI { name, .. } => name,
            Self::VBMeta { name, .. } => name,
            Self::FVM { name, .. } => name,
            Self::Fxfs { name, .. } => name,
            Self::Dtbo { name, .. } => name,
        }
    }

    /// The slot of the partition, if applicable.
    pub fn slot(&self) -> Option<&Slot> {
        match &self {
            Self::ZBI { slot, .. } => Some(slot),
            Self::VBMeta { slot, .. } => Some(slot),
            Self::FVM { .. } => None,
            Self::Fxfs { .. } => None,
            Self::Dtbo { slot, .. } => Some(slot),
        }
    }

    /// The size budget of the partition, if supplied.
    pub fn size(&self) -> Option<&u64> {
        match &self {
            Self::ZBI { size, .. } => size.as_ref(),
            Self::VBMeta { size, .. } => size.as_ref(),
            Self::FVM { size, .. } => size.as_ref(),
            Self::Fxfs { size, .. } => size.as_ref(),
            Self::Dtbo { size, .. } => size.as_ref(),
        }
    }
}

/// The slots available for flashing or OTAing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Slot {
    /// Primary slot.
    A,

    /// Alternate slot.
    B,

    /// Recovery slot.
    R,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_partition_config(json: &str, additional_files: &[&str]) -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        let mut partitions_config = File::create(base_path.join("partitions_config.json")).unwrap();
        partitions_config.write_all(json.as_bytes()).unwrap();

        additional_files.iter().for_each(|&file_name| {
            let mut file = File::create(base_path.join(file_name)).unwrap();
            file.write_all(file_name.as_bytes()).unwrap();
        });

        temp_dir
    }

    #[test]
    fn from_json() {
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "tpl",
                        name: "firmware_tpl",
                        image: "tpl_image",
                    }
                ],
                partitions: [
                    {
                        type: "ZBI",
                        name: "zircon_a",
                        slot: "A",
                    },
                    {
                        type: "VBMeta",
                        name: "vbmeta_b",
                        slot: "B",
                    },
                    {
                        type: "FVM",
                        name: "fvm",
                    },
                    {
                        type: "Fxfs",
                        name: "fxfs",
                    },
                    {
                        type: "Dtbo",
                        name: "dtbo_a",
                        slot: "A",
                    },
                    {
                        type: "Dtbo",
                        name: "dtbo_b",
                        slot: "B",
                    },
                ],
                hardware_revision: "hw",
                unlock_credentials: [
                    "unlock_credentials.zip",
                ],
            }
        "#;
        let temp_dir = write_partition_config(json, &["tpl_image", "unlock_credentials.zip"]);
        let test_dir = Utf8Path::from_path(temp_dir.path()).unwrap();

        let config =
            PartitionsConfig::try_load_from(test_dir.join("partitions_config.json")).unwrap();

        assert_eq!(config.bootloader_partitions[0].image, test_dir.join("tpl_image"));
        assert_eq!(config.unlock_credentials[0], test_dir.join("unlock_credentials.zip"));
        assert_eq!(config.partitions.len(), 6);
        assert_eq!(config.hardware_revision, "hw");
    }

    #[test]
    fn invalid_partition_type() {
        let json = r#"
            {
                bootloader_partitions: [],
                partitions: [
                    {
                        type: "Invalid",
                        name: "zircon",
                        slot: "SlotA",
                    }
                ],
                "hardware_revision": "hw",
            }
        "#;
        let temp_dir = write_partition_config(json, &[]);
        let test_dir = Utf8Path::from_path(temp_dir.path()).unwrap();

        let config = PartitionsConfig::try_load_from(test_dir.join("partitions_config.json"));

        assert!(config.is_err());
    }
}
