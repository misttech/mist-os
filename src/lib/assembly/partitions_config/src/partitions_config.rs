// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use assembly_container::{assembly_container, AssemblyContainer, WalkPaths};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// The configuration file specifying where the generated images should be placed when flashing of
/// OTAing. This file lists the partitions used in three different flashing configurations:
///   fuchsia      - primary images in A/B, recovery in R, bootloaders, bootstrap
///   fuchsia_only - primary images in A/B, recovery in R, bootloaders
///   recovery     - recovery in A/B/R, bootloaders
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, WalkPaths)]
#[serde(deny_unknown_fields)]
#[assembly_container(partitions_config.json)]
pub struct PartitionsConfig {
    /// Partitions that are only flashed in "fuchsia" configurations.
    #[serde(default)]
    #[walk_paths]
    pub bootstrap_partitions: Vec<BootstrapPartition>,

    /// Partitions designated for bootloaders, which are not slot-specific.
    #[serde(default)]
    #[walk_paths]
    pub bootloader_partitions: Vec<BootloaderPartition>,

    /// Non-bootloader partitions, which are slot-specific.
    #[serde(default)]
    pub partitions: Vec<Partition>,

    /// The name of the hardware to assert before flashing images to partitions.
    pub hardware_revision: String,

    /// Zip files containing the fastboot unlock credentials.
    #[serde(default)]
    #[walk_paths]
    pub unlock_credentials: Vec<Utf8PathBuf>,
}

impl PartitionsConfig {
    /// Determine which recovery style we will be using, and throw an error if we find both AB and R
    /// style recoveries.
    pub fn recovery_style(&self) -> Result<RecoveryStyle> {
        let mut recovery_style = RecoveryStyle::NoRecovery;
        for partition in &self.partitions {
            if partition.slot() == Some(&Slot::R) {
                if recovery_style == RecoveryStyle::AB {
                    bail!("Partitions config cannot contain both AB and R slotted recoveries.");
                }
                recovery_style = RecoveryStyle::R;
            }

            if matches!(partition, Partition::RecoveryZBI { .. }) {
                if recovery_style == RecoveryStyle::R {
                    bail!("Partitions config cannot contain both AB and R slotted recoveries.");
                }
                recovery_style = RecoveryStyle::AB;
            }
        }
        Ok(recovery_style)
    }
}

/// A partition to flash in "fuchsia" configurations.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, WalkPaths)]
pub struct BootstrapPartition {
    /// The name of the partition known to fastboot.
    pub name: String,

    /// The path on host to the bootloader image.
    #[walk_paths]
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
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, WalkPaths)]
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
    #[walk_paths]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },

    /// A partition prepared for the Recovery Zircon Boot Image (ZBI).
    /// This is for AB-slotted recovery images.
    /// R-slotted recovery images should use Partition::ZBI and Slot::R.
    RecoveryZBI {
        /// The partition name.
        name: String,
        /// The slot of the partition.
        slot: Slot,
        /// An optional size constraint in bytes for the partition.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },

    /// A partition prepared for the Verified Boot Metadata (VBMeta).
    VBMeta {
        /// The partition name.
        name: String,
        /// The slot of the partition.
        slot: Slot,
        /// An optional size constraint for the partition.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },

    /// A partition prepared for the Recovery Verified Boot Metadata (VBMeta).
    RecoveryVBMeta {
        /// The partition name.
        name: String,
        /// The slot of the partition.
        slot: Slot,
        /// An optional size constraint for the partition.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },

    /// A partition prepared for the Fuchsia Volume Manager (FVM).
    FVM {
        /// The partition name.
        name: String,
        /// An optional size constraint for the partition.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },

    /// A partition prepared for Fxfs.
    Fxfs {
        /// The partition name.
        name: String,
        /// An optional size constraint for the partition.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },

    /// A partition preparted for a device tree binary overlay.
    Dtbo {
        /// The partition name.
        name: String,
        /// The slot of the partition.
        slot: Slot,
        /// An optional size constraint for the partition.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
}

impl Partition {
    /// The name of the partition.
    pub fn name(&self) -> &String {
        match &self {
            Self::ZBI { name, .. } => name,
            Self::RecoveryZBI { name, .. } => name,
            Self::VBMeta { name, .. } => name,
            Self::RecoveryVBMeta { name, .. } => name,
            Self::FVM { name, .. } => name,
            Self::Fxfs { name, .. } => name,
            Self::Dtbo { name, .. } => name,
        }
    }

    /// The slot of the partition, if applicable.
    pub fn slot(&self) -> Option<&Slot> {
        match &self {
            Self::ZBI { slot, .. } => Some(slot),
            Self::RecoveryZBI { slot, .. } => Some(slot),
            Self::VBMeta { slot, .. } => Some(slot),
            Self::RecoveryVBMeta { slot, .. } => Some(slot),
            Self::FVM { .. } => None,
            Self::Fxfs { .. } => None,
            Self::Dtbo { slot, .. } => Some(slot),
        }
    }

    /// The size budget of the partition, if supplied.
    pub fn size(&self) -> Option<&u64> {
        match &self {
            Self::ZBI { size, .. } => size.as_ref(),
            Self::RecoveryZBI { size, .. } => size.as_ref(),
            Self::VBMeta { size, .. } => size.as_ref(),
            Self::RecoveryVBMeta { size, .. } => size.as_ref(),
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

impl std::fmt::Display for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::A => "A",
            Self::B => "B",
            Self::R => "R",
        };
        write!(f, "{}", message)
    }
}

/// The style of recovery.
#[derive(Debug, PartialEq)]
pub enum RecoveryStyle {
    /// No recovery images are present.
    NoRecovery,
    /// Recovery lives in a separate R slot.
    R,
    /// Recovery is updated alongside the "main" images in AB slots.
    AB,
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
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
                        type: "RecoveryZBI",
                        name: "recovery_a",
                        slot: "A",
                    },
                    {
                        type: "VBMeta",
                        name: "vbmeta_b",
                        slot: "B",
                    },
                    {
                        type: "RecoveryVBMeta",
                        name: "vbmeta_recovery_b",
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

        let config = PartitionsConfig::from_dir(test_dir).unwrap();

        assert_eq!(config.bootloader_partitions[0].image, test_dir.join("tpl_image"));
        assert_eq!(config.unlock_credentials[0], test_dir.join("unlock_credentials.zip"));
        assert_eq!(config.partitions.len(), 8);
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

        let config = PartitionsConfig::from_dir(test_dir);

        assert!(config.is_err());
    }
}
