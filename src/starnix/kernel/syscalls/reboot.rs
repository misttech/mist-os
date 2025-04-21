// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bstr::ByteSlice;
use fuchsia_component::client::connect_to_protocol_sync;
use linux_uapi::{
    LINUX_REBOOT_CMD_CAD_OFF, LINUX_REBOOT_CMD_CAD_ON, LINUX_REBOOT_CMD_HALT,
    LINUX_REBOOT_CMD_KEXEC, LINUX_REBOOT_CMD_POWER_OFF, LINUX_REBOOT_CMD_RESTART,
    LINUX_REBOOT_CMD_RESTART2, LINUX_REBOOT_CMD_SW_SUSPEND,
};
use starnix_logging::{log_debug, log_info, log_warn, track_stub};
use starnix_sync::{InterruptibleEvent, Locked, Unlocked};
use starnix_uapi::auth::CAP_SYS_BOOT;
use starnix_uapi::errors::Errno;
use starnix_uapi::user_address::{UserAddress, UserCString};
use starnix_uapi::{
    errno, error, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2, LINUX_REBOOT_MAGIC2A,
    LINUX_REBOOT_MAGIC2B, LINUX_REBOOT_MAGIC2C,
};
use {fidl_fuchsia_hardware_power_statecontrol as fpower, fidl_fuchsia_recovery as frecovery};

use crate::device::android::bootloader_message_store::BootloaderMessage;
use crate::mm::MemoryAccessorExt;
use crate::security;
use crate::task::{CurrentTask, Kernel};
use crate::vfs::FsString;

#[track_caller]
fn panic_or_error(kernel: &Kernel, errno: Errno) -> Result<(), Errno> {
    if kernel.features.error_on_failed_reboot {
        return Err(errno);
    }
    panic!("Fatal: {errno:?}");
}

pub fn sys_reboot(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    magic: u32,
    magic2: u32,
    cmd: u32,
    arg: UserAddress,
) -> Result<(), Errno> {
    if magic != LINUX_REBOOT_MAGIC1
        || (magic2 != LINUX_REBOOT_MAGIC2
            && magic2 != LINUX_REBOOT_MAGIC2A
            && magic2 != LINUX_REBOOT_MAGIC2B
            && magic2 != LINUX_REBOOT_MAGIC2C)
    {
        return error!(EINVAL);
    }
    security::check_task_capable(current_task, CAP_SYS_BOOT)?;

    let arg_bytes = if matches!(cmd, LINUX_REBOOT_CMD_RESTART2) {
        // This is an arbitrary limit that should be large enough.
        const MAX_REBOOT_ARG_LEN: usize = 256;
        current_task
            .read_c_string_to_vec(UserCString::new(current_task, arg), MAX_REBOOT_ARG_LEN)?
    } else {
        FsString::default()
    };

    if current_task.kernel().is_shutting_down() {
        log_debug!("Ignoring reboot() and parking caller, already shutting down.");
        let event = InterruptibleEvent::new();
        return current_task.block_until(event.begin_wait(), zx::MonotonicInstant::INFINITE);
    }

    let proxy = connect_to_protocol_sync::<fpower::AdminMarker>().or_else(|_| error!(EINVAL))?;

    match cmd {
        // CAD on/off commands turn Ctrl-Alt-Del keystroke on or off without halting the system.
        LINUX_REBOOT_CMD_CAD_ON | LINUX_REBOOT_CMD_CAD_OFF => Ok(()),

        // `kexec_load()` is not supported.
        LINUX_REBOOT_CMD_KEXEC => error!(EINVAL),

        // Suspend is not implemented.
        LINUX_REBOOT_CMD_SW_SUSPEND => error!(EINVAL),

        LINUX_REBOOT_CMD_HALT | LINUX_REBOOT_CMD_POWER_OFF => {
            log_info!("Powering off");
            match proxy.poweroff(zx::MonotonicInstant::INFINITE) {
                Ok(_) => {
                    // System is rebooting... wait until runtime ends.
                    zx::MonotonicInstant::INFINITE.sleep();
                }
                Err(e) => {
                    return panic_or_error(
                        current_task.kernel(),
                        errno!(EINVAL, format!("Failed to power off, status: {e}")),
                    )
                }
            }
            Ok(())
        }

        LINUX_REBOOT_CMD_RESTART | LINUX_REBOOT_CMD_RESTART2 => {
            let reboot_args: Vec<_> = arg_bytes.split_str(b",").collect();

            if reboot_args.contains(&&b"bootloader"[..]) {
                log_info!("Rebooting to bootloader");
                match proxy.reboot_to_bootloader(zx::MonotonicInstant::INFINITE) {
                    Ok(_) => {
                        // System is rebooting... wait until runtime ends.
                        zx::MonotonicInstant::INFINITE.sleep();
                    }
                    Err(e) => {
                        return panic_or_error(
                            current_task.kernel(),
                            errno!(EINVAL, format!("Failed to reboot, status: {e}")),
                        )
                    }
                }
            }

            // TODO(https://391585107): Loop through all the arguments and
            // generate a list of reboot reasons.
            let reboot_reason = if reboot_args.contains(&&b"ota_update"[..])
                || reboot_args.contains(&&b"System update during setup"[..])
            {
                fpower::RebootReason2::SystemUpdate
            } else if reboot_args.contains(&&b"recovery"[..]) {
                // Read the bootloader message from the misc partition to determine whether the
                // device is rebooting to perform an FDR.
                if let Some(store) = current_task.kernel().bootloader_message_store.get() {
                    match store.read_bootloader_message() {
                        Ok(BootloaderMessage::BootRecovery(args)) => {
                            if args.iter().any(|arg| arg == "--wipe_data") {
                                let factory_reset_proxy =
                                    connect_to_protocol_sync::<frecovery::FactoryResetMarker>()
                                        .or_else(|_| error!(EINVAL))?;
                                // NB: This performs a reboot for us.
                                log_info!("Initiating factory data reset...");
                                match factory_reset_proxy.reset(zx::MonotonicInstant::INFINITE) {
                                    Ok(_) => {
                                        // System is rebooting... wait until runtime ends.
                                        zx::MonotonicInstant::INFINITE.sleep();
                                    }
                                    Err(e) => {
                                        return panic_or_error(
                                            current_task.kernel(),
                                            errno!(
                                                EINVAL,
                                                format!("Failed to reboot for FDR, status: {e}")
                                            ),
                                        )
                                    }
                                }
                            }
                        }
                        // In all other cases, fall through to a regular reboot.
                        Ok(_) => log_info!("Boot message not recognized!"),
                        Err(e) => log_warn!("Failed to read boot message: {e}"),
                    }
                }
                log_warn!("Recovery mode isn't supported yet, rebooting as normal...");
                fpower::RebootReason2::UserRequest
            } else if reboot_args == [b""] // args empty? splitting "" returns [""], not []
                || reboot_args.contains(&&b"shell"[..])
                || reboot_args.contains(&&b"userrequested"[..])
            {
                fpower::RebootReason2::UserRequest
            } else {
                log_warn!("Unknown reboot args: {arg_bytes:?}");
                track_stub!(
                    TODO("https://fxbug.dev/322874610"),
                    "unknown reboot args, see logs for strings"
                );
                fpower::RebootReason2::UserRequest
            };

            log_info!("Rebooting... reason: {:?}", reboot_reason);
            match proxy.perform_reboot(
                &fpower::RebootOptions { reasons: Some(vec![reboot_reason]), ..Default::default() },
                zx::MonotonicInstant::INFINITE,
            ) {
                Ok(_) => {
                    // System is rebooting... wait until runtime ends.
                    zx::MonotonicInstant::INFINITE.sleep();
                }
                Err(e) => {
                    return panic_or_error(
                        current_task.kernel(),
                        errno!(EINVAL, format!("Failed to reboot, status: {e}")),
                    )
                }
            }
            Ok(())
        }

        _ => error!(EINVAL),
    }
}

#[cfg(feature = "arch32")]
mod arch32 {
    pub use super::sys_reboot as sys_arch32_reboot;
}

#[cfg(feature = "arch32")]
pub use arch32::*;
