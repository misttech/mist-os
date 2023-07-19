// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::forensics_config::ForensicsConfig;
use assembly_config_schema::FileEntry;

pub(crate) struct ForensicsSubsystem;
impl DefineSubsystemConfiguration<ForensicsConfig> for ForensicsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &ForensicsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if *context.feature_set_level == FeatureSupportLevel::Minimal {
            // Add a product-provided cobalt registry if available.
            if let Some(registry_path) = &config.cobalt.registry {
                builder.package("cobalt").config_data(FileEntry {
                    source: registry_path.clone(),
                    destination: "global_metrics_registry.pb".to_string(),
                })?;
            }
            // Otherwise use a default.
            else {
                builder.platform_bundle("cobalt_default_registry");
            }
        }
        Ok(())
    }
}
