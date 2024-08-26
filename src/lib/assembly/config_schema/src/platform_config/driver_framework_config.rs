// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration options for how to act when a driver host crashes.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum DriverHostCrashPolicy {
    RestartDriverHost,
    RebootSystem,
    DoNothing,
}

/// Platform configuration options for driver load testing.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestFuzzingConfig {
    #[serde(default)]
    pub enable_load_fuzzer: bool,

    #[serde(default)]
    pub max_load_delay_ms: u64,

    #[serde(default)]
    pub enable_test_shutdown_delays: bool,
}

impl std::fmt::Display for DriverHostCrashPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriverHostCrashPolicy::RestartDriverHost => write!(f, "restart-driver-host"),
            DriverHostCrashPolicy::RebootSystem => write!(f, "reboot-system"),
            DriverHostCrashPolicy::DoNothing => write!(f, "do-nothing"),
        }
    }
}

/// Platform configuration options for driver framework support.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverFrameworkConfig {
    /// The list of driver components to load eagerly. Eager drivers are those that are forced to
    /// be non-fallback drivers, even if their manifest indicates they should be fallback.
    #[serde(default)]
    pub eager_drivers: Vec<String>,

    /// The list of drivers to disable. These drivers are skipped when encountered during the
    /// driver loading process.
    #[serde(default)]
    pub disabled_drivers: Vec<String>,

    /// The policy that determines what happens when driver hosts crash. This is not used since the
    /// DFv2 migration as the crash policy is determined by each root driver in the driver host.
    /// https://fuchsia.dev/fuchsia-src/concepts/components/v2/driver_runner#host-restart-on-crash
    #[serde(default)]
    pub driver_host_crash_policy: Option<DriverHostCrashPolicy>,

    /// Fuzzing configuration used for testing.
    #[serde(default)]
    pub test_fuzzing_config: Option<TestFuzzingConfig>,

    /// Whether to enable the driver index's stop_on_idle feature, where it waits until it reaches
    /// an idle timeout, escrows its state and handles, then exits. If unspecified, this will
    /// default to `true`.
    /// See: https://fuchsia.dev/fuchsia-src/development/components/stop_idle
    #[serde(default)]
    pub enable_driver_index_stop_on_idle: Option<bool>,
}
