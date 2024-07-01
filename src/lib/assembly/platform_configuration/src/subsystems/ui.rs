// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;
use assembly_config_capabilities::{Config, ConfigNestedValueType, ConfigValueType};
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
        builder.set_config_capability(
            "fuchsia.scenic.Renderer",
            Config::new(
                ConfigValueType::String { max_size: 16 },
                serde_json::to_value(ui_config.renderer.clone())?.into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.FrameSchedulerMinPredictedFrameDurationInUs",
            Config::new(
                ConfigValueType::Uint64,
                ui_config.frame_scheduler_min_predicted_frame_duration_in_us.into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.PointerAutoFocus",
            Config::new(ConfigValueType::Bool, ui_config.pointer_auto_focus.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.DisplayComposition",
            Config::new(ConfigValueType::Bool, ui_config.display_composition.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.ICanHazDisplayId",
            Config::new(ConfigValueType::Int64, (-1i64).into()),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.ICanHazDisplayMode",
            Config::new(ConfigValueType::Int64, (-1i64).into()),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.DisplayRotation",
            Config::new(ConfigValueType::Uint64, ui_config.display_rotation.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.MinDisplayHorizontalResolutionPx",
            Config::new(
                ConfigValueType::Int32,
                ui_config
                    .display_mode_horizontal_resolution_px_range
                    .start
                    .map(|v| v as i32)
                    .unwrap_or(-1)
                    .into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.MaxDisplayHorizontalResolutionPx",
            Config::new(
                ConfigValueType::Int32,
                ui_config
                    .display_mode_horizontal_resolution_px_range
                    .end
                    .map(|v| v as i32)
                    .unwrap_or(-1)
                    .into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.MinDisplayVerticalResolutionPx",
            Config::new(
                ConfigValueType::Int32,
                ui_config
                    .display_mode_vertical_resolution_px_range
                    .start
                    .map(|v| v as i32)
                    .unwrap_or(-1)
                    .into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.MaxDisplayVerticalResolutionPx",
            Config::new(
                ConfigValueType::Int32,
                ui_config
                    .display_mode_vertical_resolution_px_range
                    .end
                    .map(|v| v as i32)
                    .unwrap_or(-1)
                    .into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.MinDisplayRefreshRateMillihertz",
            Config::new(
                ConfigValueType::Int32,
                ui_config
                    .display_mode_refresh_rate_millihertz_range
                    .start
                    .map(|v| v as i32)
                    .unwrap_or(-1)
                    .into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.scenic.MaxDisplayRefreshRateMillihertz",
            Config::new(
                ConfigValueType::Int32,
                ui_config
                    .display_mode_refresh_rate_millihertz_range
                    .end
                    .map(|v| v as i32)
                    .unwrap_or(-1)
                    .into(),
            ),
        )?;

        builder.set_config_capability(
            "fuchsia.ui.SupportedInputDevices",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 12 },
                    max_count: 6,
                },
                ui_config
                    .supported_input_devices
                    .iter()
                    .filter_map(|d| serde_json::to_value(d).ok())
                    .collect::<serde_json::Value>(),
            ),
        )?;

        builder.set_config_capability(
            "fuchsia.ui.DisplayPixelDensity",
            Config::new(
                ConfigValueType::String { max_size: 8 },
                ui_config.display_pixel_density.clone().into(),
            ),
        )?;

        builder.set_config_capability(
            "fuchsia.ui.ViewingDistance",
            Config::new(
                ConfigValueType::String { max_size: 8 },
                ui_config.viewing_distance.as_ref().to_string().into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.ui.IdleThresholdMs",
            Config::new(ConfigValueType::Uint64, {
                match context.build_type {
                    BuildType::Eng => 5000,
                    BuildType::UserDebug | BuildType::User => 100,
                }
                .into()
            }),
        )?;

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
