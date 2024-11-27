// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::syscalls::do_clone;
use crate::task::CurrentTask;
use crate::vfs::syscalls::sys_renameat2;
use crate::vfs::FdNumber;
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::errors::Errno;
use starnix_uapi::user_address::{UserAddress, UserCString, UserRef};
use starnix_uapi::{clone_args, pid_t, CSIGNAL};

/// The parameter order for `clone` varies by architecture.
pub fn sys_clone(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    flags: u64,
    user_stack: UserAddress,
    user_parent_tid: UserRef<pid_t>,
    user_tls: UserAddress,
    user_child_tid: UserRef<pid_t>,
) -> Result<pid_t, Errno> {
    // Our flags parameter uses the low 8 bits (CSIGNAL mask) of flags to indicate the exit
    // signal. The CloneArgs struct separates these as `flags` and `exit_signal`.
    do_clone(
        locked,
        current_task,
        &clone_args {
            flags: flags & !(CSIGNAL as u64),
            child_tid: user_child_tid.addr().ptr() as u64,
            parent_tid: user_parent_tid.addr().ptr() as u64,
            exit_signal: flags & (CSIGNAL as u64),
            stack: user_stack.ptr() as u64,
            tls: user_tls.ptr() as u64,
            ..Default::default()
        },
    )
}

pub fn sys_renameat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    old_dir_fd: FdNumber,
    old_user_path: UserCString,
    new_dir_fd: FdNumber,
    new_user_path: UserCString,
) -> Result<(), Errno> {
    sys_renameat2(locked, current_task, old_dir_fd, old_user_path, new_dir_fd, new_user_path, 0)
}

// Syscalls for arch32 usage
#[cfg(feature = "arch32")]
mod arch32 {
    use crate::arch::syscalls::{UserAddress, UserCString, UserRef};
    use crate::mm::syscalls::sys_mmap;
    use crate::mm::{MemoryAccessorExt, PAGE_SIZE};
    use crate::syscalls::misc::do_uname;
    use crate::task::syscalls::do_prlimit64;
    use crate::task::CurrentTask;
    use crate::vfs::syscalls::{lookup_at, sys_faccessat, sys_openat, sys_readlinkat, LookupFlags};
    use crate::vfs::{FdNumber, FsNode};
    use starnix_sync::{Locked, Unlocked};
    use starnix_uapi::errors::Errno;
    use starnix_uapi::file_mode::FileMode;
    use starnix_uapi::uapi::arch32;
    use starnix_uapi::{error, robust_list_head, uapi};

    pub fn sys_arch32_set_robust_list(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_head: UserRef<robust_list_head>, // The actual pointer is irrelevant.
        len: usize,
    ) -> Result<(), Errno> {
        if len != std::mem::size_of::<arch32::robust_list_head>() {
            return error!(EINVAL);
        }
        current_task.write().robust_list_head = user_head;
        Ok(())
    }

    pub fn sys_arch32_open(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
        flags: u32,
        mode: FileMode,
    ) -> Result<FdNumber, Errno> {
        sys_openat(locked, current_task, FdNumber::AT_FDCWD, user_path, flags, mode)
    }

    pub fn sys_arch32_access(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
        mode: u32,
    ) -> Result<(), Errno> {
        sys_faccessat(locked, current_task, FdNumber::AT_FDCWD, user_path, mode)
    }

    pub fn stat64(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        node: &FsNode,
        arch32_stat_buf: UserRef<uapi::arch32::stat64>,
    ) -> Result<(), Errno> {
        let stat_buffer = node.stat(locked, current_task)?;
        let result: uapi::arch32::stat64 = stat_buffer.into();
        // Now we copy to the arch32 version and write.
        current_task.write_object(arch32_stat_buf, &result)?;
        Ok(())
    }

    pub fn sys_arch32_fstat64(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        fd: FdNumber,
        arch32_stat_buf: UserRef<uapi::arch32::stat64>,
    ) -> Result<(), Errno> {
        let file = current_task.files.get_allowing_opath(fd)?;
        stat64(locked, current_task, file.node(), arch32_stat_buf)
    }

    pub fn sys_arch32_stat64(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
        arch32_stat_buf: UserRef<uapi::arch32::stat64>,
    ) -> Result<(), Errno> {
        let name =
            lookup_at(locked, current_task, FdNumber::AT_FDCWD, user_path, LookupFlags::default())?;
        stat64(locked, current_task, &name.entry.node, arch32_stat_buf)
    }

    pub fn sys_arch32_mmap2(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &mut CurrentTask,
        addr: UserAddress,
        length: usize,
        prot: u32,
        flags: u32,
        fd: FdNumber,
        offset: u64,
    ) -> Result<UserAddress, Errno> {
        sys_mmap(locked, current_task, addr, length, prot, flags, fd, offset * *PAGE_SIZE)
    }

    pub fn sys_arch32_munmap(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        addr: UserAddress,
        length: usize,
    ) -> Result<(), Errno> {
        if !addr.is_lower_32bit() || length >= (1 << 32) {
            return error!(EINVAL);
        }
        current_task.mm().unmap(addr, length)?;
        Ok(())
    }

    pub fn sys_arch32_ugetrlimit(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        resource: u32,
        user_rlimit: UserRef<uapi::arch32::rlimit>,
    ) -> Result<(), Errno> {
        let mut limit: uapi::rlimit = Default::default();
        do_prlimit64(locked, current_task, 0, resource, None, &mut limit, !user_rlimit.is_null())?;
        if !user_rlimit.is_null() {
            let arch32_rlimit = uapi::arch32::rlimit {
                rlim_cur: u32::try_from(limit.rlim_cur).unwrap_or(u32::MAX),
                rlim_max: u32::try_from(limit.rlim_max).unwrap_or(u32::MAX),
            };
            current_task.write_object(user_rlimit, &arch32_rlimit)?;
        }
        Ok(())
    }

    #[allow(non_snake_case)]
    pub fn sys_arch32_ARM_set_tls(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &mut CurrentTask,
        addr: UserAddress,
    ) -> Result<(), Errno> {
        current_task.thread_state.registers.set_thread_pointer_register(addr.ptr() as u64);
        Ok(())
    }

    pub fn sys_arch32_uname(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        name: UserRef<arch32::oldold_utsname>,
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
        let mut old_result: arch32::oldold_utsname = Default::default();
        trunc(&new_result.sysname, &mut old_result.sysname);
        trunc(&new_result.nodename, &mut old_result.nodename);
        trunc(&new_result.release, &mut old_result.release);
        trunc(&new_result.version, &mut old_result.version);
        let arch32_mach: &str = "armv7l\0\0\0";
        old_result.machine.copy_from_slice(arch32_mach.as_bytes());
        current_task.write_object(name, &old_result)?;
        Ok(())
    }

    pub fn sys_arch32_readlink(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
        buffer: UserAddress,
        buffer_size: usize,
    ) -> Result<usize, Errno> {
        sys_readlinkat(locked, current_task, FdNumber::AT_FDCWD, user_path, buffer, buffer_size)
    }
}

#[cfg(feature = "arch32")]
pub use arch32::*;
