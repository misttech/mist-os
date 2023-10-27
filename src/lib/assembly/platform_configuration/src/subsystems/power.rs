// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use assembly_config_schema::FileEntry;

use crate::subsystems::prelude::*;

pub(crate) struct PowerManagementSubsystem;

impl DefineSubsystemConfiguration<()> for PowerManagementSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _config: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if let Some(power_manager_config) = &context.board_info.configuration.power_manager {
            builder
                .bootfs()
                .file(FileEntry {
                    source: power_manager_config.as_utf8_pathbuf().into(),
                    destination: "config/power_manager/node_config.json".into(),
                })
                .context("Adding power_manager config file")?;
        }
        Ok(())
    }
}
