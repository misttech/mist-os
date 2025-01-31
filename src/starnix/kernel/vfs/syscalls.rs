// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{MemoryAccessor, MemoryAccessorExt, PAGE_SIZE};
use crate::security;
use crate::task::{
    CurrentTask, EnqueueEventHandler, EventHandler, ReadyItem, ReadyItemKey, Task, Timeline,
    TimerWakeup, Waiter,
};
use crate::vfs::aio::AioContext;
use crate::vfs::buffers::{UserBuffersInputBuffer, UserBuffersOutputBuffer};
use crate::vfs::eventfd::{new_eventfd, EventFdType};
use crate::vfs::fs_args::MountParams;
use crate::vfs::inotify::InotifyFileObject;
use crate::vfs::io_uring::{IoUringFileObject, IORING_MAX_ENTRIES};
use crate::vfs::pidfd::new_pidfd;
use crate::vfs::pipe::{new_pipe, PipeFileObject};
use crate::vfs::timer::TimerFile;
use crate::vfs::{
    checked_add_offset_and_length, new_memfd, splice, CheckAccessReason, DirentSink64,
    EpollFileObject, EpollKey, FallocMode, FdFlags, FdNumber, FileAsyncOwner, FileHandle,
    FileSystemOptions, FlockOperation, FsStr, FsString, LookupContext, NamespaceNode,
    PathWithReachability, RecordLockCommand, RenameFlags, SeekTarget, StatxFlags, SymlinkMode,
    SymlinkTarget, TargetFdNumber, TimeUpdateType, UnlinkKind, ValueOrSize, WdNumber, WhatToMount,
    XattrOp,
};
use starnix_logging::{log_trace, track_stub};
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::time::{
    duration_from_poll_timeout, duration_from_timespec, time_from_timespec, timespec_from_duration,
};
use starnix_types::user_buffer::UserBuffer;
use starnix_uapi::auth::{
    CAP_BLOCK_SUSPEND, CAP_DAC_READ_SEARCH, CAP_LEASE, CAP_SYS_ADMIN, CAP_WAKE_ALARM,
    PTRACE_MODE_ATTACH_REALCREDS,
};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::{
    Errno, ErrnoResultExt, EFAULT, EINTR, ENAMETOOLONG, ENOTSUP, ETIMEDOUT,
};
use starnix_uapi::file_lease::FileLeaseType;
use starnix_uapi::file_mode::{Access, AccessCheck, FileMode};
use starnix_uapi::inotify_mask::InotifyMask;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::personality::PersonalityFlags;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::seal_flags::SealFlags;
use starnix_uapi::signals::SigSet;
use starnix_uapi::unmount_flags::UnmountFlags;
use starnix_uapi::user_address::{UserAddress, UserCString, UserRef};
use starnix_uapi::user_value::UserValue;
use starnix_uapi::vfs::{EpollEvent, FdEvents, ResolveFlags};
use starnix_uapi::{
    __kernel_fd_set, aio_context_t, errno, error, f_owner_ex, io_event, io_uring_params,
    io_uring_register_op_IORING_REGISTER_BUFFERS as IORING_REGISTER_BUFFERS,
    io_uring_register_op_IORING_UNREGISTER_BUFFERS as IORING_UNREGISTER_BUFFERS, iocb, itimerspec,
    off_t, pid_t, pollfd, pselect6_sigmask, sigset_t, statfs, statx, timespec, uapi, uid_t,
    AT_EACCESS, AT_EMPTY_PATH, AT_NO_AUTOMOUNT, AT_REMOVEDIR, AT_SYMLINK_FOLLOW,
    AT_SYMLINK_NOFOLLOW, CLOCK_BOOTTIME, CLOCK_BOOTTIME_ALARM, CLOCK_MONOTONIC, CLOCK_REALTIME,
    CLOCK_REALTIME_ALARM, CLOSE_RANGE_CLOEXEC, CLOSE_RANGE_UNSHARE, EFD_CLOEXEC, EFD_NONBLOCK,
    EFD_SEMAPHORE, EPOLL_CLOEXEC, EPOLL_CTL_ADD, EPOLL_CTL_DEL, EPOLL_CTL_MOD, FIOCLEX, FIONCLEX,
    F_ADD_SEALS, F_DUPFD, F_DUPFD_CLOEXEC, F_GETFD, F_GETFL, F_GETLEASE, F_GETLK, F_GETOWN,
    F_GETOWN_EX, F_GET_SEALS, F_OFD_GETLK, F_OFD_SETLK, F_OFD_SETLKW, F_OWNER_PGRP, F_OWNER_PID,
    F_OWNER_TID, F_SETFD, F_SETFL, F_SETLEASE, F_SETLK, F_SETLKW, F_SETOWN, F_SETOWN_EX,
    IN_CLOEXEC, IN_NONBLOCK, IORING_SETUP_CQSIZE, MFD_ALLOW_SEALING, MFD_CLOEXEC, MFD_HUGETLB,
    MFD_HUGE_MASK, MFD_HUGE_SHIFT, MFD_NOEXEC_SEAL, NAME_MAX, O_CLOEXEC, O_CREAT, O_NOFOLLOW,
    O_PATH, O_TMPFILE, PATH_MAX, PIDFD_NONBLOCK, POLLERR, POLLHUP, POLLIN, POLLOUT, POLLPRI,
    POLLRDBAND, POLLRDNORM, POLLWRBAND, POLLWRNORM, POSIX_FADV_DONTNEED, POSIX_FADV_NOREUSE,
    POSIX_FADV_NORMAL, POSIX_FADV_RANDOM, POSIX_FADV_SEQUENTIAL, POSIX_FADV_WILLNEED,
    RWF_SUPPORTED, TFD_CLOEXEC, TFD_NONBLOCK, TFD_TIMER_ABSTIME, TFD_TIMER_CANCEL_ON_SET,
    XATTR_CREATE, XATTR_NAME_MAX, XATTR_REPLACE,
};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::{atomic, Arc};
use std::usize;

// Constants from bionic/libc/include/sys/stat.h
const UTIME_NOW: i64 = 0x3fffffff;
const UTIME_OMIT: i64 = 0x3ffffffe;

pub fn sys_read(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    address: UserAddress,
    length: usize,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    file.read(
        locked,
        current_task,
        &mut UserBuffersOutputBuffer::unified_new_at(current_task, address, length)?,
    )
    .map_eintr(|| errno!(ERESTARTSYS))
}

pub fn sys_write(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    address: UserAddress,
    length: usize,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    file.write(
        locked,
        current_task,
        &mut UserBuffersInputBuffer::unified_new_at(current_task, address, length)?,
    )
    .map_eintr(|| errno!(ERESTARTSYS))
}

pub fn sys_close(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
) -> Result<(), Errno> {
    current_task.files.close(fd)?;
    Ok(())
}

pub fn sys_close_range(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    first: u32,
    last: u32,
    flags: u32,
) -> Result<(), Errno> {
    if first > last || flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 {
        return error!(EINVAL);
    }
    if flags & CLOSE_RANGE_UNSHARE != 0 {
        current_task.files.unshare();
    }
    let in_range = |fd: FdNumber| fd.raw() as u32 >= first && fd.raw() as u32 <= last;
    if flags & CLOSE_RANGE_CLOEXEC != 0 {
        current_task.files.retain(|fd, flags| {
            if in_range(fd) {
                *flags |= FdFlags::CLOEXEC;
            }
            true
        });
    } else {
        current_task.files.retain(|fd, _| !in_range(fd));
    }
    Ok(())
}

pub fn sys_lseek(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    offset: off_t,
    whence: u32,
) -> Result<off_t, Errno> {
    let file = current_task.files.get(fd)?;
    file.seek(locked, current_task, SeekTarget::from_raw(whence, offset)?)
}

pub fn sys_fcntl(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    cmd: u32,
    arg: u64,
) -> Result<SyscallResult, Errno> {
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let fd_number = arg as i32;
            let flags = if cmd == F_DUPFD_CLOEXEC { FdFlags::CLOEXEC } else { FdFlags::empty() };
            let newfd = current_task.files.duplicate(
                current_task,
                fd,
                TargetFdNumber::Minimum(FdNumber::from_raw(fd_number)),
                flags,
            )?;
            Ok(newfd.into())
        }
        F_GETOWN => {
            let file = current_task.files.get(fd)?;
            security::check_file_fcntl_access(current_task, &file, cmd, arg)?;
            match file.get_async_owner() {
                FileAsyncOwner::Unowned => Ok(0.into()),
                FileAsyncOwner::Thread(tid) => Ok(tid.into()),
                FileAsyncOwner::Process(pid) => Ok(pid.into()),
                FileAsyncOwner::ProcessGroup(pgid) => Ok((-pgid).into()),
            }
        }
        F_GETOWN_EX => {
            let file = current_task.files.get(fd)?;
            let maybe_owner = match file.get_async_owner() {
                FileAsyncOwner::Unowned => None,
                FileAsyncOwner::Thread(tid) => {
                    Some(uapi::f_owner_ex { type_: F_OWNER_TID as i32, pid: tid })
                }
                FileAsyncOwner::Process(pid) => {
                    Some(uapi::f_owner_ex { type_: F_OWNER_PID as i32, pid })
                }
                FileAsyncOwner::ProcessGroup(pgid) => {
                    Some(uapi::f_owner_ex { type_: F_OWNER_PGRP as i32, pid: pgid })
                }
            };
            if let Some(owner) = maybe_owner {
                let user_owner: UserRef<f_owner_ex> =
                    UserRef::<uapi::f_owner_ex>::new(UserAddress::from(arg));
                current_task.write_object(user_owner, &owner)?;
            }
            Ok(SUCCESS)
        }
        F_SETOWN => {
            let file = current_task.files.get(fd)?;
            let pid = (arg as u32) as i32;
            let owner = match pid.cmp(&0) {
                Ordering::Equal => FileAsyncOwner::Unowned,
                Ordering::Greater => FileAsyncOwner::Process(pid),
                Ordering::Less => {
                    FileAsyncOwner::ProcessGroup(pid.checked_neg().ok_or_else(|| errno!(EINVAL))?)
                }
            };
            owner.validate(current_task)?;
            security::check_file_fcntl_access(current_task, &file, cmd, arg)?;
            file.set_async_owner(owner);
            Ok(SUCCESS)
        }
        F_SETOWN_EX => {
            let file = current_task.files.get(fd)?;
            let user_owner = UserRef::<uapi::f_owner_ex>::new(UserAddress::from(arg));
            let requested_owner = current_task.read_object(user_owner)?;
            let mut owner = match requested_owner.type_ as u32 {
                F_OWNER_TID => FileAsyncOwner::Thread(requested_owner.pid),
                F_OWNER_PID => FileAsyncOwner::Process(requested_owner.pid),
                F_OWNER_PGRP => FileAsyncOwner::ProcessGroup(requested_owner.pid),
                _ => return error!(EINVAL),
            };
            if requested_owner.pid == 0 {
                owner = FileAsyncOwner::Unowned;
            }
            owner.validate(current_task)?;
            file.set_async_owner(owner);
            Ok(SUCCESS)
        }
        F_GETFD => Ok(current_task.files.get_fd_flags_allowing_opath(fd)?.into()),
        F_SETFD => {
            current_task
                .files
                .set_fd_flags_allowing_opath(fd, FdFlags::from_bits_truncate(arg as u32))?;
            Ok(SUCCESS)
        }
        F_GETFL => {
            // O_PATH allowed for:
            //
            //   Retrieving open file status flags using the fcntl(2)
            //   F_GETFL operation: the returned flags will include the
            //   bit O_PATH.
            //
            // See https://man7.org/linux/man-pages/man2/open.2.html
            let file = current_task.files.get_allowing_opath(fd)?;
            security::check_file_fcntl_access(current_task, &file, cmd, arg)?;
            Ok(file.flags().into())
        }
        F_SETFL => {
            let settable_flags = OpenFlags::APPEND
                | OpenFlags::DIRECT
                | OpenFlags::NOATIME
                | OpenFlags::NONBLOCK
                | OpenFlags::ASYNC;
            let requested_flags =
                OpenFlags::from_bits_truncate((arg as u32) & settable_flags.bits());
            let file = current_task.files.get(fd)?;
            security::check_file_fcntl_access(current_task, &file, cmd, arg)?;

            // If `NOATIME` flag is being set then check that it's allowed.
            if requested_flags.contains(OpenFlags::NOATIME)
                && !file.flags().contains(OpenFlags::NOATIME)
            {
                file.name.check_o_noatime_allowed(current_task)?;
            }

            file.update_file_flags(requested_flags, settable_flags);
            Ok(SUCCESS)
        }
        F_SETLK | F_SETLKW | F_GETLK | F_OFD_GETLK | F_OFD_SETLK | F_OFD_SETLKW => {
            let file = current_task.files.get(fd)?;
            security::check_file_fcntl_access(current_task, &file, cmd, arg)?;
            let flock_ref = UserRef::<uapi::flock>::new(arg.into());
            let flock = current_task.read_object(flock_ref)?;
            let cmd = RecordLockCommand::from_raw(cmd).ok_or_else(|| errno!(EINVAL))?;
            if let Some(flock) = file.record_lock(locked, current_task, cmd, flock)? {
                current_task.write_object(flock_ref, &flock)?;
            }
            Ok(SUCCESS)
        }
        F_ADD_SEALS => {
            let file = current_task.files.get(fd)?;

            if !file.can_write() {
                // Cannot add seals if the file is not writable
                return error!(EPERM);
            }

            let mut state = file.name.entry.node.write_guard_state.lock();
            let flags = SealFlags::from_bits_truncate(arg as u32);
            state.try_add_seal(flags)?;
            Ok(SUCCESS)
        }
        F_GET_SEALS => {
            let file = current_task.files.get(fd)?;
            let state = file.name.entry.node.write_guard_state.lock();
            Ok(state.get_seals()?.into())
        }
        F_SETLEASE => {
            let file = current_task.files.get(fd)?;

            let creds = current_task.creds();
            if !creds.has_capability(CAP_LEASE) && creds.fsuid != file.node().info().uid {
                return error!(EPERM);
            }
            let lease = FileLeaseType::from_bits(arg as u32)?;
            file.set_lease(current_task, lease)?;
            Ok(SUCCESS)
        }
        F_GETLEASE => {
            let file = current_task.files.get(fd)?;
            Ok(file.get_lease(current_task).into())
        }
        _ => {
            let file = current_task.files.get(fd)?;
            file.fcntl(current_task, cmd, arg)
        }
    }
}

pub fn sys_pread64(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    address: UserAddress,
    length: usize,
    offset: off_t,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    let offset = offset.try_into().map_err(|_| errno!(EINVAL))?;
    file.read_at(
        locked,
        current_task,
        offset,
        &mut UserBuffersOutputBuffer::unified_new_at(current_task, address, length)?,
    )
}

pub fn sys_pwrite64(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    address: UserAddress,
    length: usize,
    offset: off_t,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    let offset = offset.try_into().map_err(|_| errno!(EINVAL))?;
    file.write_at(
        locked,
        current_task,
        offset,
        &mut UserBuffersInputBuffer::unified_new_at(current_task, address, length)?,
    )
}

fn do_readv(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
    offset: Option<off_t>,
    flags: u32,
) -> Result<usize, Errno> {
    if flags & !RWF_SUPPORTED != 0 {
        return error!(EOPNOTSUPP);
    }
    if flags != 0 {
        track_stub!(TODO("https://fxbug.dev/322875072"), "preadv2 flags", flags);
    }
    let file = current_task.files.get(fd)?;
    let iovec = current_task.read_iovec(iovec_addr, iovec_count)?;
    let mut data = UserBuffersOutputBuffer::unified_new(current_task, iovec)?;
    if let Some(offset) = offset {
        file.read_at(
            locked,
            current_task,
            offset.try_into().map_err(|_| errno!(EINVAL))?,
            &mut data,
        )
    } else {
        file.read(locked, current_task, &mut data)
    }
}

pub fn sys_readv(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
) -> Result<usize, Errno> {
    do_readv(locked, current_task, fd, iovec_addr, iovec_count, None, 0)
}

pub fn sys_preadv(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
    offset: off_t,
) -> Result<usize, Errno> {
    do_readv(locked, current_task, fd, iovec_addr, iovec_count, Some(offset), 0)
}

pub fn sys_preadv2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
    offset: off_t,
    _unused: SyscallArg, // On 32-bit systems, holds the upper 32 bits of offset.
    flags: u32,
) -> Result<usize, Errno> {
    let offset = if offset == -1 { None } else { Some(offset) };
    do_readv(locked, current_task, fd, iovec_addr, iovec_count, offset, flags)
}

fn do_writev(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
    offset: Option<off_t>,
    flags: u32,
) -> Result<usize, Errno> {
    if flags & !RWF_SUPPORTED != 0 {
        return error!(EOPNOTSUPP);
    }
    if flags != 0 {
        track_stub!(TODO("https://fxbug.dev/322874523"), "pwritev2 flags", flags);
    }

    let file = current_task.files.get(fd)?;
    // Try to minimize how far arch32 impacts the system
    let iovec = if current_task.thread_state.arch_width.is_arch32() {
        current_task.read_iovec32(iovec_addr, iovec_count)?
    } else {
        current_task.read_iovec(iovec_addr, iovec_count)?
    };
    let mut data = UserBuffersInputBuffer::unified_new(current_task, iovec)?;
    let res = if let Some(offset) = offset {
        file.write_at(
            locked,
            current_task,
            offset.try_into().map_err(|_| errno!(EINVAL))?,
            &mut data,
        )
    } else {
        file.write(locked, current_task, &mut data)
    };

    match &res {
        Err(e) if e.code == EFAULT => {
            track_stub!(TODO("https://fxbug.dev/297370529"), "allow partial writes")
        }
        _ => (),
    }

    res
}

pub fn sys_writev(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
) -> Result<usize, Errno> {
    do_writev(locked, current_task, fd, iovec_addr, iovec_count, None, 0)
}

pub fn sys_pwritev(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
    offset: off_t,
) -> Result<usize, Errno> {
    do_writev(locked, current_task, fd, iovec_addr, iovec_count, Some(offset), 0)
}

pub fn sys_pwritev2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
    offset: off_t,
    _unused: SyscallArg, // On 32-bit systems, holds the upper 32 bits of offset.
    flags: u32,
) -> Result<usize, Errno> {
    let offset = if offset == -1 { None } else { Some(offset) };
    do_writev(locked, current_task, fd, iovec_addr, iovec_count, offset, flags)
}

pub fn sys_fstatfs(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    user_buf: UserRef<statfs>,
) -> Result<(), Errno> {
    // O_PATH allowed for:
    //
    //   fstatfs(2) (since Linux 3.12).
    //
    // See https://man7.org/linux/man-pages/man2/open.2.html
    let file = current_task.files.get_allowing_opath(fd)?;
    let mut stat = file.fs.statfs(locked, current_task)?;
    stat.f_flags |= file.name.mount.flags().bits() as i64;
    current_task.write_object(user_buf, &stat)?;
    Ok(())
}

pub fn sys_statfs(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    user_buf: UserRef<statfs>,
) -> Result<(), Errno> {
    let name =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, user_path, LookupFlags::default())?;
    let fs = name.entry.node.fs();
    let mut stat = fs.statfs(locked, current_task)?;
    stat.f_flags |= name.mount.flags().bits() as i64;
    current_task.write_object(user_buf, &stat)?;

    Ok(())
}

pub fn sys_sendfile(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    out_fd: FdNumber,
    in_fd: FdNumber,
    user_offset: UserRef<off_t>,
    count: i32,
) -> Result<usize, Errno> {
    splice::sendfile(locked, current_task, out_fd, in_fd, user_offset, count)
}

/// A convenient wrapper for Task::open_file_at.
///
/// Reads user_path from user memory and then calls through to Task::open_file_at.
fn open_file_at(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    flags: u32,
    mode: FileMode,
    resolve_flags: ResolveFlags,
) -> Result<FileHandle, Errno> {
    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;
    log_trace!(dir_fd:%, path:%; "open_file_at");
    current_task.open_file_at(
        locked,
        dir_fd,
        path.as_ref(),
        OpenFlags::from_bits_truncate(flags),
        mode,
        resolve_flags,
        AccessCheck::default(),
    )
}

fn lookup_parent_at<T, F>(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    callback: F,
) -> Result<T, Errno>
where
    F: Fn(&mut Locked<'_, Unlocked>, LookupContext, NamespaceNode, &FsStr) -> Result<T, Errno>,
{
    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;
    log_trace!(dir_fd:%, path:%; "lookup_parent_at");
    if path.is_empty() {
        return error!(ENOENT);
    }
    let mut context = LookupContext::default();
    let (parent, basename) =
        current_task.lookup_parent_at(locked, &mut context, dir_fd, path.as_ref())?;
    callback(locked, context, parent, basename)
}

/// Options for lookup_at.
#[derive(Debug, Default, Copy, Clone)]
pub struct LookupFlags {
    /// Whether AT_EMPTY_PATH was supplied.
    allow_empty_path: bool,

    /// Used to implement AT_SYMLINK_NOFOLLOW.
    symlink_mode: SymlinkMode,

    /// Automount directories on the path.
    // TODO(https://fxbug.dev/297370602): Support the `AT_NO_AUTOMOUNT` flag.
    #[allow(dead_code)]
    automount: bool,
}

impl LookupFlags {
    fn no_follow() -> Self {
        Self { symlink_mode: SymlinkMode::NoFollow, ..Default::default() }
    }

    fn from_bits(flags: u32, allowed_flags: u32) -> Result<Self, Errno> {
        if flags & !allowed_flags != 0 {
            return error!(EINVAL);
        }
        let follow_symlinks = if allowed_flags & AT_SYMLINK_FOLLOW != 0 {
            flags & AT_SYMLINK_FOLLOW != 0
        } else {
            flags & AT_SYMLINK_NOFOLLOW == 0
        };
        let automount =
            if allowed_flags & AT_NO_AUTOMOUNT != 0 { flags & AT_NO_AUTOMOUNT == 0 } else { false };
        if automount {
            track_stub!(TODO("https://fxbug.dev/297370602"), "LookupFlags::automount");
        }
        Ok(LookupFlags {
            allow_empty_path: (flags & AT_EMPTY_PATH != 0)
                || (flags & O_PATH != 0 && flags & O_NOFOLLOW != 0),
            symlink_mode: if follow_symlinks { SymlinkMode::Follow } else { SymlinkMode::NoFollow },
            automount,
        })
    }
}

impl From<StatxFlags> for LookupFlags {
    fn from(flags: StatxFlags) -> Self {
        let lookup_flags = StatxFlags::AT_SYMLINK_NOFOLLOW
            | StatxFlags::AT_EMPTY_PATH
            | StatxFlags::AT_NO_AUTOMOUNT;
        Self::from_bits((flags & lookup_flags).bits(), lookup_flags.bits()).unwrap()
    }
}

pub fn lookup_at<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    options: LookupFlags,
) -> Result<NamespaceNode, Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;
    log_trace!(dir_fd:%, path:%; "lookup_at");
    if path.is_empty() {
        if options.allow_empty_path {
            let (node, _) = current_task.resolve_dir_fd(
                locked,
                dir_fd,
                path.as_ref(),
                ResolveFlags::empty(),
            )?;
            return Ok(node);
        }
        return error!(ENOENT);
    }

    let mut parent_context = LookupContext::default();
    let (parent, basename) =
        current_task.lookup_parent_at(locked, &mut parent_context, dir_fd, path.as_ref())?;

    let mut child_context = if parent_context.must_be_directory {
        // The child must resolve to a directory. This is because a trailing slash
        // was found in the path. If the child is a symlink, we should follow it.
        // See https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
        parent_context.with(SymlinkMode::Follow)
    } else {
        parent_context.with(options.symlink_mode)
    };

    parent.lookup_child(locked, current_task, &mut child_context, basename)
}

fn do_openat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    flags: u32,
    mode: FileMode,
    resolve_flags: ResolveFlags,
) -> Result<FdNumber, Errno> {
    let file = open_file_at(locked, current_task, dir_fd, user_path, flags, mode, resolve_flags)?;
    let fd_flags = get_fd_flags(flags);
    current_task.add_file(file, fd_flags)
}

pub fn sys_openat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    flags: u32,
    mode: FileMode,
) -> Result<FdNumber, Errno> {
    do_openat(locked, current_task, dir_fd, user_path, flags, mode, ResolveFlags::empty())
}

pub fn sys_openat2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    how_ref: UserRef<uapi::open_how>,
    size: usize,
) -> Result<FdNumber, Errno> {
    const EXPECTED_SIZE: usize = std::mem::size_of::<uapi::open_how>();
    if size < EXPECTED_SIZE {
        return error!(EINVAL);
    }

    let how = current_task.read_object(how_ref)?;

    // If the `size` is greater than expected, then we need to check that any extra bytes after
    // `open_how` are set to 0. This is needed to properly handle the case when `open_how` is
    // extended with new fields in the future. There is no upper limit on the buffer size, so we
    // limit size of each read to one page.
    let mut pos = EXPECTED_SIZE;
    while pos < size {
        let length = std::cmp::min(size - pos, *PAGE_SIZE as usize);
        let extra_bytes =
            current_task.read_buffer(&UserBuffer { address: how_ref.addr() + pos, length })?;
        for b in extra_bytes {
            if b != 0 {
                return error!(E2BIG);
            }
        }
        pos += length;
    }

    let flags: u32 = how.flags.try_into().map_err(|_| errno!(EINVAL))?;

    // `mode` can be specified only with `O_CREAT` or `O_TMPFILE`.
    let allowed_mode_flags = if (flags & (O_CREAT | O_TMPFILE)) > 0 { 0o7777 } else { 0 };
    if (how.mode & !allowed_mode_flags) != 0 {
        return error!(EINVAL);
    }

    let mode = FileMode::from_bits(how.mode.try_into().map_err(|_| errno!(EINVAL))?);
    let resolve_flags =
        ResolveFlags::from_bits(how.resolve.try_into().map_err(|_| errno!(EINVAL))?)
            .ok_or_else(|| errno!(EINVAL))?;

    if resolve_flags.contains(ResolveFlags::CACHED) {
        track_stub!(TODO("https://fxbug.dev/326474574"), "openat2: RESOLVE_CACHED");
        return error!(EAGAIN);
    }

    do_openat(locked, current_task, dir_fd, user_path, flags, mode, resolve_flags)
}

pub fn sys_faccessat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    mode: u32,
) -> Result<(), Errno> {
    sys_faccessat2(locked, current_task, dir_fd, user_path, mode, 0)
}

pub fn sys_faccessat2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    mode: u32,
    flags: u32,
) -> Result<(), Errno> {
    let mode = Access::from_bits(mode).ok_or_else(|| errno!(EINVAL))?;
    let lookup_flags = LookupFlags::from_bits(flags, AT_SYMLINK_NOFOLLOW | AT_EACCESS)?;
    let name = lookup_at(locked, current_task, dir_fd, user_path, lookup_flags)?;
    name.check_access(locked, current_task, mode, CheckAccessReason::Access)
}

pub fn sys_getdents64(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    user_buffer: UserAddress,
    user_capacity: usize,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    let mut offset = file.offset.lock();
    let mut sink = DirentSink64::new(current_task, &mut offset, user_buffer, user_capacity);
    let result = file.readdir(locked, current_task, &mut sink);
    sink.map_result_with_actual(result)
}

pub fn sys_chroot(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
) -> Result<(), Errno> {
    let name =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, user_path, LookupFlags::default())?;
    if !name.entry.node.is_dir() {
        return error!(ENOTDIR);
    }

    current_task.fs().chroot(locked, current_task, name)?;
    Ok(())
}

pub fn sys_chdir(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
) -> Result<(), Errno> {
    let name =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, user_path, LookupFlags::default())?;
    if !name.entry.node.is_dir() {
        return error!(ENOTDIR);
    }
    current_task.fs().chdir(locked, current_task, name)
}

pub fn sys_fchdir(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
) -> Result<(), Errno> {
    // O_PATH allowed for:
    //
    //   fchdir(2), if the file descriptor refers to a directory
    //   (since Linux 3.5).
    //
    // See https://man7.org/linux/man-pages/man2/open.2.html
    let file = current_task.files.get_allowing_opath(fd)?;
    if !file.name.entry.node.is_dir() {
        return error!(ENOTDIR);
    }
    current_task.fs().chdir(locked, current_task, file.name.to_passive())
}

pub fn sys_fstat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    buffer: UserRef<uapi::stat>,
) -> Result<(), Errno> {
    // O_PATH allowed for:
    //
    //   fstat(2) (since Linux 3.6).
    //
    // See https://man7.org/linux/man-pages/man2/open.2.html
    let file = current_task.files.get_allowing_opath(fd)?;
    let result = file.node().stat(locked, current_task)?;
    current_task.write_object(buffer, &result)?;
    Ok(())
}

pub fn sys_newfstatat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    buffer: UserRef<uapi::stat>,
    flags: u32,
) -> Result<(), Errno> {
    let flags =
        LookupFlags::from_bits(flags, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT)?;
    let name = lookup_at(locked, current_task, dir_fd, user_path, flags)?;
    let result = name.entry.node.stat(locked, current_task)?;
    current_task.write_object(buffer, &result)?;
    Ok(())
}

pub fn sys_statx(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    flags: u32,
    mask: u32,
    statxbuf: UserRef<statx>,
) -> Result<(), Errno> {
    let flags = StatxFlags::from_bits(flags).ok_or_else(|| errno!(EINVAL))?;
    if flags & (StatxFlags::AT_STATX_FORCE_SYNC | StatxFlags::AT_STATX_DONT_SYNC)
        == (StatxFlags::AT_STATX_FORCE_SYNC | StatxFlags::AT_STATX_DONT_SYNC)
    {
        return error!(EINVAL);
    }

    let name = lookup_at(locked, current_task, dir_fd, user_path, LookupFlags::from(flags))?;
    let result = name.entry.node.statx(locked, current_task, flags, mask)?;
    current_task.write_object(statxbuf, &result)?;
    Ok(())
}

pub fn sys_readlinkat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    buffer: UserAddress,
    buffer_size: usize,
) -> Result<usize, Errno> {
    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;
    let lookup_flags = if path.is_empty() {
        if dir_fd == FdNumber::AT_FDCWD {
            return error!(ENOENT);
        }
        LookupFlags {
            allow_empty_path: true,
            symlink_mode: SymlinkMode::NoFollow,
            ..Default::default()
        }
    } else {
        LookupFlags::no_follow()
    };
    let name = lookup_at(locked, current_task, dir_fd, user_path, lookup_flags)?;

    let target = match name.readlink(locked, current_task)? {
        SymlinkTarget::Path(path) => path,
        SymlinkTarget::Node(node) => node.path(current_task),
    };

    if buffer_size == 0 {
        return error!(EINVAL);
    }
    // Cap the returned length at buffer_size.
    let length = std::cmp::min(buffer_size, target.len());
    current_task.write_memory(buffer, &target[..length])?;
    Ok(length)
}

pub fn sys_truncate(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    length: off_t,
) -> Result<(), Errno> {
    let length = length.try_into().map_err(|_| errno!(EINVAL))?;
    let name =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, user_path, LookupFlags::default())?;
    name.truncate(locked, current_task, length)?;
    Ok(())
}

pub fn sys_ftruncate(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    length: off_t,
) -> Result<(), Errno> {
    let length = length.try_into().map_err(|_| errno!(EINVAL))?;
    let file = current_task.files.get(fd)?;
    file.ftruncate(locked, current_task, length)?;
    Ok(())
}

pub fn sys_mkdirat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    mode: FileMode,
) -> Result<(), Errno> {
    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;

    if path.is_empty() {
        return error!(ENOENT);
    }
    let (parent, basename) = current_task.lookup_parent_at(
        locked,
        &mut LookupContext::default(),
        dir_fd,
        path.as_ref(),
    )?;
    parent.create_node(
        locked,
        current_task,
        basename,
        mode.with_type(FileMode::IFDIR),
        DeviceType::NONE,
    )?;
    Ok(())
}

pub fn sys_mknodat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    mode: FileMode,
    dev: DeviceType,
) -> Result<(), Errno> {
    let file_type = match mode.fmt() {
        FileMode::IFREG
        | FileMode::IFCHR
        | FileMode::IFBLK
        | FileMode::IFIFO
        | FileMode::IFSOCK => mode.fmt(),
        FileMode::EMPTY => FileMode::IFREG,
        _ => return error!(EINVAL),
    };
    lookup_parent_at(locked, current_task, dir_fd, user_path, |locked, _, parent, basename| {
        parent.create_node(locked, current_task, basename, mode.with_type(file_type), dev)
    })?;
    Ok(())
}

pub fn sys_linkat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    old_dir_fd: FdNumber,
    old_user_path: UserCString,
    new_dir_fd: FdNumber,
    new_user_path: UserCString,
    flags: u32,
) -> Result<(), Errno> {
    if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        track_stub!(TODO("https://fxbug.dev/322875706"), "linkat unknown flags", flags);
        return error!(EINVAL);
    }

    if flags & AT_EMPTY_PATH != 0 && !current_task.creds().has_capability(CAP_DAC_READ_SEARCH) {
        return error!(ENOENT);
    }

    let flags = LookupFlags::from_bits(flags, AT_EMPTY_PATH | AT_SYMLINK_FOLLOW)?;
    let target = lookup_at(locked, current_task, old_dir_fd, old_user_path, flags)?;
    lookup_parent_at(
        locked,
        current_task,
        new_dir_fd,
        new_user_path,
        |locked, context, parent, basename| {
            // The path to a new link cannot end in `/`. That would imply that we are dereferencing
            // the link to a directory.
            if context.must_be_directory {
                return error!(ENOENT);
            }
            if target.mount != parent.mount {
                return error!(EXDEV);
            }
            parent.link(locked, current_task, basename, &target.entry.node)
        },
    )?;

    Ok(())
}

pub fn sys_unlinkat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    flags: u32,
) -> Result<(), Errno> {
    if flags & !AT_REMOVEDIR != 0 {
        return error!(EINVAL);
    }
    let kind =
        if flags & AT_REMOVEDIR != 0 { UnlinkKind::Directory } else { UnlinkKind::NonDirectory };
    lookup_parent_at(
        locked,
        current_task,
        dir_fd,
        user_path,
        |locked, context, parent, basename| {
            parent.unlink(locked, current_task, basename, kind, context.must_be_directory)
        },
    )?;
    Ok(())
}

pub fn sys_renameat2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    old_dir_fd: FdNumber,
    old_user_path: UserCString,
    new_dir_fd: FdNumber,
    new_user_path: UserCString,
    flags: u32,
) -> Result<(), Errno> {
    let flags = RenameFlags::from_bits(flags).ok_or_else(|| errno!(EINVAL))?;
    if flags.intersects(RenameFlags::INTERNAL) {
        return error!(EINVAL);
    };

    // RENAME_EXCHANGE cannot be combined with the other flags.
    if flags.contains(RenameFlags::EXCHANGE)
        && flags.intersects(RenameFlags::NOREPLACE | RenameFlags::WHITEOUT)
    {
        return error!(EINVAL);
    }

    // RENAME_WHITEOUT is not supported.
    if flags.contains(RenameFlags::WHITEOUT) {
        track_stub!(TODO("https://fxbug.dev/322875416"), "RENAME_WHITEOUT");
        return error!(ENOSYS);
    };

    let mut lookup = |dir_fd, user_path| {
        lookup_parent_at(locked, current_task, dir_fd, user_path, |_, _, parent, basename| {
            Ok((parent, basename.to_owned()))
        })
    };

    let (old_parent, old_basename) = lookup(old_dir_fd, old_user_path)?;
    let (new_parent, new_basename) = lookup(new_dir_fd, new_user_path)?;

    if new_basename.len() > NAME_MAX as usize {
        return error!(ENAMETOOLONG);
    }

    NamespaceNode::rename(
        locked,
        current_task,
        &old_parent,
        old_basename.as_ref(),
        &new_parent,
        new_basename.as_ref(),
        flags,
    )
}

pub fn sys_fchmod(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    mode: FileMode,
) -> Result<(), Errno> {
    // Remove the filetype from the mode.
    let mode = mode & FileMode::PERMISSIONS;
    let file = current_task.files.get(fd)?;
    file.name.entry.node.chmod(locked, current_task, &file.name.mount, mode)?;
    file.name.entry.notify_ignoring_excl_unlink(InotifyMask::ATTRIB);
    Ok(())
}

pub fn sys_fchmodat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    mode: FileMode,
) -> Result<(), Errno> {
    // Remove the filetype from the mode.
    let mode = mode & FileMode::PERMISSIONS;
    let name = lookup_at(locked, current_task, dir_fd, user_path, LookupFlags::default())?;
    name.entry.node.chmod(locked, current_task, &name.mount, mode)?;
    name.entry.notify_ignoring_excl_unlink(InotifyMask::ATTRIB);
    Ok(())
}

fn maybe_uid(id: u32) -> Option<uid_t> {
    if id == u32::MAX {
        None
    } else {
        Some(id)
    }
}

pub fn sys_fchown(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    owner: u32,
    group: u32,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    file.name.entry.node.chown(
        locked,
        current_task,
        &file.name.mount,
        maybe_uid(owner),
        maybe_uid(group),
    )?;
    file.name.entry.notify_ignoring_excl_unlink(InotifyMask::ATTRIB);
    Ok(())
}

pub fn sys_fchownat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    owner: u32,
    group: u32,
    flags: u32,
) -> Result<(), Errno> {
    let flags = LookupFlags::from_bits(flags, AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW)?;
    let name = lookup_at(locked, current_task, dir_fd, user_path, flags)?;
    name.entry.node.chown(locked, current_task, &name.mount, maybe_uid(owner), maybe_uid(group))?;
    name.entry.notify_ignoring_excl_unlink(InotifyMask::ATTRIB);
    Ok(())
}

fn read_xattr_name(current_task: &CurrentTask, name_addr: UserCString) -> Result<FsString, Errno> {
    let name = current_task
        .read_c_string_to_vec(name_addr, XATTR_NAME_MAX as usize + 1)
        .map_err(|e| if e == ENAMETOOLONG { errno!(ERANGE) } else { e })?;
    if name.is_empty() {
        return error!(ERANGE);
    }
    let dot_index = memchr::memchr(b'.', &name).ok_or_else(|| errno!(ENOTSUP))?;
    if name[dot_index + 1..].is_empty() {
        return error!(EINVAL);
    }
    match &name[..dot_index] {
        b"user" | b"security" | b"trusted" | b"system" => {}
        _ => return error!(ENOTSUP),
    }
    Ok(name)
}

fn do_getxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    node: &NamespaceNode,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let name = read_xattr_name(current_task, name_addr)?;
    let value =
        match node.entry.node.get_xattr(locked, current_task, &node.mount, name.as_ref(), size)? {
            ValueOrSize::Size(s) => return Ok(s),
            ValueOrSize::Value(v) => v,
        };
    if size == 0 {
        return Ok(value.len());
    }
    if size < value.len() {
        return error!(ERANGE);
    }
    current_task.write_memory(value_addr, &value)
}

pub fn sys_getxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::default())?;
    do_getxattr(locked, current_task, &node, name_addr, value_addr, size)
}

pub fn sys_fgetxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    do_getxattr(locked, current_task, &file.name, name_addr, value_addr, size)
}

pub fn sys_lgetxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::no_follow())?;
    do_getxattr(locked, current_task, &node, name_addr, value_addr, size)
}

fn do_setxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    node: &NamespaceNode,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
    flags: u32,
) -> Result<(), Errno> {
    if size > XATTR_NAME_MAX as usize {
        return error!(E2BIG);
    }

    let op = match flags {
        0 => XattrOp::Set,
        XATTR_CREATE => XattrOp::Create,
        XATTR_REPLACE => XattrOp::Replace,
        _ => return error!(EINVAL),
    };
    let name = read_xattr_name(current_task, name_addr)?;
    let value = FsString::from(current_task.read_memory_to_vec(value_addr, size)?);
    node.entry.node.set_xattr(locked, current_task, &node.mount, name.as_ref(), value.as_ref(), op)
}

pub fn sys_fsetxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
    flags: u32,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    do_setxattr(locked, current_task, &file.name, name_addr, value_addr, size, flags)
}

pub fn sys_lsetxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
    flags: u32,
) -> Result<(), Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::no_follow())?;
    do_setxattr(locked, current_task, &node, name_addr, value_addr, size, flags)
}

pub fn sys_setxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    name_addr: UserCString,
    value_addr: UserAddress,
    size: usize,
    flags: u32,
) -> Result<(), Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::default())?;
    do_setxattr(locked, current_task, &node, name_addr, value_addr, size, flags)
}

fn do_removexattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    node: &NamespaceNode,
    name_addr: UserCString,
) -> Result<(), Errno> {
    let mode = node.entry.node.info().mode;
    if mode.is_chr() || mode.is_fifo() {
        return error!(EPERM);
    }
    let name = read_xattr_name(current_task, name_addr)?;
    node.entry.node.remove_xattr(locked, current_task, &node.mount, name.as_ref())
}

pub fn sys_removexattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    name_addr: UserCString,
) -> Result<(), Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::default())?;
    do_removexattr(locked, current_task, &node, name_addr)
}

pub fn sys_lremovexattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    name_addr: UserCString,
) -> Result<(), Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::no_follow())?;
    do_removexattr(locked, current_task, &node, name_addr)
}

pub fn sys_fremovexattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    name_addr: UserCString,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    do_removexattr(locked, current_task, &file.name, name_addr)
}

fn do_listxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    node: &NamespaceNode,
    list_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let security_xattr = security::fs_node_listsecurity(current_task, &node.entry.node);
    let xattrs = match node.entry.node.list_xattrs(locked, current_task, size) {
        Ok(ValueOrSize::Size(s)) => return Ok(s + security_xattr.map_or(0, |s| s.len() + 1)),
        Ok(ValueOrSize::Value(mut v)) => {
            if let Some(security_value) = security_xattr {
                if !v.contains(&security_value) {
                    v.push(security_value);
                }
            }
            v
        }
        Err(e) => {
            if e.code != ENOTSUP || security_xattr.is_none() {
                return Err(e);
            }
            vec![security_xattr.unwrap()]
        }
    };

    let mut list = vec![];
    for name in xattrs.iter() {
        list.extend_from_slice(name);
        list.push(b'\0');
    }
    if size == 0 {
        return Ok(list.len());
    }
    if size < list.len() {
        return error!(ERANGE);
    }
    current_task.write_memory(list_addr, &list)
}

pub fn sys_listxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    list_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::default())?;
    do_listxattr(locked, current_task, &node, list_addr, size)
}

pub fn sys_llistxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    path_addr: UserCString,
    list_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let node =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, path_addr, LookupFlags::no_follow())?;
    do_listxattr(locked, current_task, &node, list_addr, size)
}

pub fn sys_flistxattr(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    list_addr: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let file = current_task.files.get(fd)?;
    do_listxattr(locked, current_task, &file.name, list_addr, size)
}

pub fn sys_getcwd(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    buf: UserAddress,
    size: usize,
) -> Result<usize, Errno> {
    let root = current_task.fs().root();
    let cwd = current_task.fs().cwd();
    let mut user_cwd = match cwd.path_from_root(Some(&root)) {
        PathWithReachability::Reachable(path) => path,
        PathWithReachability::Unreachable(mut path) => {
            let mut combined = vec![];
            combined.extend_from_slice(b"(unreachable)");
            combined.append(&mut path);
            combined.into()
        }
    };
    user_cwd.push(b'\0');
    if user_cwd.len() > size {
        return error!(ERANGE);
    }
    current_task.write_memory(buf, &user_cwd)?;
    Ok(user_cwd.len())
}

pub fn sys_umask(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    umask: FileMode,
) -> Result<FileMode, Errno> {
    Ok(current_task.fs().set_umask(umask))
}

fn get_fd_flags(flags: u32) -> FdFlags {
    if flags & O_CLOEXEC != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    }
}

pub fn sys_pipe2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_pipe: UserRef<FdNumber>,
    flags: u32,
) -> Result<(), Errno> {
    let supported_file_flags = OpenFlags::NONBLOCK | OpenFlags::DIRECT;
    if flags & !(O_CLOEXEC | supported_file_flags.bits()) != 0 {
        return error!(EINVAL);
    }
    let (read, write) = new_pipe(locked, current_task)?;

    let file_flags = OpenFlags::from_bits_truncate(flags & supported_file_flags.bits());
    read.update_file_flags(file_flags, supported_file_flags);
    write.update_file_flags(file_flags, supported_file_flags);

    let fd_flags = get_fd_flags(flags);
    let fd_read = current_task.add_file(read, fd_flags)?;
    let fd_write = current_task.add_file(write, fd_flags)?;
    log_trace!("pipe2 -> [{:#x}, {:#x}]", fd_read.raw(), fd_write.raw());

    current_task.write_object(user_pipe, &fd_read)?;
    let user_pipe = user_pipe.next();
    current_task.write_object(user_pipe, &fd_write)?;

    Ok(())
}

pub fn sys_ioctl(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    request: u32,
    arg: SyscallArg,
) -> Result<SyscallResult, Errno> {
    match request {
        FIOCLEX => {
            current_task.files.set_fd_flags(fd, FdFlags::CLOEXEC)?;
            Ok(SUCCESS)
        }
        FIONCLEX => {
            current_task.files.set_fd_flags(fd, FdFlags::empty())?;
            Ok(SUCCESS)
        }
        _ => {
            let file = current_task.files.get(fd)?;
            security::check_file_ioctl_access(current_task, &file, request)?;
            file.ioctl(locked, current_task, request, arg)
        }
    }
}

pub fn sys_symlinkat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_target: UserCString,
    new_dir_fd: FdNumber,
    user_path: UserCString,
) -> Result<(), Errno> {
    let target = current_task.read_c_string_to_vec(user_target, PATH_MAX as usize)?;
    if target.is_empty() {
        return error!(ENOENT);
    }

    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;
    // TODO: This check could probably be moved into parent.symlink(..).
    if path.is_empty() {
        return error!(ENOENT);
    }

    let res = lookup_parent_at(
        locked,
        current_task,
        new_dir_fd,
        user_path,
        |locked, context, parent, basename| {
            // The path to a new symlink cannot end in `/`. That would imply that we are dereferencing
            // the symlink to a directory.
            //
            // See https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
            if context.must_be_directory {
                return error!(ENOENT);
            }
            parent.create_symlink(locked, current_task, basename, target.as_ref())
        },
    );
    res?;
    Ok(())
}

pub fn sys_dup(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    oldfd: FdNumber,
) -> Result<FdNumber, Errno> {
    current_task.files.duplicate(current_task, oldfd, TargetFdNumber::Default, FdFlags::empty())
}

pub fn sys_dup3(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    oldfd: FdNumber,
    newfd: FdNumber,
    flags: u32,
) -> Result<FdNumber, Errno> {
    if oldfd == newfd {
        return error!(EINVAL);
    }
    if flags & !O_CLOEXEC != 0 {
        return error!(EINVAL);
    }
    let fd_flags = get_fd_flags(flags);
    current_task.files.duplicate(current_task, oldfd, TargetFdNumber::Specific(newfd), fd_flags)?;
    Ok(newfd)
}

/// A memfd file descriptor cannot have a name longer than 250 bytes, including
/// the null terminator.
///
/// See Errors section of https://man7.org/linux/man-pages/man2/memfd_create.2.html
const MEMFD_NAME_MAX_LEN: usize = 250;

pub fn sys_memfd_create(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_name: UserCString,
    flags: u32,
) -> Result<FdNumber, Errno> {
    const HUGE_SHIFTED_MASK: u32 = MFD_HUGE_MASK << MFD_HUGE_SHIFT;

    if flags
        & !(MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_HUGETLB | HUGE_SHIFTED_MASK | MFD_NOEXEC_SEAL)
        != 0
    {
        return error!(EINVAL);
    }

    let _huge_page_size = if flags & MFD_HUGETLB != 0 {
        Some(flags & HUGE_SHIFTED_MASK)
    } else {
        if flags & HUGE_SHIFTED_MASK != 0 {
            return error!(EINVAL);
        }
        None
    };

    if flags & !MFD_NOEXEC_SEAL != 0 {
        track_stub!(TODO("https://fxbug.dev/322875665"), "memfd_create unknown flags", flags);
    }

    let name = current_task.read_c_string_to_vec(user_name, MEMFD_NAME_MAX_LEN).map_err(|e| {
        if e == ENAMETOOLONG {
            errno!(EINVAL)
        } else {
            e
        }
    })?;

    let seals = if flags & (MFD_ALLOW_SEALING | MFD_NOEXEC_SEAL) != 0 {
        SealFlags::empty()
    } else {
        // Forbid sealing, by sealing the seal operation.
        SealFlags::SEAL
    };

    let file = new_memfd(locked, current_task, name, seals, OpenFlags::RDWR)?;

    let mut fd_flags = FdFlags::empty();
    if flags & MFD_CLOEXEC != 0 {
        fd_flags |= FdFlags::CLOEXEC;
    }
    let fd = current_task.add_file(file, fd_flags)?;
    Ok(fd)
}

pub fn sys_mount(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    source_addr: UserCString,
    target_addr: UserCString,
    filesystemtype_addr: UserCString,
    flags: u32,
    data_addr: UserCString,
) -> Result<(), Errno> {
    if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
        return error!(EPERM);
    }

    let flags = MountFlags::from_bits(flags).ok_or_else(|| {
        track_stub!(
            TODO("https://fxbug.dev/322875327"),
            "mount unknown flags",
            flags & !MountFlags::from_bits_truncate(flags).bits()
        );
        errno!(EINVAL)
    })?;

    let target =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, target_addr, LookupFlags::default())?;

    security::sb_mount(current_task, &target, flags)?;

    if flags.contains(MountFlags::REMOUNT) {
        do_mount_remount(current_task, target, flags, data_addr)
    } else if flags.contains(MountFlags::BIND) {
        do_mount_bind(locked, current_task, source_addr, target, flags)
    } else if flags.intersects(MountFlags::SHARED | MountFlags::PRIVATE | MountFlags::DOWNSTREAM) {
        do_mount_change_propagation_type(current_task, target, flags)
    } else {
        do_mount_create(
            locked,
            current_task,
            source_addr,
            target,
            filesystemtype_addr,
            data_addr,
            flags,
        )
    }
}

fn do_mount_remount(
    current_task: &CurrentTask,
    target: NamespaceNode,
    flags: MountFlags,
    data_addr: UserCString,
) -> Result<(), Errno> {
    if !data_addr.is_null() {
        track_stub!(TODO("https://fxbug.dev/322875506"), "MS_REMOUNT: Updating data");
    }
    let mount = target.mount_if_root()?;

    let mut data_buf = [MaybeUninit::uninit(); PATH_MAX as usize];
    let data = current_task.read_c_string_if_non_null(data_addr, &mut data_buf)?;
    let mount_options =
        security::sb_eat_lsm_opts(current_task.kernel(), &mut MountParams::parse(data)?)?;
    security::sb_remount(current_task, &mount, mount_options)?;
    let updated_flags = flags & MountFlags::CHANGEABLE_WITH_REMOUNT;
    mount.update_flags(updated_flags);
    if !flags.contains(MountFlags::BIND) {
        // From <https://man7.org/linux/man-pages/man2/mount.2.html>
        //
        //   Since Linux 2.6.26, the MS_REMOUNT flag can be used with MS_BIND
        //   to modify only the per-mount-point flags.  This is particularly
        //   useful for setting or clearing the "read-only" flag on a mount
        //   without changing the underlying filesystem.
        track_stub!(TODO("https://fxbug.dev/322875215"), "MS_REMOUNT: Updating superblock flags");
    }
    Ok(())
}

fn do_mount_bind(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    source_addr: UserCString,
    target: NamespaceNode,
    flags: MountFlags,
) -> Result<(), Errno> {
    let source =
        lookup_at(locked, current_task, FdNumber::AT_FDCWD, source_addr, LookupFlags::default())?;
    log_trace!(
        source:% = source.path(current_task),
        target:% = target.path(current_task),
        flags:?;
        "do_mount_bind",
    );
    target.mount(WhatToMount::Bind(source), flags)
}

fn do_mount_change_propagation_type(
    current_task: &CurrentTask,
    target: NamespaceNode,
    flags: MountFlags,
) -> Result<(), Errno> {
    log_trace!(
        target:% = target.path(current_task),
        flags:?;
        "do_mount_change_propagation_type",
    );

    // Flag validation. Of the three propagation type flags, exactly one must be passed. The only
    // valid flags other than propagation type are MS_SILENT and MS_REC.
    //
    // Use if statements to find the first propagation type flag, then check for valid flags using
    // only the first propagation flag and MS_REC / MS_SILENT as valid flags.
    let propagation_flag = if flags.contains(MountFlags::SHARED) {
        MountFlags::SHARED
    } else if flags.contains(MountFlags::PRIVATE) {
        MountFlags::PRIVATE
    } else if flags.contains(MountFlags::DOWNSTREAM) {
        MountFlags::DOWNSTREAM
    } else {
        return error!(EINVAL);
    };
    if flags.intersects(!(propagation_flag | MountFlags::REC | MountFlags::SILENT)) {
        return error!(EINVAL);
    }

    let mount = target.mount_if_root()?;
    mount.change_propagation(propagation_flag, flags.contains(MountFlags::REC));
    Ok(())
}

fn do_mount_create(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    source_addr: UserCString,
    target: NamespaceNode,
    filesystemtype_addr: UserCString,
    data_addr: UserCString,
    flags: MountFlags,
) -> Result<(), Errno> {
    let mut source_buf = [MaybeUninit::uninit(); PATH_MAX as usize];
    let source = if source_addr.is_null() {
        Default::default()
    } else {
        current_task.read_c_string(source_addr, &mut source_buf)?
    };
    let mut fs_buf = [MaybeUninit::uninit(); PATH_MAX as usize];
    let fs_type = current_task.read_c_string(filesystemtype_addr, &mut fs_buf)?;
    let mut data_buf = [MaybeUninit::uninit(); PATH_MAX as usize];
    let data = current_task.read_c_string_if_non_null(data_addr, &mut data_buf)?;
    log_trace!(
        source:%,
        target:% = target.path(current_task),
        fs_type:%,
        data:%;
        "do_mount_create",
    );

    let options = FileSystemOptions {
        source: source.into(),
        flags: flags & MountFlags::STORED_ON_FILESYSTEM,
        params: MountParams::parse(data)?,
    };

    let fs = current_task.create_filesystem(locked, fs_type, options)?;

    security::sb_kern_mount(current_task, &fs)?;
    target.mount(WhatToMount::Fs(fs), flags)
}

pub fn sys_umount2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    target_addr: UserCString,
    flags: u32,
) -> Result<(), Errno> {
    if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
        return error!(EPERM);
    }

    let unmount_flags = UnmountFlags::from_bits(flags).ok_or_else(|| {
        track_stub!(
            TODO("https://fxbug.dev/322875327"),
            "unmount unknown flags",
            flags & !UnmountFlags::from_bits_truncate(flags).bits()
        );
        errno!(EINVAL)
    })?;

    if unmount_flags.contains(UnmountFlags::EXPIRE)
        && (unmount_flags.contains(UnmountFlags::FORCE)
            || unmount_flags.contains(UnmountFlags::DETACH))
    {
        return error!(EINVAL);
    }

    let lookup_flags = if unmount_flags.contains(UnmountFlags::NOFOLLOW) {
        LookupFlags::no_follow()
    } else {
        LookupFlags::default()
    };
    let target = lookup_at(locked, current_task, FdNumber::AT_FDCWD, target_addr, lookup_flags)?;

    security::sb_umount(current_task, &target, unmount_flags)?;

    target.unmount(unmount_flags)
}

pub fn sys_eventfd2(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    value: u32,
    flags: u32,
) -> Result<FdNumber, Errno> {
    if flags & !(EFD_CLOEXEC | EFD_NONBLOCK | EFD_SEMAPHORE) != 0 {
        return error!(EINVAL);
    }
    let blocking = (flags & EFD_NONBLOCK) == 0;
    let eventfd_type =
        if (flags & EFD_SEMAPHORE) == 0 { EventFdType::Counter } else { EventFdType::Semaphore };
    let file = new_eventfd(current_task, value, eventfd_type, blocking);
    let fd_flags = if flags & EFD_CLOEXEC != 0 { FdFlags::CLOEXEC } else { FdFlags::empty() };
    let fd = current_task.add_file(file, fd_flags)?;
    Ok(fd)
}

pub fn sys_pidfd_open(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    flags: u32,
) -> Result<FdNumber, Errno> {
    if flags & !PIDFD_NONBLOCK != 0 {
        return error!(EINVAL);
    }
    if pid <= 0 {
        return error!(EINVAL);
    }

    // Validate that the pid exists and that it belongs to a thread group leader.
    let task = current_task.get_task(pid);
    let task = Task::from_weak(&task)?;
    if !task.is_leader() {
        return error!(EINVAL);
    }

    let blocking = (flags & PIDFD_NONBLOCK) == 0;
    let open_flags = if blocking { OpenFlags::empty() } else { OpenFlags::NONBLOCK };
    let file = new_pidfd(current_task, &task.thread_group, open_flags);
    current_task.add_file(file, FdFlags::CLOEXEC)
}

pub fn sys_pidfd_getfd(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pidfd: FdNumber,
    targetfd: FdNumber,
    flags: u32,
) -> Result<FdNumber, Errno> {
    if flags != 0 {
        return error!(EINVAL);
    }

    let file = current_task.files.get(pidfd)?;
    let task = current_task.get_task(file.as_pid()?);
    let task = task.upgrade().ok_or_else(|| errno!(ESRCH))?;

    current_task.check_ptrace_access_mode(locked, PTRACE_MODE_ATTACH_REALCREDS, &task)?;

    let target_file = task.files.get(targetfd)?;
    current_task.add_file(target_file, FdFlags::CLOEXEC)
}

pub fn sys_timerfd_create(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    clock_id: u32,
    flags: u32,
) -> Result<FdNumber, Errno> {
    let timeline = match clock_id {
        CLOCK_MONOTONIC => Timeline::Monotonic,
        CLOCK_BOOTTIME | CLOCK_BOOTTIME_ALARM => Timeline::BootInstant,
        CLOCK_REALTIME | CLOCK_REALTIME_ALARM => Timeline::RealTime,
        _ => return error!(EINVAL),
    };
    let timer_type = match clock_id {
        CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_REALTIME => TimerWakeup::Regular,
        CLOCK_BOOTTIME_ALARM | CLOCK_REALTIME_ALARM => {
            if !current_task.creds().has_capability(CAP_WAKE_ALARM) {
                return error!(EPERM);
            }
            TimerWakeup::Alarm
        }
        _ => return error!(EINVAL),
    };
    if flags & !(TFD_NONBLOCK | TFD_CLOEXEC) != 0 {
        track_stub!(TODO("https://fxbug.dev/322875488"), "timerfd_create unknown flags", flags);
        return error!(EINVAL);
    }
    log_trace!("timerfd_create(clock_id={:?}, flags={:#x})", clock_id, flags);

    let mut open_flags = OpenFlags::RDWR;
    if flags & TFD_NONBLOCK != 0 {
        open_flags |= OpenFlags::NONBLOCK;
    }

    let mut fd_flags = FdFlags::empty();
    if flags & TFD_CLOEXEC != 0 {
        fd_flags |= FdFlags::CLOEXEC;
    };

    let timer = TimerFile::new_file(current_task, timer_type, timeline, open_flags)?;
    let fd = current_task.add_file(timer, fd_flags)?;
    Ok(fd)
}

pub fn sys_timerfd_gettime(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    user_current_value: UserRef<itimerspec>,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    let timer_file = file.downcast_file::<TimerFile>().ok_or_else(|| errno!(EINVAL))?;
    let timer_info = timer_file.current_timer_spec();
    log_trace!("timerfd_gettime(fd={:?}, current_value={:?})", fd, timer_info);
    current_task.write_object(user_current_value, &timer_info)?;
    Ok(())
}

pub fn sys_timerfd_settime(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    flags: u32,
    user_new_value: UserRef<itimerspec>,
    user_old_value: UserRef<itimerspec>,
) -> Result<(), Errno> {
    if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
        track_stub!(TODO("https://fxbug.dev/322874722"), "timerfd_settime unknown flags", flags);
        return error!(EINVAL);
    }

    if flags & TFD_TIMER_CANCEL_ON_SET != 0 {
        track_stub!(
            TODO("https://fxbug.dev/297433837"),
            "timerfd_settime: TFD_TIMER_CANCEL_ON_SET",
        );
    }

    let file = current_task.files.get(fd)?;
    let timer_file = file.downcast_file::<TimerFile>().ok_or_else(|| errno!(EINVAL))?;

    let new_timer_spec = current_task.read_object(user_new_value)?;
    let old_timer_spec = timer_file.set_timer_spec(current_task, &file, new_timer_spec, flags)?;
    log_trace!(
        "timerfd_settime(fd={:?}, flags={:#x}, new_value={:?}, current_value={:?})",
        fd,
        flags,
        new_timer_spec,
        old_timer_spec
    );
    if !user_old_value.is_null() {
        current_task.write_object(user_old_value, &old_timer_spec)?;
    }
    Ok(())
}

fn select(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    nfds: u32,
    readfds_addr: UserRef<__kernel_fd_set>,
    writefds_addr: UserRef<__kernel_fd_set>,
    exceptfds_addr: UserRef<__kernel_fd_set>,
    deadline: zx::MonotonicInstant,
    sigmask_addr: UserRef<pselect6_sigmask>,
) -> Result<i32, Errno> {
    const BITS_PER_BYTE: usize = 8;

    fn sizeof<T>(_: &T) -> usize {
        BITS_PER_BYTE * std::mem::size_of::<T>()
    }
    fn is_fd_set(set: &__kernel_fd_set, fd: usize) -> bool {
        let index = fd / sizeof(&set.fds_bits[0]);
        let remainder = fd % sizeof(&set.fds_bits[0]);
        set.fds_bits[index] & (1 << remainder) > 0
    }
    fn add_fd_to_set(set: &mut __kernel_fd_set, fd: usize) {
        let index = fd / sizeof(&set.fds_bits[0]);
        let remainder = fd % sizeof(&set.fds_bits[0]);

        set.fds_bits[index] |= 1 << remainder;
    }
    let read_fd_set = |addr: UserRef<__kernel_fd_set>| {
        if addr.is_null() {
            Ok(Default::default())
        } else {
            current_task.read_object(addr)
        }
    };

    if nfds as usize >= BITS_PER_BYTE * std::mem::size_of::<__kernel_fd_set>() {
        return error!(EINVAL);
    }

    let read_events = POLLRDNORM | POLLRDBAND | POLLIN | POLLHUP | POLLERR;
    let write_events = POLLWRBAND | POLLWRNORM | POLLOUT | POLLERR;
    let except_events = POLLPRI;

    let readfds = read_fd_set(readfds_addr)?;
    let writefds = read_fd_set(writefds_addr)?;
    let exceptfds = read_fd_set(exceptfds_addr)?;

    let sets = &[(read_events, &readfds), (write_events, &writefds), (except_events, &exceptfds)];
    let waiter = FileWaiter::<FdNumber>::default();

    for fd in 0..nfds {
        let mut aggregated_events = 0;
        for (events, fds) in sets.iter() {
            if is_fd_set(fds, fd as usize) {
                aggregated_events |= events;
            }
        }
        if aggregated_events != 0 {
            let fd = FdNumber::from_raw(fd as i32);
            let file = current_task.files.get(fd)?;
            waiter.add(
                locked,
                current_task,
                fd,
                Some(&file),
                FdEvents::from_bits_truncate(aggregated_events),
            )?;
        }
    }

    let mask = if !sigmask_addr.is_null() {
        let sigmask = current_task.read_object(sigmask_addr)?;
        let mask = if sigmask.ss.is_null() {
            current_task.read().signal_mask()
        } else {
            if sigmask.ss_len < std::mem::size_of::<sigset_t>() {
                return error!(EINVAL);
            }
            current_task.read_object(sigmask.ss.into())?
        };
        Some(mask)
    } else {
        None
    };

    waiter.wait(locked, current_task, mask, deadline)?;

    let mut num_fds = 0;
    let mut readfds: __kernel_fd_set = Default::default();
    let mut writefds: __kernel_fd_set = Default::default();
    let mut exceptfds: __kernel_fd_set = Default::default();
    let mut sets = [
        (read_events, &mut readfds),
        (write_events, &mut writefds),
        (except_events, &mut exceptfds),
    ];

    let mut ready_items = waiter.ready_items.lock();
    for ReadyItem { key: ready_key, events: ready_events } in ready_items.drain(..) {
        let ready_key = assert_matches::assert_matches!(
            ready_key,
            ReadyItemKey::FdNumber(v) => v
        );

        sets.iter_mut().for_each(|entry| {
            let events = FdEvents::from_bits_truncate(entry.0);
            let fds: &mut __kernel_fd_set = entry.1;
            if events.intersects(ready_events) {
                add_fd_to_set(fds, ready_key.raw() as usize);
                num_fds += 1;
            }
        });
    }

    let write_fd_set =
        |addr: UserRef<__kernel_fd_set>, value: __kernel_fd_set| -> Result<(), Errno> {
            if !addr.is_null() {
                current_task.write_object(addr, &value)?;
            }
            Ok(())
        };
    write_fd_set(readfds_addr, readfds)?;
    write_fd_set(writefds_addr, writefds)?;
    write_fd_set(exceptfds_addr, exceptfds)?;
    Ok(num_fds)
}

fn deadline_after_timespec(
    current_task: &CurrentTask,
    user_timespec: UserRef<timespec>,
) -> Result<zx::MonotonicInstant, Errno> {
    if user_timespec.is_null() {
        Ok(zx::MonotonicInstant::INFINITE)
    } else {
        let timespec = current_task.read_object(user_timespec)?;
        Ok(zx::MonotonicInstant::after(duration_from_timespec(timespec)?))
    }
}

pub fn sys_pselect6(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    nfds: u32,
    readfds_addr: UserRef<__kernel_fd_set>,
    writefds_addr: UserRef<__kernel_fd_set>,
    exceptfds_addr: UserRef<__kernel_fd_set>,
    timeout_addr: UserRef<timespec>,
    sigmask_addr: UserRef<pselect6_sigmask>,
) -> Result<i32, Errno> {
    let deadline = deadline_after_timespec(current_task, timeout_addr)?;

    let num_fds = select(
        locked,
        current_task,
        nfds,
        readfds_addr,
        writefds_addr,
        exceptfds_addr,
        deadline,
        sigmask_addr,
    )?;

    if !timeout_addr.is_null()
        && !current_task.thread_group.read().personality.contains(PersonalityFlags::STICKY_TIMEOUTS)
    {
        let now = zx::MonotonicInstant::get();
        let remaining = std::cmp::max(deadline - now, zx::MonotonicDuration::from_seconds(0));
        current_task.write_object(timeout_addr, &timespec_from_duration(remaining))?;
    }

    Ok(num_fds)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_select(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    nfds: u32,
    readfds_addr: UserRef<__kernel_fd_set>,
    writefds_addr: UserRef<__kernel_fd_set>,
    exceptfds_addr: UserRef<__kernel_fd_set>,
    timeout_addr: UserRef<starnix_uapi::timeval>,
) -> Result<i32, Errno> {
    let start_time = zx::MonotonicInstant::get();

    let deadline = if timeout_addr.is_null() {
        zx::MonotonicInstant::INFINITE
    } else {
        let timeval = current_task.read_object(timeout_addr)?;
        start_time + starnix_types::time::duration_from_timeval(timeval)?
    };

    let num_fds = select(
        locked,
        current_task,
        nfds,
        readfds_addr,
        writefds_addr,
        exceptfds_addr,
        deadline,
        UserRef::<pselect6_sigmask>::default(),
    )?;

    if !timeout_addr.is_null()
        && !current_task.thread_group.read().personality.contains(PersonalityFlags::STICKY_TIMEOUTS)
    {
        let now = zx::MonotonicInstant::get();
        let remaining = std::cmp::max(deadline - now, zx::MonotonicDuration::from_seconds(0));
        current_task
            .write_object(timeout_addr, &starnix_types::time::timeval_from_duration(remaining))?;
    }

    Ok(num_fds)
}

pub fn sys_epoll_create1(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    flags: u32,
) -> Result<FdNumber, Errno> {
    if flags & !EPOLL_CLOEXEC != 0 {
        return error!(EINVAL);
    }
    let ep_file = EpollFileObject::new_file(current_task);
    let fd_flags = if flags & EPOLL_CLOEXEC != 0 { FdFlags::CLOEXEC } else { FdFlags::empty() };
    let fd = current_task.add_file(ep_file, fd_flags)?;
    Ok(fd)
}

pub fn sys_epoll_ctl(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    epfd: FdNumber,
    op: u32,
    fd: FdNumber,
    event: UserRef<EpollEvent>,
) -> Result<(), Errno> {
    let file = current_task.files.get(epfd)?;
    let epoll_file = file.downcast_file::<EpollFileObject>().ok_or_else(|| errno!(EINVAL))?;
    let operand_file = current_task.files.get(fd)?;

    if Arc::ptr_eq(&file, &operand_file) {
        return error!(EINVAL);
    }

    let epoll_event = match current_task.read_object(event) {
        Ok(mut epoll_event) => {
            // If EPOLLWAKEUP is specified in flags, but the caller does not have the CAP_BLOCK_SUSPEND
            // capability, then the EPOLLWAKEUP flag is silently ignored.
            // See https://man7.org/linux/man-pages/man2/epoll_ctl.2.html
            if epoll_event.events().contains(FdEvents::EPOLLWAKEUP) {
                if !current_task.creds().has_capability(CAP_BLOCK_SUSPEND) {
                    epoll_event.ignore(FdEvents::EPOLLWAKEUP);
                }
            }
            Ok(epoll_event)
        }
        result => result,
    };

    match op {
        EPOLL_CTL_ADD => {
            epoll_file.add(locked, current_task, &operand_file, &file, epoll_event?)?;
            operand_file.register_epfd(epfd);
        }
        EPOLL_CTL_MOD => {
            epoll_file.modify(locked, current_task, &operand_file, epoll_event?)?;
        }
        EPOLL_CTL_DEL => {
            epoll_file.delete(&operand_file)?;
            current_task
                .kernel()
                .suspend_resume_manager
                .remove_epoll(operand_file.weak_handle.as_ptr() as EpollKey);
            operand_file.unregister_epfd(epfd);
        }
        _ => return error!(EINVAL),
    }
    Ok(())
}

// Backend for sys_epoll_pwait and sys_epoll_pwait2 that takes an already-decoded deadline.
fn do_epoll_pwait(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    epfd: FdNumber,
    events: UserRef<EpollEvent>,
    unvalidated_max_events: i32,
    deadline: zx::MonotonicInstant,
    user_sigmask: UserRef<SigSet>,
) -> Result<usize, Errno> {
    let file = current_task.files.get(epfd)?;
    let epoll_file = file.downcast_file::<EpollFileObject>().ok_or_else(|| errno!(EINVAL))?;

    // Max_events must be greater than 0.
    let max_events: usize = unvalidated_max_events.try_into().map_err(|_| errno!(EINVAL))?;
    if max_events == 0 {
        return error!(EINVAL);
    }

    // Return early if the user passes an obviously invalid pointer. This avoids dropping events
    // for common pointer errors. When we catch bad pointers after the wait is complete when the
    // memory is actually written, the events will be lost. This check is not a guarantee.
    current_task
        .mm()
        .ok_or_else(|| errno!(EINVAL))?
        .check_plausible(events.addr(), max_events * std::mem::size_of::<EpollEvent>())?;

    let active_events = if !user_sigmask.is_null() {
        let signal_mask = current_task.read_object(user_sigmask)?;
        current_task.wait_with_temporary_mask(locked, signal_mask, |locked, current_task| {
            epoll_file.wait(locked, current_task, max_events, deadline)
        })?
    } else {
        epoll_file.wait(locked, current_task, max_events, deadline)?
    };

    current_task.write_objects(events, &active_events)?;
    Ok(active_events.len())
}

pub fn sys_epoll_pwait(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    epfd: FdNumber,
    events: UserRef<EpollEvent>,
    max_events: i32,
    timeout: i32,
    user_sigmask: UserRef<SigSet>,
) -> Result<usize, Errno> {
    let deadline = zx::MonotonicInstant::after(duration_from_poll_timeout(timeout)?);
    do_epoll_pwait(locked, current_task, epfd, events, max_events, deadline, user_sigmask)
}

pub fn sys_epoll_pwait2(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    epfd: FdNumber,
    events: UserRef<EpollEvent>,
    max_events: i32,
    user_timespec: UserRef<timespec>,
    user_sigmask: UserRef<SigSet>,
) -> Result<usize, Errno> {
    let deadline = deadline_after_timespec(current_task, user_timespec)?;
    do_epoll_pwait(locked, current_task, epfd, events, max_events, deadline, user_sigmask)
}

struct FileWaiter<Key: Into<ReadyItemKey>> {
    waiter: Waiter,
    ready_items: Arc<Mutex<VecDeque<ReadyItem>>>,
    _marker: PhantomData<Key>,
}

impl<Key: Into<ReadyItemKey>> Default for FileWaiter<Key> {
    fn default() -> Self {
        Self { waiter: Waiter::new(), ready_items: Default::default(), _marker: PhantomData }
    }
}

impl<Key: Into<ReadyItemKey>> FileWaiter<Key> {
    fn add<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        key: Key,
        file: Option<&FileHandle>,
        requested_events: FdEvents,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let key = key.into();

        if let Some(file) = file {
            let sought_events = requested_events | FdEvents::POLLERR | FdEvents::POLLHUP;

            let handler = EventHandler::Enqueue(EnqueueEventHandler {
                key,
                queue: self.ready_items.clone(),
                sought_events,
                mappings: Default::default(),
            });
            file.wait_async(locked, current_task, &self.waiter, sought_events, handler);
            let current_events = file.query_events(locked, current_task)? & sought_events;
            if !current_events.is_empty() {
                self.ready_items.lock().push_back(ReadyItem { key, events: current_events });
            }
        } else {
            self.ready_items.lock().push_back(ReadyItem { key, events: FdEvents::POLLNVAL });
        }
        Ok(())
    }

    fn wait<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &mut CurrentTask,
        signal_mask: Option<SigSet>,
        deadline: zx::MonotonicInstant,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if self.ready_items.lock().is_empty() {
            // When wait_until() returns Ok() it means there was a wake up; however there may not
            // be a ready item, for example if waiting on a sync file with multiple sync points.
            // Keep waiting until there's at least one ready item.
            let signal_mask = signal_mask.unwrap_or_else(|| current_task.read().signal_mask());
            let mut result = current_task.wait_with_temporary_mask(
                locked,
                signal_mask,
                |locked, current_task| self.waiter.wait_until(locked, current_task, deadline),
            );
            loop {
                match result {
                    Err(err) if err == ETIMEDOUT => return Ok(()),
                    Ok(()) => {
                        if !self.ready_items.lock().is_empty() {
                            break;
                        }
                    }
                    result => result?,
                };
                result = self.waiter.wait_until(locked, current_task, deadline);
            }
        }
        Ok(())
    }
}

pub fn poll(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    user_pollfds: UserRef<pollfd>,
    num_fds: i32,
    mask: Option<SigSet>,
    deadline: zx::MonotonicInstant,
) -> Result<usize, Errno> {
    if num_fds < 0 || num_fds as u64 > current_task.thread_group.get_rlimit(Resource::NOFILE) {
        return error!(EINVAL);
    }

    let mut pollfds = vec![pollfd::default(); num_fds as usize];
    let waiter = FileWaiter::<usize>::default();

    for (index, poll_descriptor) in pollfds.iter_mut().enumerate() {
        *poll_descriptor = current_task.read_object(user_pollfds.at(index))?;
        poll_descriptor.revents = 0;
        if poll_descriptor.fd < 0 {
            continue;
        }
        let file = current_task.files.get(FdNumber::from_raw(poll_descriptor.fd)).ok();
        waiter.add(
            locked,
            current_task,
            index,
            file.as_ref(),
            FdEvents::from_bits_truncate(poll_descriptor.events as u32),
        )?;
    }

    waiter.wait(locked, current_task, mask, deadline)?;

    let mut ready_items = waiter.ready_items.lock();
    let mut unique_ready_items =
        bit_vec::BitVec::from_elem(usize::try_from(num_fds).unwrap(), false);
    for ReadyItem { key: ready_key, events: ready_events } in ready_items.drain(..) {
        let ready_key = assert_matches::assert_matches!(
            ready_key,
            ReadyItemKey::Usize(v) => v
        );
        let interested_events = FdEvents::from_bits_truncate(pollfds[ready_key].events as u32)
            | FdEvents::POLLERR
            | FdEvents::POLLHUP
            | FdEvents::POLLNVAL;
        let return_events = (interested_events & ready_events).bits();
        pollfds[ready_key].revents = return_events as i16;
        unique_ready_items.set(ready_key, true);
    }

    for (index, poll_descriptor) in pollfds.iter().enumerate() {
        current_task.write_object(user_pollfds.at(index), poll_descriptor)?;
    }

    Ok(unique_ready_items.into_iter().filter(Clone::clone).count())
}

pub fn sys_ppoll(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    user_fds: UserRef<pollfd>,
    num_fds: i32,
    user_timespec: UserRef<timespec>,
    user_mask: UserRef<SigSet>,
    sigset_size: usize,
) -> Result<usize, Errno> {
    let start_time = zx::MonotonicInstant::get();

    let timeout = if user_timespec.is_null() {
        // Passing -1 to poll is equivalent to an infinite timeout.
        -1
    } else {
        let ts = current_task.read_object(user_timespec)?;
        duration_from_timespec::<zx::MonotonicTimeline>(ts)?.into_millis() as i32
    };

    let deadline = start_time + duration_from_poll_timeout(timeout)?;

    let mask = if !user_mask.is_null() {
        if sigset_size != std::mem::size_of::<SigSet>() {
            return error!(EINVAL);
        }
        let mask = current_task.read_object(user_mask)?;
        Some(mask)
    } else {
        None
    };

    let poll_result = poll(locked, current_task, user_fds, num_fds, mask, deadline);

    if user_timespec.is_null() {
        return poll_result;
    }

    let now = zx::MonotonicInstant::get();
    let remaining = std::cmp::max(deadline - now, zx::MonotonicDuration::from_seconds(0));
    let remaining_timespec = timespec_from_duration(remaining);

    // From gVisor: "ppoll is normally restartable if interrupted by something other than a signal
    // handled by the application (i.e. returns ERESTARTNOHAND). However, if
    // [copy out] failed, then the restarted ppoll would use the wrong timeout, so the
    // error should be left as EINTR."
    match (current_task.write_object(user_timespec, &remaining_timespec), poll_result) {
        // If write was ok, and poll was ok, return poll result.
        (Ok(_), Ok(num_events)) => Ok(num_events),
        (Ok(_), Err(e)) if e == EINTR => {
            error!(ERESTARTNOHAND)
        }
        (Ok(_), poll_result) => poll_result,
        // If write was a failure, return the poll result unchanged.
        (Err(_), poll_result) => poll_result,
    }
}

pub fn sys_flock(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    operation: u32,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    let operation = FlockOperation::from_flags(operation)?;
    file.flock(locked, current_task, operation)
}

pub fn sys_sync(
    _locked: &mut Locked<'_, Unlocked>,
    _current_task: &CurrentTask,
) -> Result<(), Errno> {
    track_stub!(TODO("https://fxbug.dev/322875826"), "sync()");
    Ok(())
}

pub fn sys_syncfs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
) -> Result<(), Errno> {
    let _file = current_task.files.get(fd)?;
    track_stub!(TODO("https://fxbug.dev/322875646"), "syncfs");
    Ok(())
}

pub fn sys_fsync(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    file.sync(current_task)
}

pub fn sys_fdatasync(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    file.data_sync(current_task)
}

pub fn sys_sync_file_range(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    offset: off_t,
    length: off_t,
    flags: u32,
) -> Result<(), Errno> {
    const KNOWN_FLAGS: u32 = uapi::SYNC_FILE_RANGE_WAIT_BEFORE
        | uapi::SYNC_FILE_RANGE_WRITE
        | uapi::SYNC_FILE_RANGE_WAIT_AFTER;
    if flags & !KNOWN_FLAGS != 0 {
        return error!(EINVAL);
    }

    let file = current_task.files.get(fd)?;

    if offset < 0 || length < 0 {
        return error!(EINVAL);
    }

    checked_add_offset_and_length(offset as usize, length as usize)?;

    // From <https://linux.die.net/man/2/sync_file_range>:
    //
    //   fd refers to something other than a regular file, a block device, a directory, or a symbolic link.
    let mode = file.node().info().mode;
    if !mode.is_reg() && !mode.is_blk() && !mode.is_dir() && !mode.is_lnk() {
        return error!(ESPIPE);
    }

    if flags == 0 {
        return Ok(());
    }

    // Syncing the whole file is much more than we need for sync_file_range, which only needs to
    // sync the specified data range.
    file.data_sync(current_task)
}

pub fn sys_fadvise64(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    offset: off_t,
    len: off_t,
    advice: u32,
) -> Result<(), Errno> {
    match advice {
        POSIX_FADV_NORMAL
        | POSIX_FADV_RANDOM
        | POSIX_FADV_SEQUENTIAL
        | POSIX_FADV_WILLNEED
        | POSIX_FADV_DONTNEED
        | POSIX_FADV_NOREUSE => (),
        _ => {
            track_stub!(TODO("https://fxbug.dev/322875684"), "fadvise64 unknown advice", advice);
            return error!(EINVAL);
        }
    }

    if offset < 0 || len < 0 {
        return error!(EINVAL);
    }

    let file = current_task.files.get(fd)?;
    // fadvise does not work on pipes.
    if file.downcast_file::<PipeFileObject>().is_some() {
        return error!(ESPIPE);
    }

    // fadvise does not work on paths.
    if file.flags().contains(OpenFlags::PATH) {
        return error!(EBADF);
    }

    track_stub!(TODO("https://fxbug.dev/297434181"), "fadvise64");

    Ok(())
}

pub fn sys_fallocate(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    mode: u32,
    offset: off_t,
    len: off_t,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;

    // Offset must not be less than 0.
    // Length must not be less than or equal to 0.
    // See https://man7.org/linux/man-pages/man2/fallocate.2.html#ERRORS
    if offset < 0 || len <= 0 {
        return error!(EINVAL);
    }

    let mode = FallocMode::from_bits(mode).ok_or_else(|| errno!(EINVAL))?;
    file.fallocate(locked, current_task, mode, offset as u64, len as u64)?;

    Ok(())
}

pub fn sys_inotify_init1(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    flags: u32,
) -> Result<FdNumber, Errno> {
    if flags & !(IN_NONBLOCK | IN_CLOEXEC) != 0 {
        return error!(EINVAL);
    }
    let non_blocking = flags & IN_NONBLOCK != 0;
    let close_on_exec = flags & IN_CLOEXEC != 0;
    let inotify_file = InotifyFileObject::new_file(current_task, non_blocking);
    let fd_flags = if close_on_exec { FdFlags::CLOEXEC } else { FdFlags::empty() };
    current_task.add_file(inotify_file, fd_flags)
}

pub fn sys_inotify_add_watch(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    user_path: UserCString,
    mask: u32,
) -> Result<WdNumber, Errno> {
    let mask = InotifyMask::from_bits(mask).ok_or_else(|| errno!(EINVAL))?;
    if !mask.intersects(InotifyMask::ALL_EVENTS) {
        // Mask must include at least 1 event.
        return error!(EINVAL);
    }
    let file = current_task.files.get(fd)?;
    let inotify_file = file.downcast_file::<InotifyFileObject>().ok_or_else(|| errno!(EINVAL))?;
    let options = if mask.contains(InotifyMask::DONT_FOLLOW) {
        LookupFlags::no_follow()
    } else {
        LookupFlags::default()
    };
    let watched_node = lookup_at(locked, current_task, FdNumber::AT_FDCWD, user_path, options)?;
    if mask.contains(InotifyMask::ONLYDIR) && !watched_node.entry.node.is_dir() {
        return error!(ENOTDIR);
    }
    inotify_file.add_watch(watched_node.entry, mask, &file)
}

pub fn sys_inotify_rm_watch(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    watch_id: WdNumber,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    let inotify_file = file.downcast_file::<InotifyFileObject>().ok_or_else(|| errno!(EINVAL))?;
    inotify_file.remove_watch(watch_id, &file)
}

pub fn sys_utimensat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    user_times: UserRef<[timespec; 2]>,
    flags: u32,
) -> Result<(), Errno> {
    let (atime, mtime) = if user_times.addr().is_null() {
        // If user_times is null, the timestamps are updated to the current time.
        (TimeUpdateType::Now, TimeUpdateType::Now)
    } else {
        let ts: [timespec; 2] = current_task.read_object(user_times)?;
        let atime = ts[0];
        let mtime = ts[1];
        let parse_timespec = |spec: timespec| match spec.tv_nsec {
            UTIME_NOW => Ok(TimeUpdateType::Now),
            UTIME_OMIT => Ok(TimeUpdateType::Omit),
            _ => time_from_timespec(spec).map(TimeUpdateType::Time),
        };
        (parse_timespec(atime)?, parse_timespec(mtime)?)
    };

    if let (TimeUpdateType::Omit, TimeUpdateType::Omit) = (atime, mtime) {
        return Ok(());
    };

    // Non-standard feature: if user_path is null, the timestamps are updated on the file referred
    // to by dir_fd.
    // See https://man7.org/linux/man-pages/man2/utimensat.2.html
    let name = if user_path.addr().is_null() {
        if dir_fd == FdNumber::AT_FDCWD {
            return error!(EFAULT);
        }
        let (node, _) = current_task.resolve_dir_fd(
            locked,
            dir_fd,
            Default::default(),
            ResolveFlags::empty(),
        )?;
        node
    } else {
        let lookup_flags = LookupFlags::from_bits(flags, AT_SYMLINK_NOFOLLOW)?;
        lookup_at(locked, current_task, dir_fd, user_path, lookup_flags)?
    };
    name.entry.node.update_atime_mtime(locked, current_task, &name.mount, atime, mtime)?;
    let event_mask = match (atime, mtime) {
        (_, TimeUpdateType::Omit) => InotifyMask::ACCESS,
        (TimeUpdateType::Omit, _) => InotifyMask::MODIFY,
        (_, _) => InotifyMask::ATTRIB,
    };
    name.entry.notify_ignoring_excl_unlink(event_mask);
    Ok(())
}

pub fn sys_splice(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd_in: FdNumber,
    off_in: UserRef<off_t>,
    fd_out: FdNumber,
    off_out: UserRef<off_t>,
    len: usize,
    flags: u32,
) -> Result<usize, Errno> {
    splice::splice(locked, current_task, fd_in, off_in, fd_out, off_out, len, flags)
}

pub fn sys_vmsplice(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: UserValue<i32>,
    flags: u32,
) -> Result<usize, Errno> {
    splice::vmsplice(locked, current_task, fd, iovec_addr, iovec_count, flags)
}

pub fn sys_copy_file_range(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd_in: FdNumber,
    off_in: UserRef<off_t>,
    fd_out: FdNumber,
    off_out: UserRef<off_t>,
    len: usize,
    flags: u32,
) -> Result<usize, Errno> {
    splice::copy_file_range(locked, current_task, fd_in, off_in, fd_out, off_out, len, flags)
}

pub fn sys_tee(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd_in: FdNumber,
    fd_out: FdNumber,
    len: usize,
    flags: u32,
) -> Result<usize, Errno> {
    splice::tee(locked, current_task, fd_in, fd_out, len, flags)
}

pub fn sys_readahead(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    offset: off_t,
    length: usize,
) -> Result<(), Errno> {
    let file = current_task.files.get(fd)?;
    // Allow only non-negative values of `offset`. Some versions of Linux allow it to be negative,
    // but GVisor tests require `readahead()` to fail in this case.
    let offset: usize = offset.try_into().map_err(|_| errno!(EINVAL))?;
    file.readahead(current_task, offset, length)
}

pub fn sys_io_setup(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_nr_events: UserValue<u32>,
    user_ctx_idp: UserRef<aio_context_t>,
) -> Result<(), Errno> {
    // From https://man7.org/linux/man-pages/man2/io_setup.2.html:
    //
    //   EINVAL ctx_idp is not initialized, or the specified nr_events
    //   exceeds internal limits.  nr_events should be greater than
    //   0.
    //
    // TODO: Determine what "internal limits" means.
    let max_operations =
        user_nr_events.validate(0..(i32::MAX as u32)).ok_or_else(|| errno!(EINVAL))? as usize;
    if current_task.read_object(user_ctx_idp)? != 0 {
        return error!(EINVAL);
    }
    let ctx_id = AioContext::create(current_task, max_operations)?;
    current_task.write_object(user_ctx_idp, &ctx_id).map_err(|e| {
        let _ = current_task
            .mm()
            .expect("previous sys_io_setup code verified mm exists")
            .destroy_aio_context(ctx_id.into());
        e
    })?;
    Ok(())
}

pub fn sys_io_submit(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ctx_id: aio_context_t,
    user_nr: UserValue<i32>,
    mut iocb_addrs: UserRef<UserAddress>,
) -> Result<i32, Errno> {
    let nr = user_nr.validate(0..i32::MAX).ok_or_else(|| errno!(EINVAL))?;
    if nr == 0 {
        return Ok(0);
    }
    let ctx = current_task
        .mm()
        .ok_or_else(|| errno!(EINVAL))?
        .get_aio_context(ctx_id.into())
        .ok_or_else(|| errno!(EINVAL))?;

    // `iocbpp` is an array of addresses to iocb's.
    let mut num_submitted: i32 = 0;
    loop {
        let iocb_addr = current_task.read_object(iocb_addrs)?;
        let iocb_ref = UserRef::<iocb>::new(iocb_addr.clone());
        let control_block = current_task.read_object(iocb_ref)?;

        match (num_submitted, ctx.submit(current_task, control_block, iocb_addr)) {
            (0, Err(e)) => return Err(e),
            (_, Err(_)) => break,
            (_, Ok(())) => {
                num_submitted += 1;
                if num_submitted == nr {
                    break;
                }
            }
        };

        iocb_addrs = iocb_addrs.next();
    }

    Ok(num_submitted)
}

pub fn sys_io_getevents(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ctx_id: aio_context_t,
    min_nr: i64,
    nr: i64,
    events_ref: UserRef<io_event>,
    user_timeout: UserRef<timespec>,
) -> Result<i32, Errno> {
    if min_nr < 0 || min_nr > nr || nr < 0 {
        return error!(EINVAL);
    }
    let min_results = min_nr as usize;
    let max_results = nr as usize;
    let deadline = deadline_after_timespec(current_task, user_timeout)?;

    let ctx = current_task
        .mm()
        .ok_or_else(|| errno!(EINVAL))?
        .get_aio_context(ctx_id.into())
        .ok_or_else(|| errno!(EINVAL))?;
    let events = ctx.get_events(current_task, min_results, max_results, deadline)?;
    current_task.write_objects(events_ref, &events)?;

    Ok(events.len() as i32)
}

pub fn sys_io_cancel(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ctx_id: aio_context_t,
    user_iocb: UserRef<iocb>,
    _result: UserRef<io_event>,
) -> Result<(), Errno> {
    let _iocb = current_task.read_object(user_iocb)?;
    let _ctx = current_task
        .mm()
        .ok_or_else(|| errno!(EINVAL))?
        .get_aio_context(ctx_id.into())
        .ok_or_else(|| errno!(EINVAL))?;

    track_stub!(TODO("https://fxbug.dev/297433877"), "io_cancel");
    return error!(ENOSYS);
}

pub fn sys_io_destroy(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ctx_id: aio_context_t,
) -> Result<(), Errno> {
    let aio_context =
        current_task.mm().ok_or_else(|| errno!(EINVAL))?.destroy_aio_context(ctx_id.into())?;
    std::mem::drop(aio_context);
    Ok(())
}

pub fn sys_io_uring_setup(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_entries: UserValue<u32>,
    user_params: UserRef<io_uring_params>,
) -> Result<FdNumber, Errno> {
    if !current_task.kernel().features.io_uring {
        return error!(ENOSYS);
    }

    // Apply policy from /proc/sys/kernel/io_uring_disabled
    let limits = &current_task.kernel().system_limits;
    match limits.io_uring_disabled.load(atomic::Ordering::Relaxed) {
        0 => (),
        1 => {
            if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
                let io_uring_group = limits
                    .io_uring_group
                    .load(atomic::Ordering::Relaxed)
                    .try_into()
                    .map_err(|_| errno!(EPERM))?;
                if !current_task.creds().is_in_group(io_uring_group) {
                    return error!(EPERM);
                }
            }
        }
        _ => {
            return error!(EPERM);
        }
    }

    let entries = user_entries.validate(1..IORING_MAX_ENTRIES).ok_or_else(|| errno!(EINVAL))?;

    let mut params = current_task.read_object(user_params)?;
    for byte in params.resv {
        if byte != 0 {
            return error!(EINVAL);
        }
    }

    const SUPPORTED_FLAGS: u32 = IORING_SETUP_CQSIZE;
    let unsupported_flags = params.flags & !SUPPORTED_FLAGS;
    if unsupported_flags != 0 {
        track_stub!(TODO("https://fxbug.dev/297431387"), "io_uring flags", unsupported_flags);
        return error!(EINVAL);
    }

    let file = IoUringFileObject::new_file(current_task, entries, &mut params)?;

    // io_uring file descriptors are always created with CLOEXEC.
    let fd = current_task.add_file(file, FdFlags::CLOEXEC)?;
    current_task.write_object(user_params, &params)?;
    Ok(fd)
}

pub fn sys_io_uring_enter(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    to_submit: u32,
    min_complete: u32,
    flags: u32,
    _sig: UserRef<sigset_t>,
) -> Result<u32, Errno> {
    if !current_task.kernel().features.io_uring {
        return error!(ENOSYS);
    }
    let file = current_task.files.get(fd)?;
    let io_uring = file.downcast_file::<IoUringFileObject>().ok_or_else(|| errno!(EOPNOTSUPP))?;
    // TODO(https://fxbug.dev/297431387): Use `_sig` to change the signal mask for `current_task`.
    io_uring.enter(locked, current_task, to_submit, min_complete, flags)
}

pub fn sys_io_uring_register(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    opcode: u32,
    arg: UserAddress,
    nr_args: UserValue<i32>,
) -> Result<SyscallResult, Errno> {
    if !current_task.kernel().features.io_uring {
        return error!(ENOSYS);
    }
    let file = current_task.files.get(fd)?;
    let io_uring = file.downcast_file::<IoUringFileObject>().ok_or_else(|| errno!(EOPNOTSUPP))?;
    match opcode {
        IORING_REGISTER_BUFFERS => {
            // TODO(https://fxbug.dev/297431387): Check nr_args for zero and return EINVAL here.
            let buffers = current_task.read_iovec(arg, nr_args)?;
            io_uring.register_buffers(buffers);
            return Ok(SUCCESS);
        }
        IORING_UNREGISTER_BUFFERS => {
            if !arg.is_null() {
                return error!(EINVAL);
            }
            io_uring.unregister_buffers();
            return Ok(SUCCESS);
        }
        _ => return error!(EINVAL),
    }
}

// Syscalls for arch32 usage
#[cfg(feature = "arch32")]
mod arch32 {
    use crate::mm::MemoryAccessorExt;
    use crate::vfs::syscalls::{
        lookup_at, sys_dup3, sys_faccessat, sys_lseek, sys_mkdirat, sys_openat, sys_readlinkat,
        sys_unlinkat, LookupFlags,
    };
    use crate::vfs::{CurrentTask, FdNumber, FsNode};
    use linux_uapi::off_t;
    use starnix_sync::{Locked, Unlocked};
    use starnix_uapi::errors::Errno;
    use starnix_uapi::file_mode::FileMode;
    use starnix_uapi::user_address::{UserAddress, UserCString, UserRef};
    use starnix_uapi::{uapi, AT_REMOVEDIR};

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

    pub fn sys_arch32_readlink(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
        buffer: UserAddress,
        buffer_size: usize,
    ) -> Result<usize, Errno> {
        sys_readlinkat(locked, current_task, FdNumber::AT_FDCWD, user_path, buffer, buffer_size)
    }

    pub fn sys_arch32_mkdir(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
        mode: FileMode,
    ) -> Result<(), Errno> {
        sys_mkdirat(locked, current_task, FdNumber::AT_FDCWD, user_path, mode)
    }

    pub fn sys_arch32_rmdir(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
    ) -> Result<(), Errno> {
        sys_unlinkat(locked, current_task, FdNumber::AT_FDCWD, user_path, AT_REMOVEDIR)
    }

    #[allow(non_snake_case)]
    pub fn sys_arch32__llseek(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        fd: FdNumber,
        offset_high: u32,
        offset_low: u32,
        result: UserRef<off_t>,
        whence: u32,
    ) -> Result<(), Errno> {
        let offset = ((offset_high as off_t) << 32) | (offset_low as off_t);
        let result_value = sys_lseek(locked, current_task, fd, offset, whence)?;
        current_task.write_object(result, &result_value).map(|_| ())
    }

    pub fn sys_arch32_dup2(
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

    pub fn sys_arch32_unlink(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        user_path: UserCString,
    ) -> Result<(), Errno> {
        sys_unlinkat(locked, current_task, FdNumber::AT_FDCWD, user_path, 0)
    }
}

#[cfg(feature = "arch32")]
pub use arch32::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use starnix_types::vfs::default_statfs;
    use starnix_uapi::{O_RDONLY, SEEK_CUR, SEEK_END, SEEK_SET};
    use zerocopy::IntoBytes;

    #[::fuchsia::test]
    async fn test_sys_lseek() -> Result<(), Errno> {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();
        let fd = FdNumber::from_raw(10);
        let file_handle =
            current_task.open_file(&mut locked, "data/testfile.txt".into(), OpenFlags::RDONLY)?;
        let file_size = file_handle.node().stat(&mut locked, &current_task).unwrap().st_size;
        current_task.files.insert(&current_task, fd, file_handle).unwrap();

        assert_eq!(sys_lseek(&mut locked, &current_task, fd, 0, SEEK_CUR)?, 0);
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, 1, SEEK_CUR)?, 1);
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, 3, SEEK_SET)?, 3);
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, -3, SEEK_CUR)?, 0);
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, 0, SEEK_END)?, file_size);
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, -5, SEEK_SET), error!(EINVAL));

        // Make sure that the failed call above did not change the offset.
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, 0, SEEK_CUR)?, file_size);

        // Prepare for an overflow.
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, 3, SEEK_SET)?, 3);

        // Check for overflow.
        assert_eq!(sys_lseek(&mut locked, &current_task, fd, i64::MAX, SEEK_CUR), error!(EINVAL));

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_sys_dup() -> Result<(), Errno> {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();
        let file_handle =
            current_task.open_file(&mut locked, "data/testfile.txt".into(), OpenFlags::RDONLY)?;
        let oldfd = current_task.add_file(file_handle, FdFlags::empty())?;
        let newfd = sys_dup(&mut locked, &current_task, oldfd)?;

        assert_ne!(oldfd, newfd);
        let files = &current_task.files;
        assert!(Arc::ptr_eq(&files.get(oldfd).unwrap(), &files.get(newfd).unwrap()));

        assert_eq!(sys_dup(&mut locked, &current_task, FdNumber::from_raw(3)), error!(EBADF));

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_sys_dup3() -> Result<(), Errno> {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();
        let file_handle =
            current_task.open_file(&mut locked, "data/testfile.txt".into(), OpenFlags::RDONLY)?;
        let oldfd = current_task.add_file(file_handle, FdFlags::empty())?;
        let newfd = FdNumber::from_raw(2);
        sys_dup3(&mut locked, &current_task, oldfd, newfd, O_CLOEXEC)?;

        assert_ne!(oldfd, newfd);
        let files = &current_task.files;
        assert!(Arc::ptr_eq(&files.get(oldfd).unwrap(), &files.get(newfd).unwrap()));
        assert_eq!(files.get_fd_flags_allowing_opath(oldfd).unwrap(), FdFlags::empty());
        assert_eq!(files.get_fd_flags_allowing_opath(newfd).unwrap(), FdFlags::CLOEXEC);

        assert_eq!(sys_dup3(&mut locked, &current_task, oldfd, oldfd, O_CLOEXEC), error!(EINVAL));

        // Pass invalid flags.
        let invalid_flags = 1234;
        assert_eq!(
            sys_dup3(&mut locked, &current_task, oldfd, newfd, invalid_flags),
            error!(EINVAL)
        );

        // Makes sure that dup closes the old file handle before the fd points
        // to the new file handle.
        let second_file_handle =
            current_task.open_file(&mut locked, "data/testfile.txt".into(), OpenFlags::RDONLY)?;
        let different_file_fd = current_task.add_file(second_file_handle, FdFlags::empty())?;
        assert!(!Arc::ptr_eq(&files.get(oldfd).unwrap(), &files.get(different_file_fd).unwrap()));
        sys_dup3(&mut locked, &current_task, oldfd, different_file_fd, O_CLOEXEC)?;
        assert!(Arc::ptr_eq(&files.get(oldfd).unwrap(), &files.get(different_file_fd).unwrap()));

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_sys_open_cloexec() -> Result<(), Errno> {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();
        let path_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let path = b"data/testfile.txt\0";
        current_task.write_memory(path_addr, path)?;
        let fd = sys_openat(
            &mut locked,
            &current_task,
            FdNumber::AT_FDCWD,
            UserCString::new(path_addr),
            O_RDONLY | O_CLOEXEC,
            FileMode::default(),
        )?;
        assert!(current_task.files.get_fd_flags_allowing_opath(fd)?.contains(FdFlags::CLOEXEC));
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_sys_epoll() -> Result<(), Errno> {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();

        let epoll_fd =
            sys_epoll_create1(&mut locked, &current_task, 0).expect("sys_epoll_create1 failed");
        sys_close(&mut locked, &current_task, epoll_fd).expect("sys_close failed");

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_fstat_tmp_file() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();

        // Create the file that will be used to stat.
        let file_path = "data/testfile.txt";
        let _file_handle =
            current_task.open_file(&mut locked, file_path.into(), OpenFlags::RDONLY).unwrap();

        // Write the path to user memory.
        let path_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task.write_memory(path_addr, file_path.as_bytes()).expect("failed to clear struct");

        let user_stat = UserRef::new(path_addr + file_path.len());
        current_task.write_object(user_stat, &default_statfs(0)).expect("failed to clear struct");

        let user_path = UserCString::new(path_addr);

        assert_eq!(sys_statfs(&mut locked, &current_task, user_path, user_stat), Ok(()));

        let returned_stat = current_task.read_object(user_stat).expect("failed to read struct");
        assert_eq!(
            returned_stat.as_bytes(),
            default_statfs(u32::from_be_bytes(*b"f.io")).as_bytes()
        );
    }

    #[::fuchsia::test]
    async fn test_unlinkat_dir() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        // Create the dir that we will attempt to unlink later.
        let no_slash_path = b"testdir";
        let no_slash_path_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task.write_memory(no_slash_path_addr, no_slash_path).expect("failed to write path");
        let no_slash_user_path = UserCString::new(no_slash_path_addr);
        sys_mkdirat(
            &mut locked,
            &current_task,
            FdNumber::AT_FDCWD,
            no_slash_user_path,
            FileMode::ALLOW_ALL.with_type(FileMode::IFDIR),
        )
        .unwrap();

        let slash_path = b"testdir/";
        let slash_path_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task.write_memory(slash_path_addr, slash_path).expect("failed to write path");
        let slash_user_path = UserCString::new(slash_path_addr);

        // Try to remove a directory without specifying AT_REMOVEDIR.
        // This should fail with EISDIR, irrespective of the terminating slash.
        let error =
            sys_unlinkat(&mut locked, &current_task, FdNumber::AT_FDCWD, slash_user_path, 0)
                .unwrap_err();
        assert_eq!(error, errno!(EISDIR));
        let error =
            sys_unlinkat(&mut locked, &current_task, FdNumber::AT_FDCWD, no_slash_user_path, 0)
                .unwrap_err();
        assert_eq!(error, errno!(EISDIR));

        // Success with AT_REMOVEDIR.
        sys_unlinkat(&mut locked, &current_task, FdNumber::AT_FDCWD, slash_user_path, AT_REMOVEDIR)
            .unwrap();
    }

    #[::fuchsia::test]
    async fn test_rename_noreplace() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked_with_pkgfs();

        // Create the file that will be renamed.
        let old_user_path = "data/testfile.txt";
        let _old_file_handle =
            current_task.open_file(&mut locked, old_user_path.into(), OpenFlags::RDONLY).unwrap();

        // Write the path to user memory.
        let old_path_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(old_path_addr, old_user_path.as_bytes())
            .expect("failed to clear struct");

        // Create a second file that we will attempt to rename to.
        let new_user_path = "data/testfile2.txt";
        let _new_file_handle =
            current_task.open_file(&mut locked, new_user_path.into(), OpenFlags::RDONLY).unwrap();

        // Write the path to user memory.
        let new_path_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(new_path_addr, new_user_path.as_bytes())
            .expect("failed to clear struct");

        // Try to rename first file to second file's name with RENAME_NOREPLACE flag.
        // This should fail with EEXIST.
        let error = sys_renameat2(
            &mut locked,
            &current_task,
            FdNumber::AT_FDCWD,
            UserCString::new(old_path_addr),
            FdNumber::AT_FDCWD,
            UserCString::new(new_path_addr),
            RenameFlags::NOREPLACE.bits(),
        )
        .unwrap_err();
        assert_eq!(error, errno!(EEXIST));
    }
}
