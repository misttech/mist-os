// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{anyhow, Context, Result};

use assembly_config_schema::platform_config::timekeeper_config::TimekeeperConfig;

pub(crate) struct TimekeeperSubsystem;
impl DefineSubsystemConfiguration<TimekeeperConfig> for TimekeeperSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &TimekeeperConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> Result<()> {
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

        // Some e2e tests need to change Timekeeper behavior at runtime. This setting
        // allows Timekeeper to serve an endpoint for such runtime behavior changes.
        // Only eng builds are allowed to have this feature.
        let serve_test_protocols = config.serve_test_protocols;

        // See:
        // https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0115_build_types?hl=en#definitions
        if serve_test_protocols {
            if *context.build_type != BuildType::Eng {
                return Err(anyhow!(
                    "`serve_test_protocols==true` is only allowed in `eng` builds, see RFC 0115"
                ));
            }
            // Gives Timekeeper some mutable persistent storage ("data").
            builder.platform_bundle("timekeeper_persistence");
        }

        // Allows Timekeeper to short-circuit RTC driver detection at startup.
        let has_real_time_clock = context.board_info.provides_feature("fuchsia::real_time_clock");

        let serve_fuchsia_time_alarms = config.serve_fuchsia_time_alarms;

        let mut config_builder = builder
            .package("timekeeper")
            .component("meta/timekeeper.cm")
            .context("while finding the timekeeper component")?;

        // Refer to //src/sys/time/timekeeper/config.shard.cml
        // for details.
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
            .field("serve_test_protocols", serve_test_protocols)?
            .field("has_real_time_clock", has_real_time_clock)?
            .field("utc_start_at_startup_when_invalid_rtc", config.utc_start_at_startup_when_invalid_rtc)?
            .field("serve_fuchsia_time_alarms", serve_fuchsia_time_alarms)?;

        let mut time_source_config_builder = builder
            .package("httpsdate-time-source-pull")
            .component("meta/httpsdate_time_source.cm")
            .context("while finding the time source component")?;

        // Refer to //src/sys/time/httpsdate_time_source/meta/service.cml
        // for details.
        time_source_config_builder
            .field("https_timeout_sec", 10)?
            .field("standard_deviation_bound_percentage", 30)?
            .field("first_rtt_time_factor", 5)?
            .field("use_pull_api", true)?
            .field("max_attempts_urgency_low", 3)?
            .field("num_polls_urgency_low", 7)?
            .field("max_attempts_urgency_medium", 3)?
            .field("num_polls_urgency_medium", 5)?
            .field("max_attempts_urgency_high", 3)?
            .field("num_polls_urgency_high", 3)?
            .field("time_source_endpoint_url", &*config.time_source_endpoint_url)?;

        Ok(())
    }
}
