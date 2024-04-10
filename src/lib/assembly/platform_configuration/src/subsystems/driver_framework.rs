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
        // This is a temporary change to include register driver through
        // register platform AIB. It will be removed after we move the driver
        // into bootstrap AIB.
        if context.board_info.provides_feature("fuchsia::platform_driver_migration") {
            builder.platform_bundle("register_driver");
        }

        // This is always set to false, but should be configurable once drivers actually support
        // using a hardware iommu.
        builder.set_config_capability(
            "fuchsia.driver.UseHardwareIommu",
            Config::new(ConfigValueType::Bool, false.into()),
        )?;

        let mut disabled_drivers = driver_framework_config.disabled_drivers.clone();
        // TODO(https://fxbug.dev/42052994): Remove this once DFv2 is enabled by default and there
        // exists only one da7219 driver.
        disabled_drivers.push("fuchsia-boot:///#meta/da7219.cm".to_string());
        builder.platform_bundle("driver_framework");

        let enable_ephemeral_drivers = match (context.build_type, context.feature_set_level) {
            (BuildType::Eng, FeatureSupportLevel::Standard) => {
                builder.platform_bundle("full_package_drivers");
                true
            }
            (_, _) => false,
        };

        let delay_fallback = !matches!(context.feature_set_level, FeatureSupportLevel::Bootstrap);

        let test_fuzzing_config =
            driver_framework_config.test_fuzzing_config.as_ref().unwrap_or(&TestFuzzingConfig {
                enable_load_fuzzer: false,
                max_load_delay_ms: 0,
                enable_test_shutdown_delays: false,
            });

        builder.set_config_capability(
            "fuchsia.driver.EnableEphemeralDrivers",
            Config::new(ConfigValueType::Bool, enable_ephemeral_drivers.into()),
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

        builder.set_config_capability(
            "fuchsia.driver.manager.DriverHostCrashPolicy",
            Config::new(
                ConfigValueType::String { max_size: 20 },
                format!("{driver_host_crash_policy}").into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.driver.manager.RootDriver",
            Config::new(
                ConfigValueType::String { max_size: 100 },
                "fuchsia-boot:///platform-bus#meta/platform-bus.cm".into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.driver.manager.EnableTestShutdownDelays",
            Config::new(
                ConfigValueType::Bool,
                test_fuzzing_config.enable_test_shutdown_delays.into(),
            ),
        )?;

        Ok(())
    }
}
