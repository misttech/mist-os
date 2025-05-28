// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;
use assembly_config_capabilities::{Config, ConfigNestedValueType, ConfigValueType};
use assembly_config_schema::platform_config::starnix_config::{
    PlatformStarnixConfig, SocketMarkTreatment,
};
use starnix_features::{Feature, FeatureAndArgs};

pub(crate) struct StarnixSubsystem;
impl DefineSubsystemConfiguration<PlatformStarnixConfig> for StarnixSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        starnix_config: &PlatformStarnixConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let PlatformStarnixConfig { enabled, enable_android_support, socket_mark } = starnix_config;

        if *enabled {
            ensure!(
                *context.feature_set_level == FeatureSetLevel::Standard,
                "Starnix is only supported in the default feature set level"
            );
            ensure!(
                *context.build_type != BuildType::User,
                "Starnix is not supported on user builds."
            );
            builder.platform_bundle("starnix_support");

            let has_fullmac = context.board_info.provides_feature("fuchsia::wlan_fullmac");
            let has_softmac = context.board_info.provides_feature("fuchsia::wlan_softmac");
            if *enable_android_support {
                if has_fullmac || has_softmac {
                    builder.platform_bundle("wlan_wlanix");
                }

                builder.set_config_capability(
                    "fuchsia.starnix.runner.EnableDataCollection",
                    Config::new(
                        ConfigValueType::Bool,
                        (*context.build_type == BuildType::UserDebug).into(),
                    ),
                )?;
                builder.platform_bundle("adb_support");
                builder.platform_bundle("hvdcp_opti_support");
                builder.platform_bundle("nanohub_support");
            } else {
                builder.set_config_capability(
                    "fuchsia.starnix.runner.EnableDataCollection",
                    Config::new(ConfigValueType::Bool, false.into()),
                )?;
            }
            builder.set_config_capability(
                "fuchsia.starnix.config.container.ExtraFeatures",
                Config::new(
                    ConfigValueType::Vector {
                        nested_type: ConfigNestedValueType::String { max_size: 1024 },
                        max_count: 1024,
                    },
                    [match socket_mark {
                        SocketMarkTreatment::StarnixOnly => None,
                        SocketMarkTreatment::SharedWithNetstack => Some(
                            FeatureAndArgs { feature: Feature::NetstackMark, raw_args: None }
                                .to_string(),
                        ),
                    }]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<String>>()
                    .into(),
                ),
            )?;
        }
        Ok(())
    }
}
