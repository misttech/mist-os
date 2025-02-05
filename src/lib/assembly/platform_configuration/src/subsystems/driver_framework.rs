// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_capabilities::{Config, ConfigNestedValueType, ConfigValueType};
use assembly_config_schema::platform_config::driver_framework_config::{
    DriverFrameworkConfig, TestFuzzingConfig,
};
use assembly_config_schema::platform_config::storage_config::StorageConfig;
use assembly_images_config::FilesystemImageMode;

pub(crate) struct DriverFrameworkSubsystemConfig;
impl DefineSubsystemConfiguration<(&DriverFrameworkConfig, &StorageConfig)>
    for DriverFrameworkSubsystemConfig
{
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &(&DriverFrameworkConfig, &StorageConfig),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // This is always set to false, but should be configurable once drivers actually support
        // using a hardware iommu.
        builder.set_config_capability(
            "fuchsia.driver.UseHardwareIommu",
            Config::new(ConfigValueType::Bool, false.into()),
        )?;
        let (driver_framework, storage) = config;

        let mut disabled_drivers = driver_framework.disabled_drivers.clone();
        // TODO(https://fxbug.dev/42052994): Remove this once DFv2 is enabled by default and there
        // exists only one da7219 driver.
        disabled_drivers.push("fuchsia-boot:///#meta/da7219.cm".to_string());
        builder.platform_bundle("driver_framework");

        let enable_ephemeral_drivers = match (context.build_type, context.feature_set_level) {
            (BuildType::Eng, FeatureSupportLevel::Standard) => {
                builder.platform_bundle("full_drivers");
                true
            }
            (_, _) => false,
        };

        let delay_fallback = matches!(
            context.feature_set_level,
            FeatureSupportLevel::Utility | FeatureSupportLevel::Standard
        );

        let test_fuzzing_config =
            driver_framework.test_fuzzing_config.as_ref().unwrap_or(&TestFuzzingConfig {
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
                driver_framework.eager_drivers.clone().into(),
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

        match driver_framework.enable_driver_index_stop_on_idle {
            true => {
                // Explicitly enabled by platform config.
                // Set to 10 seconds.
                builder.set_config_capability(
                    "fuchsia.driver.index.StopOnIdleTimeoutMillis",
                    Config::new(ConfigValueType::Int64, 10000.into()),
                )?;
            }
            false => {
                // Explicitly disabled by platform config.
                builder.set_config_capability(
                    "fuchsia.driver.index.StopOnIdleTimeoutMillis",
                    Config::new_void(),
                )?;
            }
        };

        let mut software_names = Vec::new();
        let mut software_ids = Vec::new();
        if storage.filesystems.image_mode == FilesystemImageMode::Ramdisk {
            software_names.push("ram-disk");
            software_ids.push(bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_RAM_DISK);
        }
        if *context.feature_set_level == FeatureSupportLevel::Standard
            && *context.build_type == BuildType::Eng
        {
            software_names.push("virtual-audio");
            software_ids.push(bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_VIRTUAL_AUDIO);
        }

        if context.board_info.provides_feature("fuchsia::fake_battery")
            && matches!(
                context.feature_set_level,
                FeatureSupportLevel::Utility | FeatureSupportLevel::Standard
            )
        {
            software_names.push("fake-battery");
            software_ids.push(bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_FAKE_BATTERY);
        }

        builder.set_config_capability(
            "fuchsia.platform.bus.SoftwareDeviceIds",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::Uint32,
                    max_count: 20,
                },
                software_ids.into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.platform.bus.SoftwareDeviceNames",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 100 },
                    max_count: 20,
                },
                software_names.into(),
            ),
        )?;

        // Include bus-pci driver through a platform AIB.
        if context.board_info.provides_feature("fuchsia::bus_pci") {
            builder.platform_bundle("bus_pci_driver");
            // In engineering builds, include the lspci tool whenever the pci
            // bus feature is enabled.
            if context.build_type == &BuildType::Eng {
                builder.platform_bundle("lspci");
            }
        }

        Ok(())
    }
}
