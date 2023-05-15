// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::input_config::PlatformInputConfig;

pub(crate) struct InputSubsystemConfig;
impl DefineSubsystemConfiguration<PlatformInputConfig> for InputSubsystemConfig {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        input_config: &PlatformInputConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Configure the input interaction activity service.
        const DEFAULT_IDLE_THRESHOLD_MINUTES: u64 = 15;
        let mut scene_manager_config =
            builder.package("scene_manager").component("meta/scene_manager.cm")?;

        // Configure the idle threshold
        scene_manager_config.field(
            "idle_threshold_minutes",
            input_config.idle_threshold_minutes.unwrap_or(DEFAULT_IDLE_THRESHOLD_MINUTES),
        )?;

        // Configure the supported input devices. Default to an empty list.
        scene_manager_config.field(
            "supported_input_devices",
            input_config
                .supported_input_devices
                .iter()
                .filter_map(|d| serde_json::to_value(d).ok())
                .collect::<serde_json::Value>(),
        )?;

        Ok(())
    }
}
