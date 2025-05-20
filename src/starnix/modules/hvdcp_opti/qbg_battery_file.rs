// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::utils::connect_to_device;
use fidl_fuchsia_hardware_qcom_hvdcpopti as fhvdcpopti;
use starnix_core::mm::MemoryAccessorExt;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync, FileObject, FileOps,
    FsNode,
};
use starnix_logging::{log_error, track_stub};
use starnix_sync::{DeviceOpen, Locked, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::{errno, error};
use zerocopy::FromBytes;

pub const BPIOCXBP: u32 = 0xC1304201;
pub const BPIOCXBPTABLE: u32 = 0xC00C4202;

pub fn create_battery_profile_device(
    _locked: &mut Locked<'_, DeviceOpen>,
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(BatteryProfileFile::new()))
}

struct BatteryProfileFile {
    hvdcpopti: fhvdcpopti::DeviceSynchronousProxy,
}

impl BatteryProfileFile {
    pub fn new() -> Self {
        Self { hvdcpopti: connect_to_device().expect("Could not connect to hvdcpopti service") }
    }
}

impl FileOps for BatteryProfileFile {
    fileops_impl_dataless!();
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);

        match request {
            BPIOCXBP => {
                let config = self
                    .hvdcpopti
                    .get_battery_config(zx::MonotonicInstant::INFINITE)
                    .map_err(|e| {
                        log_error!("GetBatteryConfig failed: {:?}", e);
                        errno!(EINVAL)
                    })?;
                current_task.write_object(UserRef::new(user_addr), &config).map_err(|e| {
                    log_error!("GetBatteryConfig write_object failed: {:?}", e);
                    e
                })?;
                Ok(SUCCESS)
            }
            BPIOCXBPTABLE => {
                #[derive(Debug, FromBytes)]
                struct BatteryProfileIoctlRequest {
                    request: fhvdcpopti::BatteryProfileRequest,
                    addr: UserAddress,
                }

                let profile_request: BatteryProfileIoctlRequest =
                    current_task.read_object(UserRef::new(user_addr)).map_err(|e| {
                        log_error!("GetBatteryProfile read_object failed: {:?}", e);
                        e
                    })?;
                let profile = self
                    .hvdcpopti
                    .get_battery_profile(&profile_request.request, zx::MonotonicInstant::INFINITE)
                    .map_err(|e| {
                        log_error!("Failed to GetBatteryProfile: {:?}", e);
                        errno!(EINVAL)
                    })?
                    .map_err(|e| {
                        log_error!("GetBatteryProfile failed: {:?}", e);
                        errno!(EINVAL)
                    })?;
                current_task.write_object(UserRef::new(profile_request.addr), &profile).map_err(
                    |e| {
                        log_error!("GetBatteryProfile write_object failed: {:?}", e);
                        e
                    },
                )?;
                Ok(SUCCESS)
            }
            unknown_ioctl => {
                track_stub!(
                    TODO("https://fxbug.dev/322874368"),
                    "qbg_battery ioctl",
                    unknown_ioctl
                );
                error!(ENOSYS)
            }
        }?;

        Ok(SUCCESS)
    }
}
