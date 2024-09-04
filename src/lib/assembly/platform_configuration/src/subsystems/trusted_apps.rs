// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};
use assembly_config_schema::product_config::TrustedApp as ProductTrustedApp;
use assembly_util::{BootfsPackageDestination, PackageSetDestination};
use fuchsia_tee_manager_config::TAConfig;

pub(crate) struct TrustedAppsSubsystem;
impl DefineSubsystemConfiguration<Vec<ProductTrustedApp>> for TrustedAppsSubsystem {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        config: &Vec<ProductTrustedApp>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
        if config.is_empty() {
            return Ok(());
        }

        builder.platform_bundle("ta_manager");

        // Create a domain config package for all the TA configs.
        let dir = builder
            .add_domain_config(PackageSetDestination::Boot(
                BootfsPackageDestination::TaManagerConfig,
            ))
            .directory("config");

        // Add the configs for all the TAs.
        for c in config {
            let ta_config = TAConfig::new(c.component_url.clone());
            let ta_config = serde_json::to_string(&ta_config)
                .with_context(|| format!("Failed to serialize the ta config: {}", c.guid))?;
            dir.entry_from_contents(&format!("{}.json", c.guid), &ta_config)?;
        }
        Ok(())
    }
}
