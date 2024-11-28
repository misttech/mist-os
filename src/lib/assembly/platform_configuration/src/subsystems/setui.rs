// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};
use assembly_config_schema::platform_config::setui_config::{ICUType, SetUiConfig};
use assembly_constants::FileEntry;

pub(crate) struct SetUiSubsystem;
impl DefineSubsystemConfiguration<SetUiConfig> for SetUiSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &SetUiConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
        // setui is always added to Standard feature set level system.
        if *context.feature_set_level == FeatureSupportLevel::Standard {
            let bundle_name = if config.with_camera { "setui_with_camera" } else { "setui" };
            match config.use_icu {
                ICUType::Flavored => {
                    builder
                        .icu_platform_bundle(bundle_name)
                        .context("while configuring the 'Intl' subsystem with ICU")?;
                }
                ICUType::Unflavored => {
                    builder.platform_bundle(bundle_name);
                }
            };

            if let Some(display) = &config.display {
                builder.package("setui_service").config_data(FileEntry {
                    source: display.clone(),
                    destination: "display_configuration.json".into(),
                })?;
            }

            if let Some(interface) = &config.interface {
                builder.package("setui_service").config_data(FileEntry {
                    source: interface.clone(),
                    destination: "interface_configuration.json".into(),
                })?;
            }

            if let Some(agent) = &config.agent {
                builder.package("setui_service").config_data(FileEntry {
                    source: agent.clone(),
                    destination: "agent_configuration.json".into(),
                })?;
            }
        }
        Ok(())
    }
}
