// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::driver_framework_config::{
    DriverFrameworkConfig, DriverHostCrashPolicy,
};

pub(crate) struct DriverFrameworkSubsystemConfig;
impl DefineSubsystemConfiguration<DriverFrameworkConfig> for DriverFrameworkSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        driver_framework_config: &DriverFrameworkConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let mut disabled_drivers = driver_framework_config.disabled_drivers.clone();
        let use_dfv2 = context.board_info.provides_feature("fuchsia::driver_framework_v2_support");
        if use_dfv2 {
            // TODO(http://fxbug.dev/102096): Remove this once DFv2 is enabled by default and there
            // exists only one da7219 driver.
            disabled_drivers.push("fuchsia-boot:///#meta/da7219.cm".to_string());
            builder.platform_bundle("driver_framework_v2");
        } else {
            builder.platform_bundle("driver_framework_v1");
        }

        let delay_fallback = !matches!(context.feature_set_level, FeatureSupportLevel::Bootstrap);

        builder
            .package("driver-index")
            .component("meta/driver-index.cm")?
            .field("enable_ephemeral_drivers", matches!(context.build_type, BuildType::Eng))?
            .field("delay_fallback_until_base_drivers_indexed", delay_fallback)?
            .field("bind_eager", driver_framework_config.eager_drivers.clone())?
            .field("disabled_drivers", disabled_drivers)?;

        let driver_host_crash_policy = driver_framework_config
            .driver_host_crash_policy
            .as_ref()
            .unwrap_or(&DriverHostCrashPolicy::RestartDriverHost);

        builder
            .package("driver_manager")
            .component("meta/driver_manager.cm")?
            .field("set_root_driver_host_critical", true)?
            .field("delay_fallback_until_base_drivers_indexed", delay_fallback)?
            .field("suspend_timeout_fallback", true)?
            .field("verbose", false)?
            .field("use_driver_framework_v2", use_dfv2)?
            .field("driver_host_crash_policy", format!("{driver_host_crash_policy}"))?
            .field("root_driver", "fuchsia-boot:///platform-bus#meta/platform-bus.cm")?;

        Ok(())
    }
}
