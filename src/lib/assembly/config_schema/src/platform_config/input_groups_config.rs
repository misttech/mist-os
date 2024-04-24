// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the opaque input groups.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputGroupsConfig {
    #[serde(default)]
    pub group1: bool,

    #[serde(default)]
    pub group2: bool,
}
