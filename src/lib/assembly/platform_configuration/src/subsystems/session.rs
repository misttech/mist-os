// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{ensure, Context};
use assembly_config_capabilities::{Config, ConfigValueType};
use assembly_config_schema::platform_config::session_config::PlatformSessionConfig;
use assembly_config_schema::product_config::ProductSessionConfig;
use fuchsia_url::AbsoluteComponentUrl;

pub(crate) struct SessionConfig;
impl DefineSubsystemConfiguration<(&PlatformSessionConfig, &Option<ProductSessionConfig>, &String)>
    for SessionConfig
{
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &(&PlatformSessionConfig, &Option<ProductSessionConfig>, &String),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let (platform_config, product_config, session_url) = *config;

        if platform_config.enabled {
            ensure!(
                *context.feature_set_level == FeatureSupportLevel::Standard,
                "The platform session manager is only supported in the default feature set level"
            );
            builder.platform_bundle("session_manager");
        }

        let (url, collection, element_url, view_id_annotation) = match (
            context.feature_set_level,
            product_config,
            session_url.is_empty(),
        ) {
            (FeatureSupportLevel::Standard, Some(config), true) => {
                let config_url = AbsoluteComponentUrl::parse(&config.url)
                    .with_context(|| format!("valid session URLs given by session.url must start with `fuchsia-pkg://`, got `{}`", &config.url))?
                    .to_string();
                if let Some(element) = &config.initial_element {
                    (
                        config_url,
                        Config::new(
                            ConfigValueType::String { max_size: 32 },
                            element.collection.to_owned().into(),
                        ),
                        Config::new(
                            ConfigValueType::String { max_size: 256 },
                            AbsoluteComponentUrl::parse(&element.url)
                                .with_context(|| {
                                    format!("valid initial element URLs given by session.initial_element.url must start with `fuchsia-pkg://`, got `{}`", &element.url)
                                })?
                                .to_string()
                                .into(),
                        ),
                        Config::new(
                            ConfigValueType::String { max_size: 32 },
                            element.view_id_annotation.to_owned().into(),
                        ),
                    )
                } else {
                    (config_url, Config::new_void(), Config::new_void(), Config::new_void())
                }
            }
            (FeatureSupportLevel::Standard, None, false) => (
                AbsoluteComponentUrl::parse(&session_url)
                    .with_context(|| format!("valid session URLs given by session_url must start with `fuchsia-pkg://`, got `{}`", &session_url))?
                    .to_string(),
                Config::new_void(),
                Config::new_void(),
                Config::new_void(),
            ),
            _ => {
                ensure!(
                        product_config.is_none() && session_url.is_empty(),
                        "sessions are only supported with the 'Standard' feature set level, and only one of 'session' and 'session_url' should be used at a time."
                    );
                (String::new(), Config::new_void(), Config::new_void(), Config::new_void())
            }
        };

        builder.set_config_capability(
            "fuchsia.session.SessionUrl",
            Config::new(ConfigValueType::String { max_size: 512 }, url.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.session.AutoLaunch",
            Config::new(ConfigValueType::Bool, platform_config.autolaunch.into()),
        )?;

        if platform_config.include_element_manager {
            ensure!(
                *context.feature_set_level == FeatureSupportLevel::Standard,
                "The platform element manager is only supported in the default feature set level"
            );
            builder.platform_bundle("element_manager");
        }

        // Window manager configs
        builder.set_config_capability("fuchsia.session.window.Collection", collection)?;
        builder.set_config_capability("fuchsia.session.window.InitialElementUrl", element_url)?;
        builder.set_config_capability(
            "fuchsia.session.window.InitialViewIdAnnotation",
            view_id_annotation,
        )?;

        Ok(())
    }
}
