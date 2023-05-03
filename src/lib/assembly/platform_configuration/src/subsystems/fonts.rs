// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::fonts_config::FontsConfig;

pub(crate) struct FontsSubsystem;
impl DefineSubsystemConfiguration<FontsConfig> for FontsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        fonts_config: &FontsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        builder.package("fonts").component("meta/fonts.cm")?.field(
            "verbose_logging",
            // XXX(fmil): I don't know what I'm doing here. Why do I need a
            // config item if the config is fully determined by the build type?
            // Should I be setting a default?
            matches!(context.build_type, BuildType::Eng | BuildType::UserDebug)
                || fonts_config.verbose_logging,
        )?;

        Ok(())
    }
}
