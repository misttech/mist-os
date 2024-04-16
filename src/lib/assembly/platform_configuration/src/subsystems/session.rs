// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;
use assembly_config_capabilities::{Config, ConfigValueType};
use assembly_config_schema::platform_config::session_config::PlatformSessionConfig;

pub(crate) struct SessionConfig;
impl DefineSubsystemConfiguration<(&PlatformSessionConfig, &String, &bool)> for SessionConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &(&PlatformSessionConfig, &String, &bool),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let session_config = config.0;
        let session_url = config.1;

        // TODO(https://fxbug.dev/326086827): remove this hack, it's just for a
        // soft transition
        let input_group_2 = config.2;

        if session_config.enabled || *input_group_2 {
            ensure!(
                *context.feature_set_level == FeatureSupportLevel::Standard,
                "The platform session manager is only supported in the default feature set level"
            );
            builder.platform_bundle("session_manager");
        }

        if *context.feature_set_level == FeatureSupportLevel::Standard {
            // Configure the session URL.
            ensure!(
                session_url.is_empty() || session_url.starts_with("fuchsia-pkg://"),
                "valid session URLs must start with `fuchsia-pkg://`, got `{}`",
                session_url
            );
        } else {
            ensure!(
                session_url.is_empty(),
                "sessions are only supported with the 'Standard' feature set level"
            );
        }
        builder.set_config_capability(
            "fuchsia.session.SessionUrl",
            Config::new(ConfigValueType::String { max_size: 512 }, session_url.to_owned().into()),
        )?;
        builder.set_config_capability(
            "fuchsia.session.AutoLaunch",
            Config::new(ConfigValueType::Bool, session_config.autolaunch.into()),
        )?;

        if session_config.include_element_manager {
            ensure!(
                *context.feature_set_level == FeatureSupportLevel::Standard,
                "The platform element manager is only supported in the default feature set level"
            );
            builder.platform_bundle("element_manager");
        }

        Ok(())
    }
}
