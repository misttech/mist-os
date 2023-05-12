// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{ensure, Context};
use assembly_config_schema::{platform_config::ui_config::PlatformUiConfig, FileEntry};

pub(crate) struct UiSubsystem;

impl DefineSubsystemConfiguration<PlatformUiConfig> for UiSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        ui_config: &PlatformUiConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        match (&ui_config.enabled, &ui_config.legacy, context.build_type) {
            (false, _, _) => return Ok(()),
            // TODO(fxbug.dev/124273): Delete the legacy support once it is consolidated with
            // non-legacy.
            (true, true, BuildType::Eng) => {
                builder.platform_bundle("ui_legacy");
                builder.platform_bundle("ui_eng");
                builder.platform_bundle("ui_legacy_package_eng");
            }
            (true, true, _) => {
                builder.platform_bundle("ui_legacy");
                builder.platform_bundle("ui_user_and_userdebug");
                builder.platform_bundle("ui_legacy_package_user_and_userdebug");
            }
            (true, false, BuildType::Eng) => {
                builder.platform_bundle("ui");
                builder.platform_bundle("ui_eng");
                builder.platform_bundle("ui_package_eng");
            }
            (true, false, _) => {
                builder.platform_bundle("ui");
                builder.platform_bundle("ui_user_and_userdebug");
                builder.platform_bundle("ui_package_user_and_userdebug");
            }
        }

        ensure!(
            *context.feature_set_level == FeatureSupportLevel::Minimal,
            "UI is only supported in the default feature set level"
        );

        if let Some(sensor_config_path) = &ui_config.sensor_config {
            // TODO(fxbug.dev/126530): Make an default empty sensor config once all clients are
            // specifying the config in the product assembly config.
            let config_dir = builder.add_domain_config("sensor-config").directory("sensor-config");

            // If the config is empty, do not write it to the directory.
            // TODO(fxbug.dev/126530): Remove this check once products stop supplying an empty
            // config.
            let contents = std::fs::read_to_string(sensor_config_path)
                .with_context(|| format!("reading sensor config {sensor_config_path}"))?;
            if !contents.trim_end().is_empty() {
                config_dir.entry(FileEntry {
                    source: sensor_config_path.clone(),
                    destination: "config.json".into(),
                })?;
            }
        }
        Ok(())
    }
}
