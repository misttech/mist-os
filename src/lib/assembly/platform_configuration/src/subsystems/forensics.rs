// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::forensics_config::ForensicsConfig;

pub(crate) struct ForensicsSubsystem;
impl DefineSubsystemConfiguration<ForensicsConfig> for ForensicsSubsystem {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        config: &ForensicsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if config.feedback.low_memory {
            builder.platform_bundle("feedback_low_memory_product_config");
        }
        Ok(())
    }
}
