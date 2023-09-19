// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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

    /// This is the path to the directory that contains the bundle of
    /// board-specific artifacts that the Fuchsia platform needs added to the
    /// assembled system in order to be able to boot Fuchsia on this board.
    ///
    /// Examples:
    ///  - the "board driver"
    ///  - storage drivers
    ///
    /// If any of these artifacts are removed, even the 'bootstrap' feature set
    /// may be unable to boot.
    #[file_relative_paths]
    pub main_bundle: Option<FileRelativePathBuf>,
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
            "provided_features": [
                "feature_a",
                "feature_b"
            ],
            "main_bundle": "main_bundle",
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let resolved = parsed.resolve_paths_from_file(board_file).unwrap();

        let expected = BoardInformation {
            name: "sample board".to_owned(),
            provided_features: vec!["feature_a".into(), "feature_b".into()],
            main_bundle: Some("some/path/to/board/main_bundle".into()),
            ..Default::default()
        };

        assert_eq!(resolved, expected);
    }
}
