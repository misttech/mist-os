// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use crate::util;
use assembly_config_schema::platform_config::forensics_config::ForensicsConfig;
use assembly_util::FileEntry;

pub(crate) struct ForensicsSubsystem;
impl DefineSubsystemConfiguration<ForensicsConfig> for ForensicsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &ForensicsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if config.feedback.low_memory {
            builder.platform_bundle("feedback_low_memory_product_config");
        }
        if config.feedback.large_disk {
            builder.platform_bundle("feedback_large_disk");
        }
        if config.feedback.remote_device_id_provider {
            builder.platform_bundle("feedback_remote_device_id_provider");
        }

        match context.build_type {
            // The userdebug/user configs are platform bundles that add an override config.
            BuildType::User => builder.platform_bundle("feedback_user_config"),
            BuildType::UserDebug => builder.platform_bundle("feedback_userdebug_config"),
            // The eng config is actually an absent override config.
            BuildType::Eng => {}
        }

        // Cobalt may be added to anything utility and higher.
        if matches!(
            context.feature_set_level,
            FeatureSupportLevel::Minimal | FeatureSupportLevel::Utility
        ) {
            util::add_build_type_config_data("cobalt", context, builder)?;
            if let Some(api_key) = &config.cobalt.api_key {
                builder.package("cobalt").config_data(FileEntry {
                    source: api_key.clone(),
                    destination: "api_key.hex".into(),
                })?;
            }
        }

        if let Some(url) = &config.feedback.flash_ts_feedback_id_component_url {
            util::add_platform_declared_product_provided_component(
                &url,
                "flash_ts_feedback_id.core_shard.cml.template",
                context,
                builder,
            )?;
        }

        Ok(())
    }
}
