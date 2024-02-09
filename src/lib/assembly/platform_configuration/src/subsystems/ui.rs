// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;
use assembly_config_schema::platform_config::ui_config::PlatformUiConfig;
use assembly_util::{FileEntry, PackageDestination, PackageSetDestination};

pub(crate) struct UiSubsystem;

impl DefineSubsystemConfiguration<PlatformUiConfig> for UiSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        ui_config: &PlatformUiConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if !ui_config.enabled {
            return Ok(());
        }
        match context.build_type {
            BuildType::Eng => {
                builder.platform_bundle("ui");
                builder.icu_platform_bundle("ui_eng")?;
                match &ui_config.with_synthetic_device_support {
                    true => {
                        builder.platform_bundle(
                            "ui_package_eng_userdebug_with_synthetic_device_support",
                        );
                    }
                    false => {
                        builder.platform_bundle("ui_package_eng");
                    }
                }
            }
            BuildType::UserDebug => {
                builder.platform_bundle("ui");
                builder.icu_platform_bundle("ui_user_and_userdebug")?;
                match &ui_config.with_synthetic_device_support {
                    true => {
                        builder.platform_bundle(
                            "ui_package_eng_userdebug_with_synthetic_device_support",
                        );
                    }
                    false => {
                        builder.platform_bundle("ui_package_user_and_userdebug");
                    }
                }
            }
            BuildType::User => {
                builder.platform_bundle("ui");
                builder.icu_platform_bundle("ui_user_and_userdebug")?;
                builder.platform_bundle("ui_package_user_and_userdebug");
            }
        }

        ensure!(
            *context.feature_set_level == FeatureSupportLevel::Standard,
            "UI is only supported in the default feature set level"
        );

        // We should only configure scenic here when it has been added to assembly.
        let mut scenic_config = builder.package("scenic").component("meta/scenic.cm")?;
        scenic_config
            .field("renderer", serde_json::to_value(ui_config.renderer.clone())?)?
            .field(
                "frame_scheduler_min_predicted_frame_duration_in_us",
                ui_config.frame_scheduler_min_predicted_frame_duration_in_us,
            )?
            .field("pointer_auto_focus", ui_config.pointer_auto_focus)?
            .field("display_composition", ui_config.display_composition)?
            .field("i_can_haz_display_id", -1i64)?
            .field("i_can_haz_display_mode", -1i64)?
            .field("display_rotation", ui_config.display_rotation)?
            .field(
                "min_display_horizontal_resolution_px",
                ui_config
                    .display_mode_horizontal_resolution_px_range
                    .start
                    .map(|v| v as i32)
                    .unwrap_or(-1),
            )?
            .field(
                "max_display_horizontal_resolution_px",
                ui_config
                    .display_mode_horizontal_resolution_px_range
                    .end
                    .map(|v| v as i32)
                    .unwrap_or(-1),
            )?
            .field(
                "min_display_vertical_resolution_px",
                ui_config
                    .display_mode_vertical_resolution_px_range
                    .start
                    .map(|v| v as i32)
                    .unwrap_or(-1),
            )?
            .field(
                "max_display_vertical_resolution_px",
                ui_config
                    .display_mode_vertical_resolution_px_range
                    .end
                    .map(|v| v as i32)
                    .unwrap_or(-1),
            )?
            .field(
                "min_display_refresh_rate_millihertz",
                ui_config
                    .display_mode_refresh_rate_millihertz_range
                    .start
                    .map(|v| v as i32)
                    .unwrap_or(-1),
            )?
            .field(
                "max_display_refresh_rate_millihertz",
                ui_config
                    .display_mode_refresh_rate_millihertz_range
                    .end
                    .map(|v| v as i32)
                    .unwrap_or(-1),
            )?;

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

        let config_dir = builder
            .add_domain_config(PackageSetDestination::Blob(PackageDestination::SensorConfig))
            .directory("sensor-config");
        if let Some(sensor_config_path) = &ui_config.sensor_config {
            config_dir.entry(FileEntry {
                source: sensor_config_path.clone(),
                destination: "config.json".into(),
            })?;
        }

        if let Some(brightness_manager) = &ui_config.brightness_manager {
            builder.platform_bundle("brightness_manager");
            let mut brightness_config =
                builder.package("brightness_manager").component("meta/brightness_manager.cm")?;
            if brightness_manager.with_display_power {
                brightness_config
                    .field("manage_display_power", true)?
                    .field("power_on_delay_millis", 35)?
                    .field("power_off_delay_millis", 85)?;
            } else {
                brightness_config
                    .field("manage_display_power", false)?
                    .field("power_on_delay_millis", 0)?
                    .field("power_off_delay_millis", 0)?;
            }
        }

        Ok(())
    }
}
