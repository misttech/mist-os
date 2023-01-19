// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::diagnostics_config::{
    ArchivistConfig, DiagnosticsConfig,
};

pub(crate) struct DiagnosticsSubsystem;
impl DefineSubsystemConfiguration<Option<DiagnosticsConfig>> for DiagnosticsSubsystem {
    fn define_configuration<'a>(
        _context: &ConfigurationContext<'a>,
        diagnostics_config: &Option<DiagnosticsConfig>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if let Some(diagnostics) = diagnostics_config {
            match diagnostics.archivist {
                ArchivistConfig::NoDetectService => {
                    builder.platform_bundle("archivist-no-detect-service")
                }
                ArchivistConfig::NoService => builder.platform_bundle("archivist-no-service"),
                ArchivistConfig::Bringup => builder.platform_bundle("archivist-bringup"),
                ArchivistConfig::DefaultService => builder.platform_bundle("archivist-minimal"),
                ArchivistConfig::LowMem => builder.platform_bundle("archivist-minimal"),
            }
        }
        Ok(())
    }
}
