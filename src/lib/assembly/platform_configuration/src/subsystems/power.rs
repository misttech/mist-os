// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{ensure, Context};
use assembly_config_capabilities::{Config, ConfigValueType};
use assembly_config_schema::platform_config::power_config::PowerConfig;
use assembly_util::{BootfsDestination, FileEntry};

pub(crate) struct PowerManagementSubsystem;

impl DefineSubsystemConfiguration<PowerConfig> for PowerManagementSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &PowerConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if let Some(cpu_manager_config) = &context.board_info.configuration.cpu_manager {
            builder
                .bootfs()
                .file(FileEntry {
                    source: cpu_manager_config.as_utf8_pathbuf().into(),
                    destination: BootfsDestination::CpuManagerNodeConfig,
                })
                .context("Adding cpu_manager config file")?;
        }

        if let Some(power_manager_config) = &context.board_info.configuration.power_manager {
            builder
                .bootfs()
                .file(FileEntry {
                    source: power_manager_config.as_utf8_pathbuf().into(),
                    destination: BootfsDestination::PowerManagerNodeConfig,
                })
                .context("Adding power_manager config file")?;
        }

        if let Some(thermal_config) = &context.board_info.configuration.thermal {
            builder
                .bootfs()
                .file(FileEntry {
                    source: thermal_config.as_utf8_pathbuf().into(),
                    destination: BootfsDestination::PowerManagerThermalConfig,
                })
                .context("Adding power_manager's thermal config file")?;
        }

        if config.suspend_enabled {
            ensure!(
                matches!(
                    context.feature_set_level,
                    FeatureSupportLevel::Standard | FeatureSupportLevel::Utility
                ) && *context.build_type == BuildType::Eng
            );
            builder.platform_bundle("power_framework");
        }
        builder.set_config_capability(
            "fuchsia.power.SuspendEnabled",
            Config::new(ConfigValueType::Bool, config.suspend_enabled.into()),
        )?;

        match (&context.board_info.configuration.power_metrics_recorder, &context.feature_set_level)
        {
            (Some(config), FeatureSupportLevel::Standard) => {
                builder.platform_bundle("power_metrics_recorder");
                builder.package("metrics-logger-standalone").config_data(FileEntry {
                    source: config.as_utf8_pathbuf().into(),
                    destination: "config.json".to_string(),
                })?;
            }
            _ => {} // do nothing
        }

        Ok(())
    }
}
