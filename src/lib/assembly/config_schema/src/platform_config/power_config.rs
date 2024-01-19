// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Platform configuration options for the starnix area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PowerConfig {
    /// Whether power suspend/resume is supported.
    #[serde(default)]
    pub suspend_enabled: bool,
}
