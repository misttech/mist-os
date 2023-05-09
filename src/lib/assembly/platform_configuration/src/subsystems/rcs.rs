// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    BoardInformationExt, BuildType, ConfigurationBuilder, ConfigurationContext,
    DefineSubsystemConfiguration, FeatureSupportLevel,
};

pub struct RcsSubsystemConfig;
impl DefineSubsystemConfiguration<()> for RcsSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _config: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if matches!(
            (context.feature_set_level, context.build_type),
            (
                FeatureSupportLevel::Utility | FeatureSupportLevel::Minimal,
                BuildType::UserDebug | BuildType::Eng
            )
        ) {
            if context.board_info.provides_feature("fuchsia::usb_peripheral_support") {
                builder.platform_bundle("core_realm_development_access_rcs_usb");
            } else {
                builder.platform_bundle("core_realm_development_access_rcs_no_usb");
            }
        }

        Ok(())
    }
}
