// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::connectivity_config::PlatformConnectivityConfig;

pub(crate) struct ConnectivitySubsystemConfig;
impl DefineSubsystemConfiguration<PlatformConnectivityConfig> for ConnectivitySubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        connectivity_config: &PlatformConnectivityConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if let FeatureSupportLevel::Minimal = context.feature_set_level {
            if let Some(board_info) = context.board_info {
                let has_fullmac = board_info.provides_feature("fuchsia::wlan_fullmac");
                let has_softmac = board_info.provides_feature("fuchsia::wlan_softmac");
                if has_fullmac || has_softmac {
                    builder.platform_bundle("wlan_base");
                    // Some products require legacy security types to be supported.
                    // Otherwise, they are disabled by default.
                    if connectivity_config.wlan.legacy_privacy_support {
                        builder.platform_bundle("wlan_legacy_privacy_support");
                    } else {
                        builder.platform_bundle("wlan_contemporary_privacy_only_support");
                    }

                    if has_fullmac {
                        builder.platform_bundle("wlan_fullmac_support");
                    }
                    if has_softmac {
                        builder.platform_bundle("wlan_softmac_support");
                    }
                }
            }

            builder.platform_bundle("netstack");
        }

        Ok(())
    }
}
