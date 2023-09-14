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
        match (&ui_config.enabled, context.build_type) {
            (false, _) => return Ok(()),
            (true, BuildType::Eng) => {
                builder.platform_bundle("ui");
                builder.icu_platform_bundle("ui_eng")?;
                builder.platform_bundle("ui_package_eng");
            }
            (true, _) => {
                builder.platform_bundle("ui");
                builder.icu_platform_bundle("ui_user_and_userdebug")?;
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
            .field("renderer", "vulkan")?
            .field(
                "frame_scheduler_min_predicted_frame_duration_in_us",
                ui_config.frame_scheduler_min_predicted_frame_duration_in_us,
            )?
            .field("pointer_auto_focus", ui_config.pointer_auto_focus)?
            .field("display_composition", ui_config.display_composition)?
            .field("i_can_haz_display_id", -1i64)?
            .field("i_can_haz_display_mode", -1i64)?;

        let mut scene_manager_config =
            builder.package("scene_manager").component("meta/scene_manager.cm")?;
        scene_manager_config
            .field(
                "supported_input_devices",
                ui_config
                    .supported_input_devices
                    .iter()
                    .filter_map(|d| serde_json::to_value(d).ok())
                    .collect::<serde_json::Value>(),
            )?
            .field("display_pixel_density", ui_config.display_pixel_density.clone())?
            .field("display_rotation", ui_config.display_rotation)?
            .field("viewing_distance", ui_config.viewing_distance.as_ref())?;

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
