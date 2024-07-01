// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::FeatureControl;

fn default_allowed() -> FeatureControl {
    FeatureControl::Allowed
}

/// Platform configuration options for paravirtualization.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformParavirtualizationConfig {
    #[serde(default = "default_allowed")]
    pub enabled: FeatureControl,
}

impl Default for PlatformParavirtualizationConfig {
    fn default() -> Self {
        Self {
            // Match the value given to serde when the field is omitted.
            enabled: default_allowed(),
        }
    }
}
