// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeMap;

use assembly_container::WalkPaths;
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for recovery.
#[derive(
    Debug,
    Default,
    Deserialize,
    Serialize,
    PartialEq,
    JsonSchema,
    SupportsFileRelativePaths,
    WalkPaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct RecoveryConfig {
    /// Whether to include the factory-reset-trigger package.
    ///
    /// This field is deprecated, and ignored if the 'factory_reset_trigger_config'
    /// field is provided.
    pub factory_reset_trigger: bool,

    /// Include the factory-reset-trigger package, and configure it using the given file.
    ///
    /// This is a a map of channel names to indices, when the current OTA
    /// channel matches one of the names in the file, if a stored index is less
    /// than the index value in the file, a factory reset is triggered.
    pub factory_reset_trigger_config: Option<BTreeMap<String, i32>>,

    /// Which system_recovery implementation to include
    pub system_recovery: Option<SystemRecovery>,

    /// The path to the logo for the recovery process to use.
    ///
    /// This must be a rive file (.riv).
    #[file_relative_paths]
    #[walk_paths]
    pub logo: Option<FileRelativePathBuf>,

    /// The path to the instructions to display.
    ///
    /// This file must be raw text for displaying.
    #[file_relative_paths]
    #[walk_paths]
    pub instructions: Option<FileRelativePathBuf>,

    /// Perform a managed-mode check before doing an FDR.
    pub check_for_managed_mode: bool,
}

/// Which system recovery implementation to include in the image
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SystemRecovery {
    Fdr,
}
