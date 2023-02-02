// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::graphics_config::GraphicsConfig;

pub(crate) struct GraphicsSubsystemConfig;
impl DefineSubsystemConfiguration<GraphicsConfig> for GraphicsSubsystemConfig {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        _graphics_config: &GraphicsConfig,
        _builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Disabled for a soft-transition
        //
        // let enable_virtual_console =
        //     match (context.build_type, graphics_config.enable_virtual_console) {
        //         // Use the value if one was specified.
        //         (_, Some(enable_virtual_console)) => enable_virtual_console,
        //         // If unspecified, virtcon is disabled if it's a user build-type
        //         (assembly_config_schema::BuildType::User, _) => false,
        //         // Otherwise, enable virtcon.
        //         (_, _) => true,
        //     };
        // if enable_virtual_console {
        //     builder.platform_bundle("virtcon");
        // }

        Ok(())
    }
}
