// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;

pub(crate) struct ThermalSubsystem;
impl DefineSubsystemConfiguration<()> for ThermalSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if (*context.feature_set_level == FeatureSupportLevel::Utility
            || *context.feature_set_level == FeatureSupportLevel::Minimal)
            && context.board_info.provides_feature("fuchsia::fan")
        {
            builder.platform_bundle("fan");
        }

        Ok(())
    }
}
