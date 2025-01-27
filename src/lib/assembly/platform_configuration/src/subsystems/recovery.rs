// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;

use crate::subsystems::prelude::*;
use anyhow::Context;
use assembly_config_capabilities::{Config, ConfigValueType};
use assembly_config_schema::platform_config::recovery_config::{RecoveryConfig, SystemRecovery};
use assembly_constants::{FileEntry, PackageDestination, PackageSetDestination};
use assembly_images_config::VolumeConfig;

pub(crate) struct RecoverySubsystem;
impl DefineSubsystemConfiguration<(&RecoveryConfig, &VolumeConfig)> for RecoverySubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        configs: &(&RecoveryConfig, &VolumeConfig),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let (config, volume_config) = *configs;

        if let Some(mapping) = &config.factory_reset_trigger_config {
            // If configuration is provided for the factory-reset-trigger component, include it
            // and the configuration in the build.

            builder.platform_bundle("factory_reset_trigger");

            let config = serde_json::json!({
                "version": "1",
                "content": {
                    "channel_indices": mapping
                }
            });

            let config_file_path = context
                .get_gendir()
                .context("getting gen dir for factory-reset-trigger config file")?
                .join("forced-fdr-channel-indices.config");
            let config_file = File::create(&config_file_path).with_context(|| {
                format!("Creating factory-reset-trigger config file: {}", config_file_path)
            })?;
            serde_json::to_writer_pretty(config_file, &config).with_context(|| {
                format!("Writing factory-reset-trigger config file: {}", config_file_path)
            })?;

            builder
                .package("factory-reset-trigger")
                .config_data(FileEntry {
                    source: config_file_path,
                    destination: "forced-fdr-channel-indices.config".into(),
                })
                .context("Adding factory-reset-trigger config data entry")?;
        } else {
            // Otherwise, only include the factory-reset-trigger (without configuration, assuming
            // it will be provided via the legacy bundle) if explicitly requested.
            if config.factory_reset_trigger {
                builder.platform_bundle("factory_reset_trigger");
            }
        }

        if *context.feature_set_level == FeatureSupportLevel::Standard
            || config.system_recovery.is_some()
        {
            // factory_reset is required by the standard feature set level, and when system_recovery
            // is enabled.
            builder.platform_bundle("factory_reset");
        }

        // factory_reset needs to know which mutable filesystem to use, in order to properly
        // reset it.  The value is always provided in case factory_reset has been added directly
        // by a product, and not through assembly.
        builder.set_config_capability(
            "fuchsia.recovery.UseFxBlob",
            Config::new(
                ConfigValueType::Bool,
                match volume_config {
                    VolumeConfig::Fxfs => true,
                    VolumeConfig::Fvm(_) => false,
                }
                .into(),
            ),
        )?;

        if let Some(system_recovery) = &config.system_recovery {
            context.ensure_feature_set_level(&[FeatureSupportLevel::Utility], "System Recovery")?;
            match system_recovery {
                SystemRecovery::Fdr => builder.platform_bundle("recovery_fdr"),
            }

            // Create the recovery domain configuration package
            let directory = builder
                .add_domain_config(PackageSetDestination::Blob(
                    PackageDestination::SystemRecoveryConfig,
                ))
                .directory("system-recovery-config");

            let logo_source = if let Some(logo) = &config.logo {
                logo.clone().to_utf8_pathbuf()
            } else {
                context.get_resource("fuchsia-logo.riv")
            };
            directory
                .entry(FileEntry { source: logo_source, destination: "logo.riv".to_owned() })
                .context("Adding logo to system-recovery-config")?;

            if let Some(instructions_source) = &config.instructions {
                directory
                    .entry(FileEntry {
                        source: instructions_source.clone().to_utf8_pathbuf(),
                        destination: "instructions.txt".to_owned(),
                    })
                    .context("Adding instructions.txt to system-recovery-config")?;
            }

            if config.check_for_managed_mode {
                directory
                    .entry_from_contents("check_fdr_restriction.json", "{}")
                    .context("Adding check_fdr_restriction.json to system-recovery_config")?;
            }
        }

        // system-recovery-fdr needs to know the board's display rotation so that it can
        // appropriately display the logo.
        //
        // This needs to always be set, in case recovery is being added by products directly,
        // and not via assembly.
        if let Some(display_rotation) = &context.board_info.platform.graphics.display.rotation {
            builder.set_config_capability(
                "fuchsia.recovery.DisplayRotation",
                Config::new(
                    ConfigValueType::Uint16,
                    u16::try_from(*display_rotation)
                        .context("converting 'display_rotation' to 16-bits")?
                        .into(),
                ),
            )?;
        } else {
            builder
                .set_config_capability("fuchsia.recovery.DisplayRotation", Config::new_void())?;
        }
        Ok(())
    }
}
