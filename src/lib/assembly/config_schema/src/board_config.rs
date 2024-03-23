// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

use std::collections::BTreeSet;

use crate::common::{PackageDetails, PackagedDriverDetails};
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use assembly_images_config::BoardFilesystemConfig;
use serde::{Deserialize, Serialize};

/// This struct provides information about the "board" that a product is being
/// assembled to run on.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct BoardInformation {
    /// The name of the board.
    pub name: String,

    /// Metadata about the board that's provided to the 'fuchsia.hwinfo.Board'
    /// protocol and to the Board Driver via the PlatformID and BoardInfo ZBI
    /// items.
    #[serde(default)]
    pub hardware_info: HardwareInfo,

    /// The "features" that this board provides to the product.
    ///
    /// NOTE: This is a still-evolving, loosely-coupled, set of identifiers.
    /// It's an unstable interface between the boards and the platform.
    #[serde(default)]
    pub provided_features: Vec<String>,

    /// Path to the devicetree binary (.dtb) this provided by this board.
    #[serde(default)]
    #[file_relative_paths]
    pub devicetree: Option<FileRelativePathBuf>,

    /// Configuration for the various filesystems that the product can choose to
    /// include.
    #[serde(default)]
    #[file_relative_paths]
    pub filesystems: BoardFilesystemConfig,

    /// These are paths to the directories that are board input bundles that
    /// this board configuration includes.  Product assembly will always include
    /// these into the images that it creates.
    ///
    /// These are the board-specific artifacts that the Fuchsia platform needs
    /// added to the assembled system in order to be able to boot Fuchsia on
    /// this board.
    ///
    /// Examples:
    ///  - the "board driver"
    ///  - storage drivers
    ///
    /// If any of these artifacts are removed, even the 'bootstrap' feature set
    /// may be unable to boot.
    #[serde(default)]
    #[file_relative_paths]
    pub input_bundles: Vec<FileRelativePathBuf>,

    /// Consolidated configuration from all of the BoardInputBundles.  This is
    /// not deserialized from the BoardConfiguration, but is instead created by
    /// parsing each of the input_bundles and merging their configuration fields.
    #[serde(skip_deserializing)]
    #[file_relative_paths]
    pub configuration: BoardProvidedConfig,

    /// Configure kernel cmdline args
    /// TODO: Move this into platform section below
    #[serde(default)]
    pub kernel: BoardKernelConfig,

    /// Configure platform related feature
    #[serde(default)]
    pub platform: PlatformConfig,
}

/// This struct defines board-provided data for the 'fuchsia.hwinfo.Board' fidl
/// protocol and for the Platform_ID and Board_Info ZBI items.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HardwareInfo {
    /// This is the value returned in the 'BoardInfo.name' field, if different
    /// from the name provided for the board itself.  It's also the name that's
    /// set in the PLATFORM_ID ZBI Item.
    pub name: Option<String>,

    /// The vendor id to add to a PLATFORM_ID ZBI Item.
    pub vendor_id: Option<u32>,

    /// The product id to add to a PLATFORM_ID ZBI Item.
    pub product_id: Option<u32>,

    /// The board revision to add to a BOARD_INFO ZBI Item.
    pub revision: Option<u32>,
}

/// This struct defines a bundle of artifacts that can be included by the board
/// in the assembled image.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct BoardInputBundle {
    /// These are the drivers that are included by this bundle.
    #[file_relative_paths]
    pub drivers: Vec<PackagedDriverDetails>,

    /// These are the packages to include with this bundle.
    #[file_relative_paths]
    pub packages: Vec<PackageDetails>,

    /// These are kernel boot arguments that are to be passed to the kernel when
    /// this bundle is included in the assembled system.
    pub kernel_boot_args: BTreeSet<String>,

    /// Board-provided configuration for platform services.  Each field of this
    /// structure can only be provided by one of the BoardInputBundles that a
    /// BoardInformation uses.
    #[file_relative_paths]
    pub configuration: Option<BoardProvidedConfig>,
}

/// This struct defines board-provided configuration for platform services and
/// features, used if those services are included by the product's supplied
/// platform configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct BoardProvidedConfig {
    /// Configuration for the cpu-manager service
    #[file_relative_paths]
    pub cpu_manager: Option<FileRelativePathBuf>,

    /// Configuration for the power-manager service
    #[file_relative_paths]
    pub power_manager: Option<FileRelativePathBuf>,

    /// Configuration for the power metrics recorder service
    #[file_relative_paths]
    pub power_metrics_recorder: Option<FileRelativePathBuf>,

    /// Thermal configuration for the power-manager service
    #[file_relative_paths]
    pub thermal: Option<FileRelativePathBuf>,
}

/// This struct defines supported kernel features.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BoardKernelConfig {
    /// Enable the use of 'contiguous physical pages'. This should be enabled
    /// when a significant contiguous memory size is required.
    pub contiguous_physical_pages: bool,
}

/// This struct defines platform configurations specified by board.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    /// Configure connectivity related features
    pub connectivity: ConnectivityConfig,
}

/// This struct defines connectivity configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConnectivityConfig {
    /// Configure network related features
    pub network: NetworkConfig,
}

/// This struct defines network configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// This option instructs netsvc to use only the device whose topological
    /// path ends with the option's value. All other devices are ignored by
    /// netsvc. The topological path for a device can be determined from the
    /// shell by running the `lsdev` command on the device
    /// (e.g. `/dev/class/network/000` or `/dev/class/ethernet/000`).
    pub netsvc_interface: Option<String>,
}

#[cfg(test)]
mod test {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn test_basic_board_deserialize() {
        let json = serde_json::json!({
            "name": "sample board",
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let expected = BoardInformation { name: "sample board".to_owned(), ..Default::default() };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn test_complete_board_deserialize_with_relative_paths() {
        let board_dir = Utf8PathBuf::from("some/path/to/board");
        let board_file = board_dir.join("board_configuration.json");

        let json = serde_json::json!({
            "name": "sample board",
            "hardware_info": {
                "name": "hwinfo_name",
                "vendor_id": 1,
                "product_id": 2,
                "revision": 3,
            },
            "provided_features": [
                "feature_a",
                "feature_b"
            ],
            "input_bundles": [
                "bundle_a",
                "bundle_b"
            ],
            "devicetree": "test.dtb",
            "kernel": {
                "contiguous_physical_pages": true,
            }
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let resolved = parsed.resolve_paths_from_file(board_file).unwrap();

        let expected = BoardInformation {
            name: "sample board".to_owned(),
            hardware_info: HardwareInfo {
                name: Some("hwinfo_name".into()),
                vendor_id: Some(0x01),
                product_id: Some(0x02),
                revision: Some(0x03),
            },
            provided_features: vec!["feature_a".into(), "feature_b".into()],
            input_bundles: vec![
                FileRelativePathBuf::Resolved("some/path/to/board/bundle_a".into()),
                FileRelativePathBuf::Resolved("some/path/to/board/bundle_b".into()),
            ],
            devicetree: Some(FileRelativePathBuf::Resolved("some/path/to/board/test.dtb".into())),
            kernel: BoardKernelConfig { contiguous_physical_pages: true },
            platform: PlatformConfig::default(),
            ..Default::default()
        };

        assert_eq!(resolved, expected);
    }
}
