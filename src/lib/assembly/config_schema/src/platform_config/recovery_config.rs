// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for recovery.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    /// Whether to include the factory-reset-trigger package.
    #[serde(default)]
    pub factory_reset_trigger: bool,

    /// Which system_recovery implementation to include
    pub system_recovery: Option<SystemRecovery>,
}

/// Which system recovery implementation to include in the image
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SystemRecovery {
    Fdr,
}
