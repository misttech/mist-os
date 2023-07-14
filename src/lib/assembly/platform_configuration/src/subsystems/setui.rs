// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};
use assembly_config_schema::platform_config::setui_config::{ICUType, SetUiConfig};

pub(crate) struct SetUiSubsystem;
impl DefineSubsystemConfiguration<SetUiConfig> for SetUiSubsystem {
    fn define_configuration(
        _: &ConfigurationContext<'_>,
        config: &SetUiConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
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
            ICUType::None => { /* Skip the bundle altogether. */ }
        };
        Ok(())
    }
}
