// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bstr::ByteSlice;
use fuchsia_component::client::connect_to_protocol_sync;
use linux_uapi::LINUX_REBOOT_CMD_POWER_OFF;
use starnix_sync::{Locked, Unlocked};
#[cfg(feature = "starnix_lite")]
use {fidl_fuchsia_buildinfo as buildinfo, fidl_fuchsia_hardware_power_statecontrol as fpower};
#[cfg(not(feature = "starnix_lite"))]
use {
    fidl_fuchsia_buildinfo as buildinfo, fidl_fuchsia_hardware_power_statecontrol as fpower,
    fidl_fuchsia_recovery as frecovery,
};

use crate::arch::{ARCH_NAME, ARCH_NAME_COMPAT};
#[cfg(not(feature = "starnix_lite"))]
use crate::device::android::bootloader_message_store::BootloaderMessage;
use crate::mm::{MemoryAccessor, MemoryAccessorExt, PAGE_SIZE};
use crate::task::{CurrentTask, Kernel};
use crate::vfs::{FdNumber, FsString};
use starnix_logging::{log_error, log_info, log_warn, track_stub};
#[cfg(feature = "arch32")]
use starnix_syscalls::{for_each_arch32_syscall, syscall_arch32_number_to_name_literal_callback};
use starnix_syscalls::{
    for_each_syscall, syscall_number_to_name_literal_callback, SyscallResult, SUCCESS,
};
use starnix_types::user_buffer::MAX_RW_COUNT;
use starnix_uapi::auth::{CAP_SYS_ADMIN, CAP_SYS_BOOT, CAP_SYS_MODULE};
use starnix_uapi::errors::Errno;
use starnix_uapi::personality::PersonalityFlags;
use starnix_uapi::user_address::{UserAddress, UserCString, UserRef};
use starnix_uapi::version::KERNEL_RELEASE;
use starnix_uapi::{
    c_char, errno, error, from_status_like_fdio, perf_event_attr, pid_t, uapi, utsname, EFAULT,
    GRND_NONBLOCK, GRND_RANDOM, LINUX_REBOOT_CMD_CAD_OFF, LINUX_REBOOT_CMD_CAD_ON,
    LINUX_REBOOT_CMD_HALT, LINUX_REBOOT_CMD_KEXEC, LINUX_REBOOT_CMD_RESTART,
    LINUX_REBOOT_CMD_RESTART2, LINUX_REBOOT_CMD_SW_SUSPEND, LINUX_REBOOT_MAGIC1,
    LINUX_REBOOT_MAGIC2, LINUX_REBOOT_MAGIC2A, LINUX_REBOOT_MAGIC2B, LINUX_REBOOT_MAGIC2C,
};

pub fn do_uname(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    result: &mut utsname,
) -> Result<(), Errno> {
    fn init_array(fixed: &mut [c_char; 65], init: &[u8]) {
        let len = init.len();
        let as_c_char = unsafe { std::mem::transmute::<&[u8], &[c_char]>(init) };
        fixed[..len].copy_from_slice(as_c_char)
    }

    init_array(&mut result.sysname, b"Linux");
    if current_task.thread_group.read().personality.contains(PersonalityFlags::UNAME26) {
        init_array(&mut result.release, b"2.6.40-starnix");
    } else {
        init_array(&mut result.release, KERNEL_RELEASE.as_bytes());
    }

    let version = current_task.kernel().build_version.get_or_try_init(|| {
        let proxy =
            connect_to_protocol_sync::<buildinfo::ProviderMarker>().map_err(|_| errno!(ENOENT))?;
        let buildinfo = proxy.get_build_info(zx::MonotonicInstant::INFINITE).map_err(|e| {
            log_error!("FIDL error getting build info: {e}");
            errno!(EIO)
        })?;
        Ok(buildinfo.version.unwrap_or_else(|| "starnix".to_string()))
    })?;

    init_array(&mut result.version, version.as_bytes());
    // TODO(https://fxbug.dev/380431743) rename property or use personality?
    if cfg!(feature = "arch32") && current_task.thread_state.arch_width.is_arch32() {
        init_array(&mut result.machine, ARCH_NAME_COMPAT);
    } else {
        init_array(&mut result.machine, ARCH_NAME);
    }

    {
        // Get the UTS namespace from the perspective of this task.
        let task_state = current_task.read();
        let uts_ns = task_state.uts_ns.read();
        init_array(&mut result.nodename, uts_ns.hostname.as_slice());
        init_array(&mut result.domainname, uts_ns.domainname.as_slice());
    }
    Ok(())
}

pub fn sys_uname(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    name: UserRef<utsname>,
) -> Result<(), Errno> {
    let mut result = utsname {
        sysname: [0; 65],
        nodename: [0; 65],
        release: [0; 65],
        version: [0; 65],
        machine: [0; 65],
        domainname: [0; 65],
    };
    do_uname(locked, current_task, &mut result)?;
    current_task.write_object(name, &result)?;
    Ok(())
}

pub fn sys_sysinfo(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    info: UserRef<uapi::sysinfo>,
) -> Result<(), Errno> {
    let page_size = zx::system_get_page_size();
    let total_ram_pages = zx::system_get_physmem() / (page_size as u64);
    let num_procs = current_task.kernel().pids.read().len();

    track_stub!(TODO("https://fxbug.dev/297374270"), "compute system load");
    let loads = [0; 3];

    track_stub!(TODO("https://fxbug.dev/322874530"), "compute actual free ram usage");
    let freeram = total_ram_pages / 8;

    let result = uapi::sysinfo {
        uptime: (zx::MonotonicInstant::get() - zx::MonotonicInstant::ZERO).into_seconds(),
        loads,
        totalram: total_ram_pages,
        freeram,
        procs: num_procs.try_into().map_err(|_| errno!(EINVAL))?,
        mem_unit: page_size,
        ..Default::default()
    };

    current_task.write_object(info, &result)?;
    Ok(())
}

// Used to read a hostname or domainname from task memory
fn read_name(current_task: &CurrentTask, name: UserCString, len: u64) -> Result<FsString, Errno> {
    const MAX_LEN: usize = 64;
    let len = len as usize;

    if len > MAX_LEN {
        return error!(EINVAL);
    }

    // Read maximum characters and mark the null terminator.
    let mut name = current_task.read_c_string_to_vec(name, MAX_LEN)?;

    // Syscall may have specified an even smaller length, so trim to the requested length.
    if len < name.len() {
        name.truncate(len);
    }
    Ok(name)
}

pub fn sys_sethostname(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    hostname: UserCString,
    len: u64,
) -> Result<SyscallResult, Errno> {
    if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
        return error!(EPERM);
    }

    let hostname = read_name(current_task, hostname, len)?;

    let task_state = current_task.read();
    let mut uts_ns = task_state.uts_ns.write();
    uts_ns.hostname = hostname;

    Ok(SUCCESS)
}

pub fn sys_setdomainname(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    domainname: UserCString,
    len: u64,
) -> Result<SyscallResult, Errno> {
    if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
        return error!(EPERM);
    }

    let domainname = read_name(current_task, domainname, len)?;

    let task_state = current_task.read();
    let mut uts_ns = task_state.uts_ns.write();
    uts_ns.domainname = domainname;

    Ok(SUCCESS)
}

pub fn sys_getrandom(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    start_addr: UserAddress,
    size: usize,
    flags: u32,
) -> Result<usize, Errno> {
    if flags & !(GRND_RANDOM | GRND_NONBLOCK) != 0 {
        return error!(EINVAL);
    }

    // Copy random bytes in up-to-page-size chunks, stopping either when all the user-requested
    // space has been written to or when we fault.
    let mut bytes_written = 0;
    let mut bounce_buffer = Vec::with_capacity(std::cmp::min(*PAGE_SIZE as usize, size));
    let bounce_buffer = bounce_buffer.spare_capacity_mut();

    let bytes_to_write = std::cmp::min(size, *MAX_RW_COUNT);

    while bytes_written < bytes_to_write {
        let chunk_start = start_addr.saturating_add(bytes_written);
        let chunk_len = std::cmp::min(*PAGE_SIZE as usize, size - bytes_written);

        // Fine to index, chunk_len can't be greater than bounce_buffer.len();
        let chunk = zx::cprng_draw_uninit(&mut bounce_buffer[..chunk_len]);
        match current_task.write_memory_partial(chunk_start, chunk) {
            Ok(n) => {
                bytes_written += n;

                // If we didn't write the whole chunk then we faulted. Don't try to write any more.
                if n < chunk_len {
                    break;
                }
            }

            // write_memory_partial fails if no bytes were written, but we might have
            // written bytes already.
            Err(e) if e.code.error_code() == EFAULT && bytes_written > 0 => break,
            Err(e) => return Err(e),
        }
    }

    Ok(bytes_written)
}

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
    if !current_task.creds().has_capability(CAP_SYS_BOOT) {
        return error!(EPERM);
    }

    let arg_bytes = if matches!(cmd, LINUX_REBOOT_CMD_RESTART2) {
        // This is an arbitrary limit that should be large enough.
        const MAX_REBOOT_ARG_LEN: usize = 256;
        current_task.read_c_string_to_vec(UserCString::new(arg), MAX_REBOOT_ARG_LEN)?
    } else {
        FsString::default()
    };

    let proxy = connect_to_protocol_sync::<fpower::AdminMarker>().or_else(|_| error!(EINVAL))?;

    match cmd {
        // CAD on/off commands turn Ctrl-Alt-Del keystroke on or off without halting the system.
        LINUX_REBOOT_CMD_CAD_ON | LINUX_REBOOT_CMD_CAD_OFF => Ok(()),

        // `kexec_load()` is not supported.
        LINUX_REBOOT_CMD_KEXEC => error!(EINVAL),

        // Suspend is not implemented.
        LINUX_REBOOT_CMD_SW_SUSPEND => error!(EINVAL),

        LINUX_REBOOT_CMD_HALT | LINUX_REBOOT_CMD_POWER_OFF => {
            match proxy.poweroff(zx::MonotonicInstant::INFINITE) {
                Ok(_) => {
                    log_info!("Powering off device.");
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

            let reboot_reason = if reboot_args.contains(&&b"ota_update"[..])
                || reboot_args.contains(&&b"System update during setup"[..])
            {
                fpower::RebootReason::SystemUpdate
            } else if reboot_args.contains(&&b"recovery"[..]) {
                // Read the bootloader message from the misc partition to determine whether the
                // device is rebooting to perform an FDR.
                #[cfg(not(feature = "starnix_lite"))]
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
                fpower::RebootReason::UserRequest
            } else if reboot_args == [b""] // args empty? splitting "" returns [""], not []
                || reboot_args.contains(&&b"shell"[..])
                || reboot_args.contains(&&b"userrequested"[..])
            {
                fpower::RebootReason::UserRequest
            } else {
                log_warn!("Unknown reboot args: {arg_bytes:?}");
                track_stub!(
                    TODO("https://fxbug.dev/322874610"),
                    "unknown reboot args, see logs for strings"
                );
                fpower::RebootReason::UserRequest
            };

            log_info!("Rebooting... reason: {:?}", reboot_reason);
            match proxy.reboot(reboot_reason, zx::MonotonicInstant::INFINITE) {
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

pub fn sys_sched_yield(
    _locked: &mut Locked<'_, Unlocked>,
    _current_task: &CurrentTask,
) -> Result<(), Errno> {
    // SAFETY: This is unsafe because it is a syscall. zx_thread_legacy_yield is always safe.
    let status = unsafe { zx::sys::zx_thread_legacy_yield(0) };
    zx::Status::ok(status).map_err(|status| from_status_like_fdio!(status))
}

pub fn sys_unknown(
    _locked: &mut Locked<'_, Unlocked>,
    #[allow(unused_variables)] current_task: &CurrentTask,
    syscall_number: u64,
) -> Result<SyscallResult, Errno> {
    #[cfg(feature = "arch32")]
    if current_task.thread_state.arch_width.is_arch32() {
        let name = for_each_arch32_syscall! { syscall_arch32_number_to_name_literal_callback, syscall_number };
        track_stub!(TODO("https://fxbug.dev/322874143"), name, syscall_number,);
        return error!(ENOSYS);
    }
    let name = for_each_syscall! { syscall_number_to_name_literal_callback, syscall_number };
    track_stub!(TODO("https://fxbug.dev/322874143"), name, syscall_number,);
    // TODO: We should send SIGSYS once we have signals.
    error!(ENOSYS)
}

pub fn sys_personality(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    persona: u32,
) -> Result<SyscallResult, Errno> {
    let mut state = current_task.task.thread_group.write();
    let previous_value = state.personality.bits();
    if persona != 0xffffffff {
        // Use `from_bits_retain()` since we want to keep unknown flags.
        state.personality = PersonalityFlags::from_bits_retain(persona);
    }
    Ok(previous_value.into())
}

pub fn sys_delete_module(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_name: UserCString,
    _flags: u32,
) -> Result<SyscallResult, Errno> {
    if !current_task.creds().has_capability(CAP_SYS_MODULE) {
        return error!(EPERM);
    }
    // According to LTP test delete_module02.c
    const MODULE_NAME_LEN: usize = 64 - std::mem::size_of::<u64>();
    let _name = current_task.read_c_string_to_vec(user_name, MODULE_NAME_LEN)?;
    // We don't ever have any modules loaded.
    error!(ENOENT)
}

pub fn sys_perf_event_open(
    _locked: &mut Locked<'_, Unlocked>,
    _current_task: &CurrentTask,
    _attr: UserRef<perf_event_attr>,
    __pid: pid_t,
    _cpu: i32,
    _group_fd: FdNumber,
    _flags: u64,
) -> Result<SyscallResult, Errno> {
    track_stub!(TODO("https://fxbug.dev/287120583"), "perf_event_open()");
    error!(ENOSYS)
}

// Syscalls for arch32 usage
#[cfg(feature = "arch32")]
mod arch32 {
    use crate::mm::MemoryAccessorExt;
    use crate::syscalls::misc::do_uname;
    use crate::task::CurrentTask;
    use starnix_sync::{Locked, Unlocked};
    use starnix_uapi::errors::Errno;
    use starnix_uapi::uapi;
    use starnix_uapi::user_address::UserRef;

    pub fn sys_arch32_uname(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        name: UserRef<uapi::arch32::oldold_utsname>,
    ) -> Result<(), Errno> {
        fn trunc(val64: &[u8; 65], val32: &mut [u8; 9]) {
            val32.copy_from_slice(&val64[..9]);
            val32[8] = 0;
        }
        let mut new_result = uapi::utsname {
            sysname: [0; 65],
            nodename: [0; 65],
            release: [0; 65],
            version: [0; 65],
            machine: [0; 65],
            domainname: [0; 65],
        };
        do_uname(locked, current_task, &mut new_result)?;
        let mut old_result: uapi::arch32::oldold_utsname = Default::default();
        trunc(&new_result.sysname, &mut old_result.sysname);
        trunc(&new_result.nodename, &mut old_result.nodename);
        trunc(&new_result.release, &mut old_result.release);
        trunc(&new_result.version, &mut old_result.version);
        let arch32_mach: &str = "armv7l\0\0\0";
        old_result.machine.copy_from_slice(arch32_mach.as_bytes());
        current_task.write_object(name, &old_result)?;
        Ok(())
    }
}

#[cfg(feature = "arch32")]
pub use arch32::*;
