// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::development_support_config::DevelopmentSupportConfig;

pub(crate) struct DevelopmentConfig;
impl DefineSubsystemConfiguration<Option<DevelopmentSupportConfig>> for DevelopmentConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &Option<DevelopmentSupportConfig>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Select the correct AIB based on the user-provided setting if present
        // and fall-back to the default by build-type.
        let aib_name = match (config, context.build_type) {
            (Some(DevelopmentSupportConfig { enabled: true }), BuildType::User) => {
                anyhow::bail!("Development support is not allowed on user builds");
            }

            // User-provided development setting for non-user builds.
            (Some(DevelopmentSupportConfig { enabled: true }), _) => "kernel_args_eng",
            (Some(DevelopmentSupportConfig { enabled: false }), _) => "kernel_args_user",

            // Default development setting by build-type.
            (None, BuildType::Eng) => "kernel_args_eng",
            (None, BuildType::UserDebug) => "kernel_args_userdebug",
            (None, BuildType::User) => "kernel_args_user",
        };

        builder.platform_bundle(aib_name);
        Ok(())
    }
}
