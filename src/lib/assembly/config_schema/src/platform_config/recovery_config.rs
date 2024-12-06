// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for recovery.
#[derive(
    Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct RecoveryConfig {
    /// Whether to include the factory-reset-trigger package.
    pub factory_reset_trigger: bool,

    /// Which system_recovery implementation to include
    pub system_recovery: Option<SystemRecovery>,

    /// The path to the logo for the recovery process to use.
    ///
    /// This must be a rive file (.riv).
    #[file_relative_paths]
    pub logo: Option<FileRelativePathBuf>,

    /// The path to the instructions to display.
    ///
    /// This file must be raw text for displaying.
    #[file_relative_paths]
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
