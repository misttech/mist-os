// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::starnix_config::PlatformStarnixConfig;

pub(crate) struct StarnixSubsystem;
impl DefineSubsystemConfiguration<PlatformStarnixConfig> for StarnixSubsystem {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        starnix_config: &PlatformStarnixConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if starnix_config.enabled {
            builder.platform_bundle("starnix_support");
        }
        Ok(())
    }
}
