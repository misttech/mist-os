// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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

    /// Metadata about the board that's provided by the 'fuchsia.hwinfo.Board'
    /// protocol.
    pub hardware_info: Option<HardwareInfo>,

    /// The "features" that this board provides to the product.
    ///
    /// NOTE: This is a still-evolving, loosely-coupled, set of identifiers.
    /// It's an unstable interface between the boards and the platform.
    #[serde(default)]
    pub provided_features: Vec<String>,

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
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct HardwareInfo {
    pub name: String,
}

#[cfg(test)]
mod test {
    use super::*;
    use assembly_file_relative_path::SupportsFileRelativePaths;
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
                "name": "hwinfo_name"
            },
            "provided_features": [
                "feature_a",
                "feature_b"
            ],
            "input_bundles": [
                "bundle_a",
                "bundle_b"
            ]
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let resolved = parsed.resolve_paths_from_file(board_file).unwrap();

        let expected = BoardInformation {
            name: "sample board".to_owned(),
            hardware_info: Some(HardwareInfo { name: "hwinfo_name".into() }),
            provided_features: vec!["feature_a".into(), "feature_b".into()],
            input_bundles: vec![
                FileRelativePathBuf::Resolved("some/path/to/board/bundle_a".into()),
                FileRelativePathBuf::Resolved("some/path/to/board/bundle_b".into()),
            ],
            ..Default::default()
        };

        assert_eq!(resolved, expected);
    }
}
