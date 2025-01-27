// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the starnix area.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths)]
#[serde(default, deny_unknown_fields)]
pub struct PowerConfig {
    /// Whether power suspend/resume is supported.
    pub suspend_enabled: bool,

    /// Whether storage power management is supported.
    /// This will only work on |suspend_enabled| is also true.
    /// TODO(https://fxbug.dev/383772372): Remove when no longer needed.
    pub storage_power_management_enabled: bool,

    /// Whether the testing SAG with testing based controls
    /// should be used. This will only work when |suspend_enabled|
    /// is also true, as there is no SAG when suspend support is disabled.
    /// TODO(https://fxbug.dev/335526423): Remove when no longer needed.
    pub testing_sag_enabled: bool,

    /// Whether to include the power framework components that are needed
    /// for power system non-hermetic testing in the platform.
    pub enable_non_hermetic_testing: bool,

    /// Configuration of devices and drivers for power-metrics collection
    #[schemars(schema_with = "crate::option_path_schema")]
    #[file_relative_paths]
    pub metrics_logging_config: Option<FileRelativePathBuf>,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            suspend_enabled: false,
            storage_power_management_enabled: true,
            testing_sag_enabled: false,
            enable_non_hermetic_testing: false,
            metrics_logging_config: None,
        }
    }
}
