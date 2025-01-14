// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// core
mod error;
mod message;
mod node;
mod power_manager;
mod shutdown_request;
mod timer;
mod utils;

#[path = "../../common/lib/common_utils.rs"]
mod common_utils;
#[path = "../../common/lib/types.rs"]
mod types;

// nodes
mod activity_handler;
mod crash_report_handler;
mod debug_service;
mod input_settings_handler;
mod platform_metrics;
mod shutdown_watcher;
mod system_power_mode_handler;
mod system_profile_handler;
mod system_shutdown_handler;
mod temperature_handler;
mod thermal_load_driver;
mod thermal_policy;
mod thermal_shutdown;
mod thermal_state_handler;

#[cfg(test)]
mod test;

use crate::power_manager::PowerManager;
use anyhow::Error;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    log::info!("started");

    // Setup tracing
    fuchsia_trace_provider::trace_provider_create_with_fdio();

    // Set up the PowerManager
    let mut pm = PowerManager::new();

    // This future should never complete
    let result = pm.run().await;
    log::error!("Unexpected exit with result: {:?}", result);
    result
}
