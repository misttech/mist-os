// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::recovery_config::{RecoveryConfig, SystemRecovery};

pub(crate) struct RecoverySubsystem;
impl DefineSubsystemConfiguration<RecoveryConfig> for RecoverySubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &RecoveryConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if config.factory_reset_trigger {
            builder.platform_bundle("factory_reset_trigger");
        }

        if *context.feature_set_level == FeatureSupportLevel::Standard
            || config.system_recovery.is_some()
        {
            // factory_reset is required by the standard feature set level, and when system_recovery
            // is enabled.
            builder.platform_bundle("factory_reset");
        }

        if let Some(system_recovery) = &config.system_recovery {
            context.ensure_feature_set_level(&[FeatureSupportLevel::Utility], "System Recovery")?;
            match system_recovery {
                SystemRecovery::Fdr => builder.platform_bundle("recovery_fdr"),
            }
        }
        Ok(())
    }
}
