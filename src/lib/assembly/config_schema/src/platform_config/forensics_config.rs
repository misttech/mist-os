// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration options for the forensics area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, WalkPaths)]
#[serde(default, deny_unknown_fields)]
pub struct ForensicsConfig {
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub feedback: FeedbackConfig,

    #[walk_paths]
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub cobalt: CobaltConfig,
}

/// Configuration options for the feedback configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FeedbackConfig {
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub large_disk: bool,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub remote_device_id_provider: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_ts_feedback_id_component_url: Option<String>,

    /// Whether to include the last few kernel logs in the last reboot info.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub include_kernel_logs_in_last_reboot_info: bool,
}

/// Configuration options for the cobalt configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, WalkPaths)]
#[serde(default, deny_unknown_fields)]
pub struct CobaltConfig {
    #[schemars(schema_with = "crate::option_path_schema")]
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<Utf8PathBuf>,
}
