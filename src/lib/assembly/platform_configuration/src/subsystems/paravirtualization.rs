// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::bail;
use assembly_config_schema::platform_config::paravirtualization_config::PlatformParavirtualizationConfig;
use assembly_config_schema::FeatureControl;

pub(crate) struct ParavirtualizationSubsystem;

impl DefineSubsystemConfiguration<PlatformParavirtualizationConfig>
    for ParavirtualizationSubsystem
{
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        virtualization_config: &PlatformParavirtualizationConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let enabled = &virtualization_config.enabled;
        let supported = context.board_info.provides_feature("fuchsia::paravirtualization");
        match (enabled, supported) {
            (FeatureControl::Required, false) => bail!("Product requires paravirtualization, but board doesn't provide feature: fuchsia::paravirtualization"),
            (FeatureControl::Disabled, true) => (),
            (FeatureControl::Disabled, false) => (),
            (FeatureControl::Allowed, false) => (),
            (FeatureControl::Allowed, true) | (FeatureControl::Required, true) => {
                match context.feature_set_level {
                    FeatureSetLevel::Embeddable | FeatureSetLevel::Bootstrap =>
                builder.platform_bundle("paravirtualization_support_bootstrap"),
                    FeatureSetLevel::Utility | FeatureSetLevel::Standard =>
                        builder.platform_bundle("paravirtualization_support"),
                };
            }
        }
        Ok(())
    }
}
