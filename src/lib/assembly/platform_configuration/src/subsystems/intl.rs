// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};
use assembly_config_schema::platform_config::intl_config::{IntlConfig, Type};

pub(crate) struct IntlSubsystem;
impl DefineSubsystemConfiguration<IntlConfig> for IntlSubsystem {
    fn define_configuration(
        _: &ConfigurationContext<'_>,
        config: &IntlConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
        match config.config_type {
            Type::Default => {
                builder
                    .icu_platform_bundle("intl_services")
                    .context("while configuring the 'Intl' subsystem")?;
            }
            Type::Small => {
                builder
                    .icu_platform_bundle("intl_services_small")
                    .context("while configuring the 'small Intl' subsystem")?;
            }
            Type::None => { /* Skip the bundle altogether. */ }
        };

        Ok(())
    }
}
