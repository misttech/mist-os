// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::common::PackageSet;

/// This struct provides information about the "board" that a product is being
/// assembled to run on.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
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

    /// This is the bundle of board-specific artifacts that the Fuchsia platform
    /// needs added to the assembled system in order to be able to boot Fuchsia
    /// on this board.
    ///
    /// Examples:
    ///  - the "board driver"
    ///  - storage drivers
    ///
    /// If any of these artifacts are removed, even the 'bootstrap' feature set
    /// may be unable to boot.
    pub main_support_bundle: Option<HardwareSupportBundle>,
}

/// This struct defines a bundle of artifacts that can be included by the board
/// in the assembled image.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HardwareSupportBundle {
    /// These are the drivers that are included by this bundle.
    pub drivers: Vec<PackagedDriverDetails>,
}

/// This defines one or more drivers in a package, and which package set they
/// belong to.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PackagedDriverDetails {
    /// The package containing the driver.
    pub package: Utf8PathBuf,

    /// Which set this package belongs to.
    pub set: PackageSet,

    /// The driver components within the package, e.g. meta/foo.cm.
    pub components: Vec<Utf8PathBuf>,
}

#[cfg(test)]
mod test {
    use super::*;

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
    fn test_complete_board_deserialize() {
        let json = serde_json::json!({
            "name": "sample board",
            "provided_features": [
                "feature_a",
                "feature_b"
            ],
            "main_support_bundle": {
                "drivers": [
                    {
                        "package": "path/to/package_manifest.json",
                        "set": "base",
                        "components": [ "meta/foo.cm" ]
                    }
                ]
            }
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let expected = BoardInformation {
            name: "sample board".to_owned(),
            provided_features: vec!["feature_a".into(), "feature_b".into()],
            main_support_bundle: Some(HardwareSupportBundle {
                drivers: vec![PackagedDriverDetails {
                    package: "path/to/package_manifest.json".into(),
                    set: PackageSet::Base,
                    components: vec!["meta/foo.cm".into()],
                }],
            }),
        };

        assert_eq!(parsed, expected);
    }
}
