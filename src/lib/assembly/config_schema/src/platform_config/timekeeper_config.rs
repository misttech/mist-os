// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Platform configuration options for the input area.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TimekeeperConfig {
    /// The time to wait until retrying to sample the pull time source,
    /// expressed in seconds.
    #[serde(default)]
    pub back_off_time_between_pull_samples_sec: i64,
    /// The time to wait before sampling the time source for the first time,
    /// expressed in seconds.
    #[serde(default)]
    pub first_sampling_delay_sec: i64,
}

impl Default for TimekeeperConfig {
    fn default() -> Self {
        Self {
            // This is the default applied in static configs.
            back_off_time_between_pull_samples_sec: 300,
            // This is the default applied in static configs.
            first_sampling_delay_sec: 0,
        }
    }
}
