// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;

pub(crate) struct RadarSubsystemConfig;
impl DefineSubsystemConfiguration<()> for RadarSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if context.board_info.provides_feature("fuchsia::radar") {
            let is_eng_or_user_debug =
                matches!(context.build_type, BuildType::Eng | BuildType::UserDebug);

            builder
                .package("radar-proxy")
                .component("meta/radar-proxy.cm")?
                .field("proxy_radar_burst_reader", is_eng_or_user_debug)?;
        }

        Ok(())
    }
}
