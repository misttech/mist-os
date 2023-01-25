// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::{
    platform_config::identity_config::PlatformIdentityConfig, FeatureControl,
};

pub(crate) struct IdentitySubsystemConfig;
impl DefineSubsystemConfiguration<PlatformIdentityConfig> for IdentitySubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        identity_config: &PlatformIdentityConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Configure enabling pinweaver.
        let (allow_scrypt, allow_pinweaver) = match (
            context.board_info.provides_feature("fuchsia::cr50"),
            &identity_config.password_pinweaver,
        ) {
            // if the board doesn't support pinweaver:
            // scrypt is always allowed, and pinweaver is not.
            (false, _) => (true, false),
            // if the board supports pinweaver, and pinweaver is required:
            // scrypt is not allowed, and pinweaver is
            (true, FeatureControl::Required) => (false, true),
            // if the board supports pinweaver, and use of pinweaver is allowed:
            // both are allowed
            (true, FeatureControl::Allowed) => (true, true),
            // if the board supports pinweaver, and use of pinweaver is disabled
            // scrypt is allowed, pinweaver is not
            (true, FeatureControl::Disabled) => (true, false),
        };
        builder
            .package("password_authenticator")
            .component("meta/password-authenticator.cm")?
            .field("allow_scrypt", allow_scrypt)?
            .field("allow_pinweaver", allow_pinweaver)?;

        Ok(())
    }
}
