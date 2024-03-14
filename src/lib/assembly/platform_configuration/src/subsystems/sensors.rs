// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_config_schema::platform_config::starnix_config::PlatformStarnixConfig;

use crate::subsystems::prelude::*;

pub(crate) struct SensorsSubsystemConfig;
impl DefineSubsystemConfiguration<PlatformStarnixConfig> for SensorsSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        starnix_config: &PlatformStarnixConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if starnix_config.enabled && *context.feature_set_level == FeatureSupportLevel::Standard {
            builder.platform_bundle("sensors_framework");
        }

        Ok(())
    }
}
