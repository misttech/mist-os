// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the input area.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
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
    /// If set, the device's real time clock is only ever read from, but
    /// not written to.
    #[serde(default)]
    pub time_source_endpoint_url: String,
    /// If set, Timekeeper will serve test-only protocols from the library
    /// `fuchsia.time.test`.
    #[serde(default)]
    pub serve_test_protocols: bool,
}

impl Default for TimekeeperConfig {
    fn default() -> Self {
        // Values applied here are taken from static configuration defaults.
        Self {
            back_off_time_between_pull_samples_sec: 300,
            first_sampling_delay_sec: 0,
            time_source_endpoint_url: "https://clients3.google.com/generate_204".into(),
            serve_test_protocols: false,
        }
    }
}
