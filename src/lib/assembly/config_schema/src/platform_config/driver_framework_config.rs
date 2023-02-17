// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Platform configuration options for driver framework support.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DriverFrameworkConfig {
    #[serde(default)]
    pub eager_drivers: Vec<String>,

    #[serde(default)]
    pub disabled_drivers: Vec<String>,
}
