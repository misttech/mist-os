// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::virtualization_config::PlatformVirtualizationConfig;

pub(crate) struct VirtualizationSubsystem;

impl DefineSubsystemConfiguration<PlatformVirtualizationConfig> for VirtualizationSubsystem {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        virtualization_config: &PlatformVirtualizationConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if virtualization_config.enabled {
            builder.platform_bundle("virtualization_support");
        }
        Ok(())
    }
}
