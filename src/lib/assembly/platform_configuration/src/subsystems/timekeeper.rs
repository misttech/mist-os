// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{Context, Result};

use assembly_config_schema::platform_config::timekeeper_config::TimekeeperConfig;

pub(crate) struct TimekeeperSubsystem;
impl DefineSubsystemConfiguration<TimekeeperConfig> for TimekeeperSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &TimekeeperConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
        let mut config_builder = builder
            .package("timekeeper")
            .component("meta/timekeeper.cm")
            .context("while finding the timekeeper component")?;

        // This is an experimental feature that we want to deploy with care.
        // We originally wanted to deploy on eng builds as well, but it proved
        // to be confusing for debugging.
        //
        // See: b/308199171
        let utc_start_at_startup =
            context.board_info.provides_feature("fuchsia::utc_start_at_startup");

        // Soft crypto boards don't yet have crypto support, so we exit timekeeper
        // early instead of having it crash repeatedly.
        //
        // See: b/299320231
        let early_exit = context.board_info.provides_feature("fuchsia::soft_crypto");

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
            .field("back_off_time_between_pull_samples_sec",
                config.back_off_time_between_pull_samples_sec)?
            .field("first_sampling_delay_sec", config.first_sampling_delay_sec)?
            .field("utc_start_at_startup", utc_start_at_startup)?
            .field("early_exit", early_exit)?
            // TODO: b/295537795 - provide this setting somehow.
            .field("power_topology_integration_enabled", false)?
            .field("rtc_is_read_only", false)?;

        Ok(())
    }
}
