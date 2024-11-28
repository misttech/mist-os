// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use crate::util;
use assembly_config_schema::developer_overrides::{
    DeveloperOnlyOptions, FeedbackBuildTypeConfig, ForensicsOptions,
};
use assembly_config_schema::platform_config::forensics_config::ForensicsConfig;
use assembly_constants::FileEntry;

pub(crate) struct ForensicsSubsystem;
impl DefineSubsystemConfiguration<ForensicsConfig> for ForensicsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &ForensicsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if config.feedback.large_disk {
            builder.platform_bundle("feedback_large_disk");
        }
        if config.feedback.remote_device_id_provider {
            builder.platform_bundle("feedback_remote_device_id_provider");
        }
        if config.feedback.include_kernel_logs_in_last_reboot_info {
            builder.platform_bundle("kernel_logs_in_reboot_info");
        }

        if *context.build_type != BuildType::Eng {
            if let Some(DeveloperOnlyOptions {
                forensics_options: Some(ForensicsOptions { build_type_override: Some(_), .. }),
                ..
            }) = context.developer_only_options
            {
                anyhow::bail!("Feedback build type overrides only supported in eng build-types");
            }
        }

        match context.build_type {
            // The userdebug/user configs are platform bundles that add an override config.
            BuildType::User => builder.platform_bundle("feedback_user_config"),
            BuildType::UserDebug => builder.platform_bundle("feedback_userdebug_config"),
            // The eng config is actually an absent override config. Apply override if specified.
            BuildType::Eng => {
                if let Some(DeveloperOnlyOptions {
                    forensics_options: Some(forensics_options),
                    ..
                }) = context.developer_only_options
                {
                    match forensics_options.build_type_override {
                        Some(FeedbackBuildTypeConfig::EngWithUpload) => {
                            builder.platform_bundle("feedback_upload_config")
                        }
                        Some(FeedbackBuildTypeConfig::UserDebug) => {
                            builder.platform_bundle("feedback_userdebug_config")
                        }
                        Some(FeedbackBuildTypeConfig::User) => {
                            builder.platform_bundle("feedback_user_config")
                        }
                        None => (),
                    }
                }
            }
        }

        // Cobalt may be added to anything utility and higher.
        if matches!(
            context.feature_set_level,
            FeatureSupportLevel::Standard | FeatureSupportLevel::Utility
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
                url,
                "flash_ts_feedback_id.core_shard.cml.template",
                context,
                builder,
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use assembly_config_schema::developer_overrides::{DeveloperOnlyOptions, ForensicsOptions};

    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;

    #[test]
    fn test_build_type_override() {
        let developer_only_options = DeveloperOnlyOptions {
            forensics_options: Some(ForensicsOptions {
                build_type_override: Some(FeedbackBuildTypeConfig::UserDebug),
            }),
            ..Default::default()
        };

        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::Eng,
            board_info: &Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
            developer_only_options: Some(&developer_only_options),
        };

        let forensics_config: ForensicsConfig = Default::default();
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            ForensicsSubsystem::define_configuration(&context, &forensics_config, &mut builder);
        assert!(result.is_ok());
        assert!(builder.build().bundles.contains("feedback_userdebug_config"));
    }

    #[test]
    fn test_build_type_override_bails_on_user_builds() {
        // Build type overrides are only allowed for eng builds.
        let developer_only_options = DeveloperOnlyOptions {
            forensics_options: Some(ForensicsOptions {
                build_type_override: Some(FeedbackBuildTypeConfig::UserDebug),
            }),
            ..Default::default()
        };

        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::User,
            board_info: &Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
            developer_only_options: Some(&developer_only_options),
        };

        let forensics_config: ForensicsConfig = Default::default();
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            ForensicsSubsystem::define_configuration(&context, &forensics_config, &mut builder);

        assert!(result.is_err());
    }

    #[test]
    fn test_build_type_override_bails_on_userdebug_builds() {
        // Build type overrides are only allowed for eng builds.
        let developer_only_options = DeveloperOnlyOptions {
            forensics_options: Some(ForensicsOptions {
                build_type_override: Some(FeedbackBuildTypeConfig::EngWithUpload),
            }),
            ..Default::default()
        };

        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::UserDebug,
            board_info: &Default::default(),
            gendir: Default::default(),
            resource_dir: Default::default(),
            developer_only_options: Some(&developer_only_options),
        };

        let forensics_config: ForensicsConfig = Default::default();
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            ForensicsSubsystem::define_configuration(&context, &forensics_config, &mut builder);

        assert!(result.is_err());
    }
}
