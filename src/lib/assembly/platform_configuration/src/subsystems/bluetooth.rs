// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::bluetooth_config::{BluetoothConfig, Snoop};

pub(crate) struct BluetoothSubsystemConfig;
impl DefineSubsystemConfiguration<BluetoothConfig> for BluetoothSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &BluetoothConfig,
        _builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Snoop is only useful when Inspect filtering is turned on. In practice, this is in Eng &
        // UserDebug builds.
        // TODO(b/310964410): Currently a no-op. Update rules for snoop after the soft-migration.
        match (context.build_type, config.snoop) {
            (BuildType::User, _) => {}
            (_, Snoop::Eager) => {}
            (_, Snoop::Lazy) => {}
            (_, Snoop::None) => {}
        }

        // TODO(b/292109810): Add rules for the Bluetooth core realm & profiles once the platform
        // configuration has been fully defined.
        Ok(())
    }
}
