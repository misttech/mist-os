// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_capabilities::{Config, ConfigValueType};
use assembly_config_schema::platform_config::graphics_config::GraphicsConfig;

pub(crate) struct GraphicsSubsystemConfig;
impl DefineSubsystemConfiguration<GraphicsConfig> for GraphicsSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        graphics_config: &GraphicsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let enable_virtual_console = match (
            context.build_type,
            context.feature_set_level,
            graphics_config.enable_virtual_console,
        ) {
            // Use the value if one was specified.
            (_, _, Some(enable_virtual_console)) => enable_virtual_console,
            // If unspecified, virtcon is disabled if it's a user build-type
            (assembly_config_schema::BuildType::User, _, _) => false,
            // If neither of those, disable if we're targeting embeddable as well.
            (_, FeatureSupportLevel::Embeddable, _) => false,
            // Otherwise, enable virtcon.
            (_, _, _) => true,
        };
        if enable_virtual_console {
            builder.platform_bundle("virtcon");
        }

        builder.set_config_capability("fuchsia.virtcon.BootAnimation", Config::new_void())?;
        builder.set_config_capability("fuchsia.virtcon.BufferCount", Config::new_void())?;
        builder.set_config_capability("fuchsia.virtcon.ColorScheme", Config::new_void())?;
        builder.set_config_capability(
            "fuchsia.virtcon.Disable",
            Config::new(ConfigValueType::Bool, (!enable_virtual_console).into()),
        )?;
        if let Some(rotation) = context.board_info.platform.graphics.display.rotation {
            builder.set_config_capability(
                "fuchsia.virtcon.DisplayRotation",
                Config::new(ConfigValueType::Uint32, rotation.into()),
            )?;
        } else {
            builder.set_config_capability("fuchsia.virtcon.DisplayRotation", Config::new_void())?;
        }
        builder.set_config_capability("fuchsia.virtcon.DotsPerInch", Config::new_void())?;
        builder.set_config_capability("fuchsia.virtcon.FontSize", Config::new_void())?;
        builder.set_config_capability("fuchsia.virtcon.KeepLogVisible", Config::new_void())?;
        if let Some(keymap) = &graphics_config.keymap {
            builder.set_config_capability(
                "fuchsia.virtcon.KeyMap",
                Config::new(ConfigValueType::String { max_size: 10 }, keymap.as_str().into()),
            )?;
        } else {
            builder.set_config_capability("fuchsia.virtcon.KeyMap", Config::new_void())?;
        }
        builder.set_config_capability("fuchsia.virtcon.KeyRepeat", Config::new_void())?;
        let rounded_corners = context.board_info.platform.graphics.display.rounded_corners;
        builder.set_config_capability(
            "fuchsia.virtcon.RoundedCorners",
            Config::new(ConfigValueType::Bool, rounded_corners.into()),
        )?;
        builder.set_config_capability("fuchsia.virtcon.ScrollbackRows", Config::new_void())?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ConfigurationBuilderImpl;

    #[test]
    fn test_user_default() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::User,
            ..ConfigurationContext::default_for_tests()
        };
        let config = GraphicsConfig { enable_virtual_console: None, ..Default::default() };
        let mut builder = ConfigurationBuilderImpl::default();
        GraphicsSubsystemConfig::define_configuration(&context, &config, &mut builder).unwrap();
        let config = builder.build();
        assert_eq!(config.bundles, [].into());
    }

    #[test]
    fn test_user_disabled() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::User,
            ..ConfigurationContext::default_for_tests()
        };
        let config = GraphicsConfig { enable_virtual_console: Some(false), ..Default::default() };
        let mut builder = ConfigurationBuilderImpl::default();
        GraphicsSubsystemConfig::define_configuration(&context, &config, &mut builder).unwrap();
        let config = builder.build();
        assert_eq!(config.bundles, [].into());
    }

    #[test]
    fn test_user_enabled() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Standard,
            build_type: &BuildType::User,
            ..ConfigurationContext::default_for_tests()
        };
        let config = GraphicsConfig { enable_virtual_console: Some(true), ..Default::default() };
        let mut builder = ConfigurationBuilderImpl::default();
        GraphicsSubsystemConfig::define_configuration(&context, &config, &mut builder).unwrap();
        let config = builder.build();
        assert_eq!(config.bundles, ["virtcon".to_string()].into());
    }
}
