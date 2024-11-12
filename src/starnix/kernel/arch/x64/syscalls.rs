// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_sync::{InterruptibleEvent, Locked, Unlocked, WakeReason};

use crate::mm::MemoryAccessorExt;
use crate::signals::syscalls::sys_signalfd4;
use crate::signals::RunState;
use crate::task::syscalls::do_clone;
use crate::task::CurrentTask;
use crate::time::utc;
use crate::vfs::syscalls::{
    poll, sys_dup3, sys_epoll_create1, sys_epoll_pwait, sys_eventfd2, sys_faccessat, sys_fchmodat,
    sys_fchownat, sys_inotify_init1, sys_linkat, sys_mkdirat, sys_mknodat, sys_newfstatat,
    sys_openat, sys_pipe2, sys_readlinkat, sys_renameat2, sys_symlinkat, sys_unlinkat,
};
use crate::vfs::{DirentSink32, FdNumber};
use starnix_logging::track_stub;
use starnix_types::time::{
    duration_from_poll_timeout, duration_from_timeval, timeval_from_duration,
};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::{Errno, ErrnoResultExt};
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::signals::{SigSet, SIGCHLD};
use starnix_uapi::user_address::{UserAddress, UserCString, UserRef};
use starnix_uapi::vfs::EpollEvent;
use starnix_uapi::{
    __kernel_time_t, clone_args, errno, error, gid_t, itimerval, pid_t, pollfd, uapi, uid_t,
    ARCH_SET_FS, ARCH_SET_GS, AT_REMOVEDIR, AT_SYMLINK_NOFOLLOW, CLONE_VFORK, CLONE_VM, CSIGNAL,
    ITIMER_REAL,
};

pub fn sys_access(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    mode: u32,
) -> Result<(), Errno> {
    sys_faccessat(locked, current_task, FdNumber::AT_FDCWD, user_path, mode)
}

pub fn sys_alarm(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    duration: u32,
) -> Result<u32, Errno> {
    let duration = zx::MonotonicDuration::from_seconds(duration.into());
    let new_value = timeval_from_duration(duration);
    let old_value = current_task.thread_group.set_itimer(
        current_task,
        ITIMER_REAL,
        itimerval { it_value: new_value, it_interval: Default::default() },
    )?;

    let remaining = duration_from_timeval(old_value.it_value)?;

    let old_value_seconds = remaining.into_seconds();
    if old_value_seconds == 0 && remaining != zx::MonotonicDuration::default() {
        // We can't return a zero value if the alarm was scheduled even if it had
        // less than one second remaining. Return 1 instead.
        return Ok(1);
    }
    old_value_seconds.try_into().map_err(|_| errno!(EDOM))
}

pub fn sys_arch_prctl(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    code: u32,
    addr: UserAddress,
) -> Result<(), Errno> {
    match code {
        ARCH_SET_FS => {
            current_task.thread_state.registers.fs_base = addr.ptr() as u64;
            Ok(())
        }
        ARCH_SET_GS => {
            current_task.thread_state.registers.gs_base = addr.ptr() as u64;
            Ok(())
        }
        _ => {
            track_stub!(TODO("https://fxbug.dev/322874054"), "arch_prctl", code);
            error!(ENOSYS)
        }
    }
}

pub fn sys_chmod(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    mode: FileMode,
) -> Result<(), Errno> {
    sys_fchmodat(locked, current_task, FdNumber::AT_FDCWD, user_path, mode)
}

pub fn sys_chown(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    owner: uid_t,
    group: gid_t,
) -> Result<(), Errno> {
    sys_fchownat(locked, current_task, FdNumber::AT_FDCWD, user_path, owner, group, 0)
}

/// The parameter order for `clone` varies by architecture.
pub fn sys_clone(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    flags: u64,
    user_stack: UserAddress,
    user_parent_tid: UserRef<pid_t>,
    user_child_tid: UserRef<pid_t>,
    user_tls: UserAddress,
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

pub fn sys_fork(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
) -> Result<pid_t, Errno> {
    do_clone(
        locked,
        current_task,
        &clone_args { exit_signal: uapi::SIGCHLD.into(), ..Default::default() },
    )
}

// https://pubs.opengroup.org/onlinepubs/9699919799/functions/creat.html
pub fn sys_creat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    mode: FileMode,
) -> Result<FdNumber, Errno> {
    sys_open(
        locked,
        current_task,
        user_path,
        (OpenFlags::WRONLY | OpenFlags::CREAT | OpenFlags::TRUNC).bits(),
        mode,
    )
}

pub fn sys_dup2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    oldfd: FdNumber,
    newfd: FdNumber,
) -> Result<FdNumber, Errno> {
    if oldfd == newfd {
        // O_PATH allowed for:
        //
        //  Duplicating the file descriptor (dup(2), fcntl(2)
        //  F_DUPFD, etc.).
        //
        // See https://man7.org/linux/man-pages/man2/open.2.html
        current_task.files.get_allowing_opath(oldfd)?;
        return Ok(newfd);
    }
    sys_dup3(locked, current_task, oldfd, newfd, 0)
}

pub fn sys_epoll_create(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    size: i32,
) -> Result<FdNumber, Errno> {
    if size < 1 {
        // The man page for epoll_create says the size was used in a previous implementation as
        // a hint but no longer does anything. But it's still required to be >= 1 to ensure
        // programs are backwards-compatible.
        return error!(EINVAL);
    }
    sys_epoll_create1(locked, current_task, 0)
}

pub fn sys_epoll_wait(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    epfd: FdNumber,
    events: UserRef<EpollEvent>,
    max_events: i32,
    timeout: i32,
) -> Result<usize, Errno> {
    sys_epoll_pwait(
        locked,
        current_task,
        epfd,
        events,
        max_events,
        timeout,
        UserRef::<SigSet>::default(),
    )
}

pub fn sys_eventfd(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    value: u32,
) -> Result<FdNumber, Errno> {
    sys_eventfd2(locked, current_task, value, 0)
}

pub fn sys_getdents(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    user_buffer: UserAddress,
    user_capacity: usize,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    let mut offset = file.offset.lock();
    let mut sink = DirentSink32::new(current_task, &mut offset, user_buffer, user_capacity);
    let result = file.readdir(locked, current_task, &mut sink);
    sink.map_result_with_actual(result)
}

pub fn sys_getpgrp(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<pid_t, Errno> {
    Ok(current_task.thread_group.read().process_group.leader)
}

pub fn sys_inotify_init(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<FdNumber, Errno> {
    sys_inotify_init1(locked, current_task, 0)
}

pub fn sys_lchown(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    owner: uid_t,
    group: gid_t,
) -> Result<(), Errno> {
    sys_fchownat(
        locked,
        current_task,
        FdNumber::AT_FDCWD,
        user_path,
        owner,
        group,
        AT_SYMLINK_NOFOLLOW,
    )
}

pub fn sys_link(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    old_user_path: UserCString,
    new_user_path: UserCString,
) -> Result<(), Errno> {
    sys_linkat(
        locked,
        current_task,
        FdNumber::AT_FDCWD,
        old_user_path,
        FdNumber::AT_FDCWD,
        new_user_path,
        0,
    )
}

pub fn sys_lstat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    buffer: UserRef<uapi::stat>,
) -> Result<(), Errno> {
    // TODO(https://fxbug.dev/42172993): Add the `AT_NO_AUTOMOUNT` flag once it is supported in
    // `sys_newfstatat`.
    sys_newfstatat(locked, current_task, FdNumber::AT_FDCWD, user_path, buffer, AT_SYMLINK_NOFOLLOW)
}

pub fn sys_mkdir(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    mode: FileMode,
) -> Result<(), Errno> {
    sys_mkdirat(locked, current_task, FdNumber::AT_FDCWD, user_path, mode)
}

pub fn sys_mknod(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    mode: FileMode,
    dev: DeviceType,
) -> Result<(), Errno> {
    sys_mknodat(locked, current_task, FdNumber::AT_FDCWD, user_path, mode, dev)
}

pub fn sys_open(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    flags: u32,
    mode: FileMode,
) -> Result<FdNumber, Errno> {
    sys_openat(locked, current_task, FdNumber::AT_FDCWD, user_path, flags, mode)
}

pub fn sys_pause(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<(), Errno> {
    let event = InterruptibleEvent::new();
    let guard = event.begin_wait();
    let result = current_task.run_in_state(RunState::Event(event.clone()), || {
        match guard.block_until(zx::MonotonicInstant::INFINITE) {
            Err(WakeReason::Interrupted) => error!(ERESTARTNOHAND),
            Err(WakeReason::DeadlineExpired) => panic!("blocking forever cannot time out"),
            Ok(()) => Ok(()),
        }
    });
    result.map_eintr(|| errno!(ERESTARTNOHAND))
}

pub fn sys_pipe(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_pipe: UserRef<FdNumber>,
) -> Result<(), Errno> {
    sys_pipe2(locked, current_task, user_pipe, 0)
}

pub fn sys_poll(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    user_fds: UserRef<pollfd>,
    num_fds: i32,
    timeout: i32,
) -> Result<usize, Errno> {
    let deadline = zx::MonotonicInstant::after(duration_from_poll_timeout(timeout)?);
    poll(locked, current_task, user_fds, num_fds, None, deadline)
}

pub fn sys_readlink(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    buffer: UserAddress,
    buffer_size: usize,
) -> Result<usize, Errno> {
    sys_readlinkat(locked, current_task, FdNumber::AT_FDCWD, user_path, buffer, buffer_size)
}

pub fn sys_rmdir(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
) -> Result<(), Errno> {
    sys_unlinkat(locked, current_task, FdNumber::AT_FDCWD, user_path, AT_REMOVEDIR)
}

pub fn sys_rename(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    old_user_path: UserCString,
    new_user_path: UserCString,
) -> Result<(), Errno> {
    sys_renameat2(
        locked,
        current_task,
        FdNumber::AT_FDCWD,
        old_user_path,
        FdNumber::AT_FDCWD,
        new_user_path,
        0,
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

pub fn sys_stat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    buffer: UserRef<uapi::stat>,
) -> Result<(), Errno> {
    // TODO(https://fxbug.dev/42172993): Add the `AT_NO_AUTOMOUNT` flag once it is supported in
    // `sys_newfstatat`.
    sys_newfstatat(locked, current_task, FdNumber::AT_FDCWD, user_path, buffer, 0)
}

// https://man7.org/linux/man-pages/man2/symlink.2.html
pub fn sys_symlink(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_target: UserCString,
    user_path: UserCString,
) -> Result<(), Errno> {
    sys_symlinkat(locked, current_task, user_target, FdNumber::AT_FDCWD, user_path)
}

pub fn sys_time(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    time_addr: UserRef<__kernel_time_t>,
) -> Result<__kernel_time_t, Errno> {
    let time = (utc::utc_now().into_nanos() / zx::MonotonicDuration::from_seconds(1).into_nanos())
        as __kernel_time_t;
    if !time_addr.is_null() {
        current_task.write_object(time_addr, &time)?;
    }
    Ok(time)
}

pub fn sys_unlink(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
) -> Result<(), Errno> {
    sys_unlinkat(locked, current_task, FdNumber::AT_FDCWD, user_path, 0)
}

pub fn sys_signalfd(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    mask_addr: UserRef<SigSet>,
    mask_size: usize,
) -> Result<FdNumber, Errno> {
    sys_signalfd4(locked, current_task, fd, mask_addr, mask_size, 0)
}

pub fn sys_vfork(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
) -> Result<pid_t, Errno> {
    do_clone(
        locked,
        current_task,
        &clone_args {
            flags: (CLONE_VFORK | CLONE_VM) as u64,
            exit_signal: SIGCHLD.number() as u64,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::{MemoryAccessor, PAGE_SIZE};
    use crate::testing::*;
    use crate::vfs::FdFlags;

    #[::fuchsia::test]
    async fn test_sys_dup2() {
        // Most tests are handled by test_sys_dup3, only test the case where both fds are equals.
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();
        let fd = FdNumber::from_raw(42);
        assert_eq!(sys_dup2(&mut locked, &current_task, fd, fd), error!(EBADF));
        let file_handle = current_task
            .open_file(&mut locked, "data/testfile.txt".into(), OpenFlags::RDONLY)
            .expect("open_file");
        let fd = current_task.add_file(file_handle, FdFlags::empty()).expect("add");
        assert_eq!(sys_dup2(&mut locked, &current_task, fd, fd), Ok(fd));
    }

    #[::fuchsia::test]
    async fn test_sys_creat() -> Result<(), Errno> {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let path_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let path = "newfile.txt";
        current_task.write_memory(path_addr, path.as_bytes())?;
        let fd = sys_creat(
            &mut locked,
            &current_task,
            UserCString::new(path_addr),
            FileMode::default(),
        )?;
        let _file_handle = current_task.open_file(&mut locked, path.into(), OpenFlags::RDONLY)?;
        assert!(!current_task.files.get_fd_flags_allowing_opath(fd)?.contains(FdFlags::CLOEXEC));
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_time() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let time1 = sys_time(&mut locked, &current_task, Default::default()).expect("time");
        assert!(time1 > 0);
        let address = map_memory(
            &mut locked,
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<__kernel_time_t>() as u64,
        );
        zx::MonotonicDuration::from_seconds(2).sleep();
        let time2 = sys_time(&mut locked, &current_task, address.into()).expect("time");
        assert!(time2 >= time1 + 2);
        assert!(time2 < time1 + 10);
        let time3: __kernel_time_t = current_task.read_object(address.into()).expect("read_object");
        assert_eq!(time2, time3);
    }
}
