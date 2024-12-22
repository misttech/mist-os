// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the input area.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TimekeeperConfig {
    /// The time to wait until retrying to sample the pull time source,
    /// expressed in seconds.
    pub back_off_time_between_pull_samples_sec: i64,
    /// The time to wait before sampling the time source for the first time,
    /// expressed in seconds.
    pub first_sampling_delay_sec: i64,
    /// If set, the device's real time clock is only ever read from, but
    /// not written to.
    pub time_source_endpoint_url: String,
    /// If set, Timekeeper will serve test-only protocols from the library
    /// `fuchsia.time.test`.
    pub serve_test_protocols: bool,
    /// If set, the UTC clock will be started if we attempt to read the RTC,
    /// but the reading of the RTC is known invalid.
    pub utc_start_at_startup_when_invalid_rtc: bool,
    /// If set, Timekeeper will serve `fuchsia.time.alarms` and will connect
    /// to the appropriate hardware device to do so.
    pub serve_fuchsia_time_alarms: bool,
}

impl Default for TimekeeperConfig {
    fn default() -> Self {
        // Values applied here are taken from static configuration defaults.
        Self {
            back_off_time_between_pull_samples_sec: 300,
            first_sampling_delay_sec: 0,
            time_source_endpoint_url: "https://clients3.google.com/generate_204".into(),
            serve_test_protocols: false,
            utc_start_at_startup_when_invalid_rtc: false,
            serve_fuchsia_time_alarms: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_default_serde() {
        let v: TimekeeperConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(v, Default::default());
    }
}
