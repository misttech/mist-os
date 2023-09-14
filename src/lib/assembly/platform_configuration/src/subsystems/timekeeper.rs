// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};
use assembly_config_schema::platform_config::BuildType;

pub(crate) struct TimekeeperSubsystem;
impl DefineSubsystemConfiguration<()> for TimekeeperSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
        let mut config_builder = builder
            .package("timekeeper")
            .component("meta/timekeeper.cm")
            .context("while finding the timekeeper component")?;

        let has_rtc = context.board_info.provides_feature("fuchsia::rtc");

        // This is an experimental feature that we want to deploy with care.
        let utc_start_at_startup = match (has_rtc, context.build_type) {
            // Don't change the behavior on any user builds, so that this change
            // remains invisible to end users.
            //
            // Don't change on any builds that have the RTC feature, as having the RTC
            // will take care of UTC clock startup.
            (false, BuildType::User) | (false, BuildType::UserDebug) | (true, _) => false,

            // Eng builds are OK to start UTC no matter the state of the network or
            // the presence of RTC.  This will help us run some specific tests, and
            // *may* become the overall default at some point in the future.
            (false, BuildType::Eng) => true,
        };

        config_builder
            .field("disable_delays", false)?
            .field("oscillator_error_std_dev_ppm", 15)?
            .field("max_frequency_error_ppm", 30)?
            .field(
                "primary_time_source_url",
                "fuchsia-pkg://fuchsia.com/httpsdate-time-source-pull#meta/httpsdate_time_source.cm",
            )?
            .field("monitor_time_source_url", "")?
            .field("initial_frequency_ppm", 1_000_000)?
            .field("primary_uses_pull", true)?
            .field("monitor_uses_pull", false)?
            .field("back_off_time_between_pull_samples_sec", 300)?

            .field("utc_start_at_startup", utc_start_at_startup)?;

        Ok(())
    }
}
