// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::input_groups_config::InputGroupsConfig;

pub(crate) struct InputGroupsSubsystem;
impl DefineSubsystemConfiguration<InputGroupsConfig> for InputGroupsSubsystem {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        config: &InputGroupsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if config.group1 {
            builder.platform_bundle("input_group_one");
        }
        if config.group2 {
            builder.platform_bundle("input_group_two");
        }
        Ok(())
    }
}
