// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;
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

        // We should only configure scenic here when it has been added to assembly.
        let mut scenic_config = builder.package("scenic").component("meta/scenic.cm")?;
        scenic_config
            .field(
                "frame_scheduler_min_predicted_frame_duration_in_us",
                ui_config.frame_scheduler_min_predicted_frame_duration_in_us,
            )?
            .field("i_can_haz_flatland", true)?
            .field("enable_allocator_for_flatland", true)?
            .field("pointer_auto_focus", ui_config.pointer_auto_focus)?
            .field("flatland_enable_display_composition", false)?
            .field("i_can_haz_display_id", -1i64)?
            .field("i_can_haz_display_mode", -1i64)?;

        let config_dir = builder.add_domain_config("sensor-config").directory("sensor-config");
        if let Some(sensor_config_path) = &ui_config.sensor_config {
            config_dir.entry(FileEntry {
                source: sensor_config_path.clone(),
                destination: "config.json".into(),
            })?;
        }
        Ok(())
    }
}
