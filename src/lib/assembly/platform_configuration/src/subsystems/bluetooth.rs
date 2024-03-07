// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::format_err;

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
            (_, Snoop::None) => {}
            (BuildType::User, _) => return Err(format_err!("Snoop forbidden on user builds")),
            (_, Snoop::Eager) => {
                builder.platform_bundle("bluetooth_snoop_eager");
            }
            (_, Snoop::Lazy) => {
                builder.platform_bundle("bluetooth_snoop_lazy");
            }
        }

        let BluetoothConfig::Standard { profiles, .. } = config else {
            return Ok(());
        };

        // Bluetooth Core & Profile packages can only be added to the Standard platform
        // service level.
        if *context.feature_set_level != FeatureSupportLevel::Standard {
            return Err(format_err!(
                "Bluetooth core & profiles are forbidden on non-Standard service levels"
            ));
        }
        builder.platform_bundle("bluetooth_core");

        // TODO(b/292109810): Add relevant AIBs when OOT product assembly has been updated.
        if profiles.a2dp.enabled {}
        if profiles.avrcp.enabled {}
        if profiles.hfp.enabled {}

        Ok(())
    }
}
