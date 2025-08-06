// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use crate::util;
use assembly_config_schema::developer_overrides::{
    DeveloperOnlyOptions, FeedbackBuildTypeConfig, ForensicsOptions,
};
use assembly_config_schema::platform_settings::forensics_config::{
    FeedbackIdComponentUrl, ForensicsConfig,
};
use assembly_constants::{FileEntry, PackageDestination, PackageSetDestination};

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
                forensics_options: ForensicsOptions { build_type_override: Some(_), .. },
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
                if let Some(DeveloperOnlyOptions { forensics_options, .. }) =
                    context.developer_only_options
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

        // Cobalt and Feedback may be added to anything utility and higher.
        if matches!(context.feature_set_level, FeatureSetLevel::Standard | FeatureSetLevel::Utility)
        {
            let config_dir = builder
                .add_domain_config(PackageSetDestination::Blob(PackageDestination::FeedbackConfig))
                .directory("feedback-config");

            config_dir.entry_from_contents(
                "snapshot_exclusion.json",
                &serde_json::to_string_pretty(&config.feedback.snapshot_exclusion)?,
            )?;

            match context.build_type {
                BuildType::User => builder.platform_bundle("cobalt_user_config"),
                BuildType::UserDebug => builder.platform_bundle("cobalt_userdebug_config"),
                BuildType::Eng => builder.platform_bundle("cobalt_default_config"),
            }

            util::add_build_type_config_data("cobalt", context, builder)?;
            if let Some(api_key) = &config.cobalt.api_key {
                builder.package("cobalt").config_data(FileEntry {
                    source: api_key.clone(),
                    destination: "api_key.hex".into(),
                })?;
            }
        }

        match &config.feedback.feedback_id_component_url {
            FeedbackIdComponentUrl::FlashTs(url) => {
                util::add_platform_declared_product_provided_component(
                    url,
                    "flash_ts_feedback_id.core_shard.cml.template",
                    context,
                    builder,
                )?;
            }
            FeedbackIdComponentUrl::SysInfo(url) => {
                util::add_platform_declared_product_provided_component(
                    url,
                    "sysinfo_feedback_id.core_shard.cml.template",
                    context,
                    builder,
                )?;
            }
            FeedbackIdComponentUrl::None => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use assembly_config_schema::developer_overrides::{DeveloperOnlyOptions, ForensicsOptions};
    use assembly_config_schema::platform_settings::forensics_config::FeedbackConfig;
    use camino::Utf8Path;

    use super::*;
    use crate::subsystems::ConfigurationBuilderImpl;

    #[test]
    fn test_build_type_override() {
        let developer_only_options = DeveloperOnlyOptions {
            forensics_options: ForensicsOptions {
                build_type_override: Some(FeedbackBuildTypeConfig::UserDebug),
            },
            ..Default::default()
        };

        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::Eng,
            board_config: &Default::default(),
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
            forensics_options: ForensicsOptions {
                build_type_override: Some(FeedbackBuildTypeConfig::UserDebug),
            },
            ..Default::default()
        };

        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::User,
            board_config: &Default::default(),
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
            forensics_options: ForensicsOptions {
                build_type_override: Some(FeedbackBuildTypeConfig::EngWithUpload),
            },
            ..Default::default()
        };

        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::UserDebug,
            board_config: &Default::default(),
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
    fn flash_ts_feedback_id_core_shard() {
        let resource_dir = tempfile::TempDir::new().unwrap();
        std::fs::File::create(
            resource_dir.path().join("flash_ts_feedback_id.core_shard.cml.template"),
        )
        .unwrap();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::Eng,
            board_config: &Default::default(),
            gendir: Default::default(),
            resource_dir: Utf8Path::from_path(resource_dir.path()).unwrap().to_path_buf(),
            developer_only_options: Default::default(),
        };

        let forensics_config = ForensicsConfig {
            feedback: FeedbackConfig {
                feedback_id_component_url: FeedbackIdComponentUrl::FlashTs(
                    "fuchsia-pkg://fuchsia.com/test-package#meta/test-component.cm".to_string(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            ForensicsSubsystem::define_configuration(&context, &forensics_config, &mut builder);

        assert!(result.is_ok());
        assert!(builder
            .build()
            .core_shards
            .contains(&"flash_ts_feedback_id.core_shard.cml.template.rendered.cml".into()));
    }

    #[test]
    fn sysinfo_feedback_id_core_shard() {
        let resource_dir = tempfile::TempDir::new().unwrap();
        std::fs::File::create(
            resource_dir.path().join("sysinfo_feedback_id.core_shard.cml.template"),
        )
        .unwrap();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::Eng,
            board_config: &Default::default(),
            gendir: Default::default(),
            resource_dir: Utf8Path::from_path(resource_dir.path()).unwrap().to_path_buf(),
            developer_only_options: Default::default(),
        };

        let forensics_config = ForensicsConfig {
            feedback: FeedbackConfig {
                feedback_id_component_url: FeedbackIdComponentUrl::SysInfo(
                    "fuchsia-pkg://fuchsia.com/test-package#meta/test-component.cm".to_string(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut builder: ConfigurationBuilderImpl = Default::default();
        let result =
            ForensicsSubsystem::define_configuration(&context, &forensics_config, &mut builder);

        assert!(result.is_ok());
        assert!(builder
            .build()
            .core_shards
            .contains(&"sysinfo_feedback_id.core_shard.cml.template.rendered.cml".into()));
    }
}
