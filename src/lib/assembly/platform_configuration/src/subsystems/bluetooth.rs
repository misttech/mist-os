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
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Snoop is only useful when Inspect filtering is turned on. In practice, this is in Eng &
        // UserDebug builds.
        match (context.build_type, config.snoop()) {
            (BuildType::User, _) => {}
            (_, Snoop::Eager) => {
                builder.platform_bundle("bluetooth_snoop_eager");
            }
            (_, Snoop::Lazy) => {
                builder.platform_bundle("bluetooth_snoop_lazy");
            }
            (_, Snoop::None) => {}
        }

        // TODO(b/292109810): Add rules for Bluetooth profiles once the platform configuration has
        // been fully defined.
        Ok(())
    }
}
