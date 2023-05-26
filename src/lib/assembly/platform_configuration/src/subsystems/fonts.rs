// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::Context;
use assembly_config_schema::platform_config::fonts_config::FontsConfig;

pub(crate) struct FontsSubsystem;
impl DefineSubsystemConfiguration<FontsConfig> for FontsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        fonts_config: &FontsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Adding the platform bundle conditionally allows us to soft-migrate
        // products that use the packages from the `fonts` bundle.
        if *context.feature_set_level == FeatureSupportLevel::Minimal && fonts_config.enabled {
            builder.platform_bundle("fonts");
            let mut fonts = builder.package("fonts").component("meta/fonts.cm")?;
            fonts
                .field(
                    "verbose_logging",
                    matches!(context.build_type, BuildType::Eng | BuildType::UserDebug),
                )
                .context("while setting verbose_logging")?
                // This will be filled in once we switch to asset provided font
                // manifests.
                .field("font_manifest", String::from(""))
                .context("while setting font_manifest")?;
        }
        Ok(())
    }
}
