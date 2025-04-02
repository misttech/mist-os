// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for driver load testing.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TestFuzzingConfig {
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_load_fuzzer: bool,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub max_load_delay_ms: u64,

    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_test_shutdown_delays: bool,
}

fn is_true(val: &bool) -> bool {
    *val
}

/// Platform configuration options for driver framework support.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DriverFrameworkConfig {
    /// The list of driver components to load eagerly. Eager drivers are those that are forced to
    /// be non-fallback drivers, even if their manifest indicates they should be fallback.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub eager_drivers: Vec<String>,

    /// The list of drivers to disable. These drivers are skipped when encountered during the
    /// driver loading process.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_drivers: Vec<String>,

    /// Fuzzing configuration used for testing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_fuzzing_config: Option<TestFuzzingConfig>,

    /// Whether to enable the driver index's stop_on_idle feature, where it waits until it reaches
    /// an idle timeout, escrows its state and handles, then exits. If unspecified, this will
    /// default to `false`.
    /// See: https://fuchsia.dev/fuchsia-src/development/components/stop_idle
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enable_driver_index_stop_on_idle: bool,

    /// Whether to use the rust driver host or to use the c++ one instead.
    /// If unspecified, this will default to `true`.
    #[serde(skip_serializing_if = "is_true")]
    pub use_rust_driver_host: bool,
}

impl Default for DriverFrameworkConfig {
    fn default() -> Self {
        Self {
            eager_drivers: Vec::new(),
            disabled_drivers: Vec::new(),
            test_fuzzing_config: None,
            enable_driver_index_stop_on_idle: false,
            use_rust_driver_host: true,
        }
    }
}
