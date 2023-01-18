// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_device_manager as fdevice_manager,
    fidl_fuchsia_hardware_power_statecontrol as fpower,
};

/// Type alias for the reboot reasons defined in fuchsia.hardware.power.statecontrol.RebootReason.
pub type RebootReason = fpower::RebootReason;

/// Represents the available shutdown types of the system. These are intended to mirror the
/// supported shutdown APIs of fuchsia.hardware.power.statecontrol.Admin.
#[derive(Debug, PartialEq, PartialOrd, Copy, Clone)]
pub enum ShutdownRequest {
    PowerOff,
    Reboot(RebootReason),
    RebootBootloader,
    RebootRecovery,
    SuspendToRam,
}

/// Converts a ShutdownRequest into a fuchsia.hardare.power.statecontrol.SystemPowerState value.
impl Into<fdevice_manager::SystemPowerState> for ShutdownRequest {
    fn into(self) -> fdevice_manager::SystemPowerState {
        match self {
            ShutdownRequest::PowerOff => fdevice_manager::SystemPowerState::Poweroff,
            ShutdownRequest::Reboot(fpower::RebootReason::OutOfMemory) => {
                fdevice_manager::SystemPowerState::RebootKernelInitiated
            }
            ShutdownRequest::Reboot(_) => fdevice_manager::SystemPowerState::Reboot,
            ShutdownRequest::RebootBootloader => {
                fdevice_manager::SystemPowerState::RebootBootloader
            }
            ShutdownRequest::RebootRecovery => fdevice_manager::SystemPowerState::RebootRecovery,
            ShutdownRequest::SuspendToRam => fdevice_manager::SystemPowerState::SuspendRam,
        }
    }
}
