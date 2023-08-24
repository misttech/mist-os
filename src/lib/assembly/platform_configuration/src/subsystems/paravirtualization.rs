// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::bail;
use assembly_config_schema::platform_config::virtualization_config::{
    FeatureControl, PlatformParavirtualizationConfig,
};

pub(crate) struct VirtualizationSubsystem;

impl DefineSubsystemConfiguration<PlatformParavirtualizationConfig> for VirtualizationSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        virtualization_config: &PlatformParavirtualizationConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let enabled = virtualization_config.enabled;
        let supported = context.board_info.provides_feature("fuchsia::paravirtualization");
        match (enabled, supported) {
            (FeatureControl::Enabled, false) => bail!("Product requires paravirtualization, but board doesn't provide feature: fuchsia::paravirtualization"),
            (FeatureControl::Disabled, true) => _,
            (FeatureControl::Allowed, false) => _,
            (FeatureContorl::Allowed, true) | (FeatureControl::Required, true) => {
                builder.platform_bundle("paravirtualization_support");
            }
        }
        Ok(())
    }
}
