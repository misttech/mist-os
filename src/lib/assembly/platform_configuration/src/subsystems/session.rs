// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;
use assembly_config_schema::platform_config::session_manager_config::PlatformSessionManagerConfig;

pub(crate) struct SessionConfig;
impl DefineSubsystemConfiguration<(&PlatformSessionManagerConfig, &String)> for SessionConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        session_config: &(&PlatformSessionManagerConfig, &String),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let session_manager_config = session_config.0;
        let session_url = session_config.1;

        if session_manager_config.enabled {
            ensure!(
                *context.feature_set_level == FeatureSupportLevel::Minimal,
                "The platform session manager is only supported in the default feature set level"
            );
            builder.platform_bundle("session_manager");
        }

        if *context.feature_set_level == FeatureSupportLevel::Minimal {
            // Configure the session URL.
            ensure!(
                session_url.is_empty() || session_url.starts_with("fuchsia-pkg://"),
                "valid session URLs must start with `fuchsia-pkg://`, got `{}`",
                session_url
            );
            builder
                .package("session_manager")
                .component("meta/session_manager.cm")?
                .field("session_url", session_url.to_owned())?;
        } else {
            ensure!(
                session_url.is_empty(),
                "sessions are only supported with the 'Minimal' feature set level"
            );
        }

        Ok(())
    }
}
