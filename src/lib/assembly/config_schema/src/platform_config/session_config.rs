// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the session.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformSessionConfig {
    #[serde(default)]
    pub enabled: bool,

    /// If `autolaunch` is true (the default) and the `session.url` is set in
    /// the `ProductConfig`, the named session will be launched when the device
    /// boots up.
    #[serde(default = "autolaunch_default")]
    pub autolaunch: bool,

    #[serde(default)]
    pub include_element_manager: bool,
}

impl Default for PlatformSessionConfig {
    fn default() -> Self {
        Self {
            enabled: Default::default(),
            autolaunch: autolaunch_default(),
            include_element_manager: Default::default(),
        }
    }
}

fn autolaunch_default() -> bool {
    true
}
