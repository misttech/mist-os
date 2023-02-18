// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::driver_framework_config::DriverFrameworkConfig;

pub(crate) struct DriverFrameworkSubsystemConfig;
impl DefineSubsystemConfiguration<DriverFrameworkConfig> for DriverFrameworkSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        driver_framework_config: &DriverFrameworkConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let mut disabled_drivers = driver_framework_config.disabled_drivers.clone();
        if context.board_info.provides_feature("driver_framework_v2_support") {
            // TODO(http://fxbug.dev/102096): Remove this once DFv2 is enabled by default and there
            // exists only one da7219 driver.
            disabled_drivers.push("fuchsia-boot:///#meta/da7219.cm".to_string());
            builder.platform_bundle("driver_framework_v2");
        }

        let delay_fallback = !matches!(context.feature_set_level, FeatureSupportLevel::Bringup);

        builder
            .package("driver-index")
            .component("meta/driver-index.cm")?
            .field("enable_ephemeral_drivers", matches!(context.build_type, BuildType::Eng))?
            .field("delay_fallback_until_base_drivers_indexed", delay_fallback)?
            .field("bind_eager", driver_framework_config.eager_drivers.clone())?
            .field("disabled_drivers", disabled_drivers)?;

        Ok(())
    }
}
