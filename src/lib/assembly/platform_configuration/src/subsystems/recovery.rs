// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::recovery_config::RecoveryConfig;

pub(crate) struct RecoverySubsystem;
impl DefineSubsystemConfiguration<RecoveryConfig> for RecoverySubsystem {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        config: &RecoveryConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if config.factory_reset_trigger {
            builder.platform_bundle("factory_reset_trigger");
        }
        Ok(())
    }
}
