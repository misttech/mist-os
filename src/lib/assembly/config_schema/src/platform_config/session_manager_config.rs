// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Platform configuration options for the session manager.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformSessionManagerConfig {
    #[serde(default)]
    pub enabled: bool,

    /// If `autolaunch` is true (the default) and the `session_url` is set in
    /// the `ProductConfig`, the named session will be launched when the device
    /// boots up.
    #[serde(default = "autolaunch_default")]
    pub autolaunch: bool,
}

fn autolaunch_default() -> bool {
    true
}
