// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for driver load testing.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TestFuzzingConfig {
    pub enable_load_fuzzer: bool,
    pub max_load_delay_ms: u64,
    pub enable_test_shutdown_delays: bool,
}

/// Platform configuration options for driver framework support.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DriverFrameworkConfig {
    /// The list of driver components to load eagerly. Eager drivers are those that are forced to
    /// be non-fallback drivers, even if their manifest indicates they should be fallback.
    pub eager_drivers: Vec<String>,

    /// The list of drivers to disable. These drivers are skipped when encountered during the
    /// driver loading process.
    pub disabled_drivers: Vec<String>,

    /// Fuzzing configuration used for testing.
    pub test_fuzzing_config: Option<TestFuzzingConfig>,

    /// Whether to enable the driver index's stop_on_idle feature, where it waits until it reaches
    /// an idle timeout, escrows its state and handles, then exits. If unspecified, this will
    /// default to `true`.
    /// See: https://fuchsia.dev/fuchsia-src/development/components/stop_idle
    pub enable_driver_index_stop_on_idle: bool,
}
