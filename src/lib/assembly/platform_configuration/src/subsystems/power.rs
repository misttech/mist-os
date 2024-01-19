// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{ensure, Context};
use assembly_config_schema::platform_config::power_config::PowerConfig;
use assembly_util::{BootfsDestination, FileEntry};

use crate::subsystems::prelude::*;

pub(crate) struct PowerManagementSubsystem;

impl DefineSubsystemConfiguration<PowerConfig> for PowerManagementSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &PowerConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
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
                    FeatureSupportLevel::Minimal | FeatureSupportLevel::Utility
                ) && *context.build_type == BuildType::Eng
            );
            builder.platform_bundle("power_framework");
        }

        Ok(())
    }
}
