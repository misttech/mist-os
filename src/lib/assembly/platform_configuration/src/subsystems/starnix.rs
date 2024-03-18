// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;
use assembly_config_schema::platform_config::starnix_config::PlatformStarnixConfig;

pub(crate) struct StarnixSubsystem;
impl DefineSubsystemConfiguration<PlatformStarnixConfig> for StarnixSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        starnix_config: &PlatformStarnixConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if starnix_config.enabled {
            ensure!(
                *context.feature_set_level == FeatureSupportLevel::Standard,
                "Starnix is only supported in the default feature set level"
            );
            builder.platform_bundle("starnix_support");

            let has_fullmac = context.board_info.provides_feature("fuchsia::wlan_fullmac");
            let has_softmac = context.board_info.provides_feature("fuchsia::wlan_softmac");
            if starnix_config.enable_android_support {
                if has_fullmac || has_softmac {
                    builder.platform_bundle("wlan_wlanix");
                }

                builder
                    .package("starnix")
                    .component("meta/starnix_runner.cm")?
                    .field("enable_data_collection", *context.build_type == BuildType::UserDebug)?;
            } else {
                builder
                    .package("starnix")
                    .component("meta/starnix_runner.cm")?
                    .field("enable_data_collection", false)?;
            }

            // TODO(https://fxbug.dev/330159122): Re-enable this in the starnix configuration once
            // a more precise mechanism for including aibs exists.
            // if *context.build_type == BuildType::Eng {
            //    builder.platform_bundle("adb_support");
            // }
        }
        Ok(())
    }
}
