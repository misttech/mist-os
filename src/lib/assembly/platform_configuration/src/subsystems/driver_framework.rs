// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_capabilities::{Config, ConfigNestedValueType, ConfigValueType};
use assembly_config_schema::platform_config::driver_framework_config::{
    DriverFrameworkConfig, DriverHostCrashPolicy, TestFuzzingConfig,
};

pub(crate) struct DriverFrameworkSubsystemConfig;
impl DefineSubsystemConfiguration<DriverFrameworkConfig> for DriverFrameworkSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        driver_framework_config: &DriverFrameworkConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let mut disabled_drivers = driver_framework_config.disabled_drivers.clone();
        // TODO(https://fxbug.dev/42052994): Remove this once DFv2 is enabled by default and there
        // exists only one da7219 driver.
        disabled_drivers.push("fuchsia-boot:///#meta/da7219.cm".to_string());
        builder.platform_bundle("driver_framework_v2");

        let delay_fallback = !matches!(context.feature_set_level, FeatureSupportLevel::Bootstrap);

        let test_fuzzing_config =
            driver_framework_config.test_fuzzing_config.as_ref().unwrap_or(&TestFuzzingConfig {
                enable_load_fuzzer: false,
                max_load_delay_ms: 0,
                enable_test_shutdown_delays: false,
            });

        builder.set_config_capability(
            "fuchsia.driver.EnableEphemeralDrivers",
            Config::new(ConfigValueType::Bool, matches!(context.build_type, BuildType::Eng).into()),
        )?;
        builder.set_config_capability(
            "fuchsia.driver.DelayFallbackUntilBaseDriversIndexed",
            Config::new(ConfigValueType::Bool, delay_fallback.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.driver.BindEager",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 100 },
                    max_count: 20,
                },
                driver_framework_config.eager_drivers.clone().into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.driver.EnableDriverLoadFuzzer",
            Config::new(ConfigValueType::Bool, test_fuzzing_config.enable_load_fuzzer.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.driver.DriverLoadFuzzerMaxDelayMs",
            Config::new(ConfigValueType::Int64, test_fuzzing_config.max_load_delay_ms.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.driver.DisabledDrivers",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 100 },
                    max_count: 20,
                },
                disabled_drivers.into(),
            ),
        )?;

        let driver_host_crash_policy = driver_framework_config
            .driver_host_crash_policy
            .as_ref()
            .unwrap_or(&DriverHostCrashPolicy::RestartDriverHost);

        builder
            .package("driver_manager")
            .component("meta/driver_manager.cm")?
            .field("set_root_driver_host_critical", true)?
            .field("delay_fallback_until_base_drivers_indexed", delay_fallback)?
            .field("suspend_timeout_fallback", true)?
            .field("verbose", false)?
            .field("use_driver_framework_v2", true)?
            .field("driver_host_crash_policy", format!("{driver_host_crash_policy}"))?
            .field("root_driver", "fuchsia-boot:///platform-bus#meta/platform-bus.cm")?
            .field(
                "enable_test_shutdown_delays",
                test_fuzzing_config.enable_test_shutdown_delays,
            )?;

        Ok(())
    }
}
