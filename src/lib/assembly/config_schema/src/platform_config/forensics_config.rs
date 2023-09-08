// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Configuration options for the forensics area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ForensicsConfig {
    #[serde(default)]
    pub feedback: FeedbackConfig,
}

/// Configuration options for the feedback configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackConfig {
    pub low_memory: bool,
}
