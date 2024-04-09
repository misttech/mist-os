// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use bstr::ByteSlice;
use fidl_fuchsia_buildinfo as buildinfo;
use fidl_fuchsia_hardware_power_statecontrol as fpower;
use fuchsia_component::client::connect_to_protocol_sync;
use linux_uapi::LINUX_REBOOT_CMD_POWER_OFF;
use starnix_sync::{Locked, Unlocked};

use crate::{
    mm::{read_to_vec, MemoryAccessor, MemoryAccessorExt, NumberOfElementsRead},
    task::CurrentTask,
    vfs::{FdNumber, FsString},
};
use starnix_logging::{log_error, log_info, log_warn, track_stub};
use starnix_syscalls::{
    for_each_syscall, syscall_number_to_name_literal_callback, SyscallResult, SUCCESS,
};
use starnix_uapi::{
    auth::{CAP_SYS_ADMIN, CAP_SYS_BOOT},
    c_char, errno, error,
    errors::Errno,
    from_status_like_fdio, perf_event_attr,
    personality::PersonalityFlags,
    pid_t, uapi,
    user_address::{UserAddress, UserCString, UserRef},
    utsname, GRND_NONBLOCK, GRND_RANDOM, LINUX_REBOOT_CMD_CAD_OFF, LINUX_REBOOT_CMD_CAD_ON,
    LINUX_REBOOT_CMD_HALT, LINUX_REBOOT_CMD_KEXEC, LINUX_REBOOT_CMD_RESTART,
    LINUX_REBOOT_CMD_RESTART2, LINUX_REBOOT_CMD_SW_SUSPEND, LINUX_REBOOT_MAGIC1,
    LINUX_REBOOT_MAGIC2, LINUX_REBOOT_MAGIC2A, LINUX_REBOOT_MAGIC2B, LINUX_REBOOT_MAGIC2C,
};

pub fn sys_uname(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    name: UserRef<utsname>,
) -> Result<(), Errno> {
    fn init_array(fixed: &mut [c_char; 65], init: &[u8]) {
        let len = init.len();
        let as_c_char = unsafe { std::mem::transmute::<&[u8], &[c_char]>(init) };
        fixed[..len].copy_from_slice(as_c_char)
    }

    let mut result = utsname {
        sysname: [0; 65],
        nodename: [0; 65],
        release: [0; 65],
        version: [0; 65],
        machine: [0; 65],
        domainname: [0; 65],
    };

    init_array(&mut result.sysname, b"Linux");
    if current_task.thread_group.read().personality.contains(PersonalityFlags::UNAME26) {
        init_array(&mut result.release, b"2.6.40-starnix");
    } else {
        init_array(&mut result.release, b"5.10.107-starnix");
    }

    let version = current_task.kernel().build_version.get_or_try_init(|| {
        let proxy =
            connect_to_protocol_sync::<buildinfo::ProviderMarker>().map_err(|_| errno!(ENOENT))?;
        let buildinfo = proxy.get_build_info(zx::Time::INFINITE).map_err(|e| {
            log_error!("FIDL error getting build info: {e}");
            errno!(EIO)
        })?;
        Ok(buildinfo.version.unwrap_or("starnix".to_string()))
    })?;

    init_array(&mut result.version, version.as_bytes());
    init_array(&mut result.machine, b"x86_64");

    {
        // Get the UTS namespace from the perspective of this task.
        let task_state = current_task.read();
        let uts_ns = task_state.uts_ns.read();
        init_array(&mut result.nodename, uts_ns.hostname.as_slice());
        init_array(&mut result.domainname, uts_ns.domainname.as_slice());
    }

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
        uptime: (zx::Time::get_monotonic() - zx::Time::ZERO).into_seconds(),
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
    buf_addr: UserAddress,
    size: usize,
    flags: u32,
) -> Result<usize, Errno> {
    if flags & !(GRND_RANDOM | GRND_NONBLOCK) != 0 {
        return error!(EINVAL);
    }

    // SAFETY: The callback returns the number of bytes read.
    let buf = unsafe {
        read_to_vec::<u8, _>(size, |b| Ok(NumberOfElementsRead(zx::cprng_draw_uninit(b).len())))
    }?;
    current_task.write_memory(buf_addr, &buf)?;
    Ok(size)
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

    let proxy = connect_to_protocol_sync::<fpower::AdminMarker>().or_else(|_| error!(EINVAL))?;

    match cmd {
        // CAD on/off commands turn Ctrl-Alt-Del keystroke on or off without halting the system.
        LINUX_REBOOT_CMD_CAD_ON | LINUX_REBOOT_CMD_CAD_OFF => Ok(()),

        // `kexec_load()` is not supported.
        LINUX_REBOOT_CMD_KEXEC => error!(EINVAL),

        // Suspend is not implemented.
        LINUX_REBOOT_CMD_SW_SUSPEND => error!(EINVAL),

        LINUX_REBOOT_CMD_HALT | LINUX_REBOOT_CMD_POWER_OFF => {
            match proxy.poweroff(zx::Time::INFINITE) {
                Ok(_) => {
                    log_info!("Powering off device.");
                    // System is rebooting... wait until runtime ends.
                    zx::Time::INFINITE.sleep();
                }
                Err(e) => return error!(EINVAL, format!("Failed to power off, status: {e}")),
            }
            Ok(())
        }

        LINUX_REBOOT_CMD_RESTART | LINUX_REBOOT_CMD_RESTART2 => {
            // This is an arbitrary limit that should be large enough.
            const MAX_REBOOT_ARG_LEN: usize = 256;
            let arg_bytes = current_task
                .read_c_string_to_vec(UserCString::new(arg), MAX_REBOOT_ARG_LEN)
                .unwrap_or_default();
            let reboot_args: Vec<_> = arg_bytes.split_str(b",").collect();

            if reboot_args.contains(&&b"bootloader"[..]) {
                log_info!("Rebooting to bootloader");
                match proxy.reboot_to_bootloader(zx::Time::INFINITE) {
                    Ok(_) => {
                        // System is rebooting... wait until runtime ends.
                        zx::Time::INFINITE.sleep();
                    }
                    Err(e) => return error!(EINVAL, format!("Failed to reboot, status: {e}")),
                }
            }

            let reboot_reason = if reboot_args.contains(&&b"ota_update"[..])
                || reboot_args.contains(&&b"System update during setup"[..])
            {
                fpower::RebootReason::SystemUpdate
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
            match proxy.reboot(reboot_reason, zx::Time::INFINITE) {
                Ok(_) => {
                    // System is rebooting... wait until runtime ends.
                    zx::Time::INFINITE.sleep();
                }
                Err(e) => return error!(EINVAL, format!("Failed to reboot, status: {e}")),
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
    _current_task: &CurrentTask,
    syscall_number: u64,
) -> Result<SyscallResult, Errno> {
    track_stub!(
        TODO("https://fxbug.dev/322874143"),
        for_each_syscall! { syscall_number_to_name_literal_callback, syscall_number },
        syscall_number,
    );
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
