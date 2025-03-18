// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

use assembly_container::{assembly_container, AssemblyContainer, DirectoryPathBuf, WalkPaths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A container that is full of board input bundles that can be shipped between
/// repositories. This container reduces the api contract between repositories
/// by hiding all the BIBs inside a single container. Boards can include an
/// entire set of BIBs at once.
///
/// In the future, we may implement flags in a BIB set that allows consumers to
/// include/exclude specific BIBs.
///
/// ```
/// fuchsia_board_configuration(
///   name = "my_board",
///   board_input_bundle_sets = [ ":board_input_bundle_set" ],
/// )
/// ```
///
#[derive(Debug, Deserialize, Serialize, WalkPaths)]
#[serde(deny_unknown_fields)]
#[assembly_container(board_input_bundle_set.json)]
pub struct BoardInputBundleSet {
    /// The name of the BIB set.
    pub name: String,

    /// A map of BIB names to their configs which include their directories.
    #[walk_paths]
    pub board_input_bundles: BTreeMap<String, BoardInputBundleEntry>,
}

/// A single BIB in the BIB set.
#[derive(Debug, Deserialize, Serialize, WalkPaths)]
#[serde(deny_unknown_fields)]
pub struct BoardInputBundleEntry {
    /// The directory of the BIB.
    #[walk_paths]
    pub path: DirectoryPathBuf,
}
