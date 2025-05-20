// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_feedback::{LastRebootInfoProviderMarker, RebootReason};
use fuchsia_component::client::connect_to_protocol_sync;
use log::{debug, info};

/// Timeout for FIDL calls to LastRebootInfoProvider
const LRIP_FIDL_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::INFINITE;

/// Get an Android-compatible boot reason suitable to add to the cmdline or bootconfig.
pub fn get_android_bootreason() -> Result<&'static str, Error> {
    info!("Converting LastRebootInfo to an android-friendly bootreason.");
    let reboot_info_proxy = connect_to_protocol_sync::<LastRebootInfoProviderMarker>()?;
    let deadline = zx::MonotonicInstant::after(LRIP_FIDL_TIMEOUT);
    let reboot_info = reboot_info_proxy.get(deadline)?;

    match reboot_info.reason {
        Some(RebootReason::Unknown) => return Ok("reboot,unknown"),
        Some(RebootReason::Cold) => return Ok("reboot,cold"),
        Some(RebootReason::BriefPowerLoss) => return Ok("reboot,powerloss"),
        Some(RebootReason::Brownout) => return Ok("reboot,undervoltage"),
        Some(RebootReason::KernelPanic) => return Ok("kernel_panic"),
        Some(RebootReason::SystemOutOfMemory) => return Ok("kernel_panic,oom"),
        Some(RebootReason::HardwareWatchdogTimeout) => return Ok("watchdog"),
        Some(RebootReason::SoftwareWatchdogTimeout) => return Ok("watchdog,sw"),
        Some(RebootReason::RootJobTermination) => return Ok("kernel_panic"),
        Some(RebootReason::UserRequest) => return Ok("reboot,userrequested"),
        Some(RebootReason::RetrySystemUpdate) => return Ok("reboot,ota"),
        Some(RebootReason::HighTemperature) => return Ok("shutdown,thermal"),
        Some(RebootReason::SessionFailure) => return Ok("kernel_panic"),
        Some(RebootReason::SysmgrFailure) => return Ok("kernel_panic"),
        Some(RebootReason::FactoryDataReset) => return Ok("reboot,factory_reset"),
        Some(RebootReason::CriticalComponentFailure) => return Ok("kernel_panic"),
        Some(RebootReason::ZbiSwap) => return Ok("reboot,normal"),
        Some(RebootReason::SystemUpdate) => return Ok("reboot,ota"),
        Some(RebootReason::NetstackMigration) => return Ok("reboot,normal"),
        Some(RebootReason::__SourceBreaking { .. }) => return Ok("reboot,normal"),
        None => return Ok("reboot,unknown"),
    }
}

/// Get contents for the pstore/console-ramoops* file.
///
/// In Linux it contains a limited amount of some of the previous boot's kernel logs.
pub fn get_console_ramoops() -> Option<Vec<u8>> {
    debug!("Getting console-ramoops contents");
    // Placeholder while we figure out how to get the proper logs.
    Some("Fuchsia Console Ramoops\n".as_bytes().to_vec())
}
