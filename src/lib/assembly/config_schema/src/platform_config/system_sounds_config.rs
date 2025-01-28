// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for system sounds
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
pub struct SystemSoundsConfig {
    /// earcon configuration
    #[file_relative_paths]
    #[walk_paths]
    pub earcons: Option<Earcons>,
}

/// Earcons are "audible icons"
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
pub struct Earcons {
    /// Sound to play on bluetooth connection
    #[schemars(schema_with = "crate::option_path_schema")]
    #[file_relative_paths]
    #[walk_paths]
    pub bluetooth_connected: Option<FileRelativePathBuf>,

    /// Sound to play on bluetooth disconnect
    #[schemars(schema_with = "crate::option_path_schema")]
    #[file_relative_paths]
    #[walk_paths]
    pub bluetooth_disconnected: Option<FileRelativePathBuf>,

    /// Sound to play when changing volume
    #[schemars(schema_with = "crate::option_path_schema")]
    #[file_relative_paths]
    #[walk_paths]
    pub volume_changed: Option<FileRelativePathBuf>,

    /// Sound to play when reaching max volume
    #[schemars(schema_with = "crate::option_path_schema")]
    #[file_relative_paths]
    #[walk_paths]
    pub volume_max_reached: Option<FileRelativePathBuf>,

    /// Sound to play on system start
    #[schemars(schema_with = "crate::option_path_schema")]
    #[file_relative_paths]
    #[walk_paths]
    pub system_start: Option<FileRelativePathBuf>,
}
