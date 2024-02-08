// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{anyhow, Context, Result};
use assembly_config_schema::platform_config::intl_config::{IntlConfig, Type};

pub(crate) struct IntlSubsystem;
impl DefineSubsystemConfiguration<IntlConfig> for IntlSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &IntlConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
        // These settings require a core realm.
        if *context.feature_set_level != FeatureSupportLevel::Standard {
            if config.config_type != Type::None {
                return Err(anyhow!(concat!(
                    "setting config_type for `intl` is only supported",
                    " on `standard` feature support level"
                )));
            }
            if config.include_zoneinfo_files {
                return Err(anyhow!(concat!(
                    "setting include_zoneinfo_files for `intl` only supported",
                    " only on `standard` feature support level"
                )));
            }
        }

        // If, however we are able to configure...
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
            Type::SmallWithTimezone => {
                builder
                    .icu_platform_bundle("intl_services_small_with_timezone")
                    .context("while configuring the 'small Intl with timezone' subsystem")?;
            }
            Type::None => { /* Skip the bundle altogether. */ }
        }

        if config.include_zoneinfo_files {
            builder.icu_platform_bundle("zoneinfo").context("while configuring `zoneinfo`")?;
        }

        Ok(())
    }
}
