// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::execution::execute_task;
use crate::mm::{DumpPolicy, MemoryAccessor, MemoryAccessorExt, PAGE_SIZE};
use crate::security;
use crate::task::{
    max_priority_for_sched_policy, min_priority_for_sched_policy, ptrace_attach, ptrace_dispatch,
    ptrace_traceme, CurrentTask, ExitStatus, PtraceAllowedPtracers, PtraceAttachType,
    PtraceOptions, SchedulerPolicy, SeccompAction, SeccompStateValue, Task, PR_SET_PTRACER_ANY,
};
use crate::vfs::{
    FdNumber, FileHandle, MountNamespaceFile, PidFdFileObject, UserBuffersOutputBuffer,
    VecOutputBuffer,
};
use starnix_logging::{log_error, log_info, log_trace, set_zx_name, track_stub};
use starnix_sync::{Locked, RwLock, Unlocked};
use starnix_syscalls::SyscallResult;
use starnix_types::ownership::WeakRef;
use starnix_types::time::timeval_from_duration;
use starnix_uapi::auth::{
    Capabilities, Credentials, SecureBits, CAP_SETGID, CAP_SETPCAP, CAP_SETUID, CAP_SYS_ADMIN,
    CAP_SYS_NICE, CAP_SYS_PTRACE, CAP_SYS_RESOURCE, CAP_SYS_TTY_CONFIG,
};
use starnix_uapi::errors::{Errno, ENAMETOOLONG};
use starnix_uapi::file_mode::{Access, AccessCheck, FileMode};
use starnix_uapi::kcmp::KcmpResource;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::signals::{Signal, UncheckedSignal};
use starnix_uapi::syslog::SyslogAction;
use starnix_uapi::user_address::{MultiArchUserRef, UserAddress, UserCString, UserRef};
use starnix_uapi::vfs::ResolveFlags;
use starnix_uapi::{
    __user_cap_data_struct, __user_cap_header_struct, c_char, c_int, clone_args, errno, error,
    gid_t, pid_t, rlimit, rusage, sched_param, sock_filter, sock_fprog, uapi, uid_t, AT_EMPTY_PATH,
    AT_SYMLINK_NOFOLLOW, BPF_MAXINSNS, CLONE_ARGS_SIZE_VER0, CLONE_ARGS_SIZE_VER1,
    CLONE_ARGS_SIZE_VER2, CLONE_FILES, CLONE_FS, CLONE_NEWNS, CLONE_NEWUTS, CLONE_SETTLS,
    CLONE_VFORK, NGROUPS_MAX, PATH_MAX, PRIO_PROCESS, PR_CAPBSET_DROP, PR_CAPBSET_READ,
    PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, PR_CAP_AMBIENT_IS_SET, PR_CAP_AMBIENT_LOWER,
    PR_CAP_AMBIENT_RAISE, PR_GET_CHILD_SUBREAPER, PR_GET_DUMPABLE, PR_GET_KEEPCAPS, PR_GET_NAME,
    PR_GET_NO_NEW_PRIVS, PR_GET_SECCOMP, PR_GET_SECUREBITS, PR_SET_CHILD_SUBREAPER,
    PR_SET_DUMPABLE, PR_SET_KEEPCAPS, PR_SET_NAME, PR_SET_NO_NEW_PRIVS, PR_SET_PDEATHSIG,
    PR_SET_PTRACER, PR_SET_SECCOMP, PR_SET_SECUREBITS, PR_SET_TIMERSLACK, PR_SET_VMA,
    PR_SET_VMA_ANON_NAME, PTRACE_ATTACH, PTRACE_SEIZE, PTRACE_TRACEME, RUSAGE_CHILDREN,
    SECCOMP_FILTER_FLAG_LOG, SECCOMP_FILTER_FLAG_NEW_LISTENER, SECCOMP_FILTER_FLAG_SPEC_ALLOW,
    SECCOMP_FILTER_FLAG_TSYNC, SECCOMP_FILTER_FLAG_TSYNC_ESRCH, SECCOMP_GET_ACTION_AVAIL,
    SECCOMP_GET_NOTIF_SIZES, SECCOMP_MODE_FILTER, SECCOMP_MODE_STRICT, SECCOMP_SET_MODE_FILTER,
    SECCOMP_SET_MODE_STRICT, _LINUX_CAPABILITY_VERSION_1, _LINUX_CAPABILITY_VERSION_2,
    _LINUX_CAPABILITY_VERSION_3,
};
use static_assertions::const_assert;
use std::cmp;
use std::ffi::CString;
use std::sync::{Arc, LazyLock};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub fn do_clone(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    args: &clone_args,
) -> Result<pid_t, Errno> {
    security::check_task_create_access(current_task)?;

    let child_exit_signal = if args.exit_signal == 0 {
        None
    } else {
        Some(Signal::try_from(UncheckedSignal::new(args.exit_signal))?)
    };

    let mut new_task = current_task.clone_task(
        locked,
        args.flags,
        child_exit_signal,
        UserRef::<pid_t>::new(UserAddress::from(args.parent_tid)),
        UserRef::<pid_t>::new(UserAddress::from(args.child_tid)),
    )?;
    // Set the result register to 0 for the return value from clone in the
    // cloned process.
    new_task.thread_state.registers.set_return_register(0);
    let (trace_kind, ptrace_state) = current_task.get_ptrace_core_state_for_clone(args);

    if args.stack != 0 {
        // In clone() the `stack` argument points to the top of the stack, while in clone3()
        // `stack` points to the bottom of the stack. Therefore, in clone3() we need to add
        // `stack_size` to calculate the stack pointer. Note that in clone() `stack_size` is 0.
        new_task
            .thread_state
            .registers
            .set_stack_pointer_register(args.stack.wrapping_add(args.stack_size));
    }
    if args.flags & (CLONE_SETTLS as u64) != 0 {
        new_task.thread_state.registers.set_thread_pointer_register(args.tls);
    }

    let tid = new_task.task.id;
    let task_ref = WeakRef::from(&new_task.task);
    execute_task(locked, new_task, |_, _| Ok(()), |_| {}, ptrace_state)?;

    current_task.ptrace_event(locked, trace_kind, tid as u64);

    if args.flags & (CLONE_VFORK as u64) != 0 {
        current_task.wait_for_execve(task_ref)?;
        current_task.ptrace_event(locked, PtraceOptions::TRACEVFORKDONE, tid as u64);
    }
    Ok(tid)
}

pub fn sys_clone3(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    user_clone_args: UserRef<clone_args>,
    user_clone_args_size: usize,
) -> Result<pid_t, Errno> {
    // Only these specific sized versions are supported.
    if !(user_clone_args_size == CLONE_ARGS_SIZE_VER0 as usize
        || user_clone_args_size == CLONE_ARGS_SIZE_VER1 as usize
        || user_clone_args_size == CLONE_ARGS_SIZE_VER2 as usize)
    {
        return error!(EINVAL);
    }

    // The most recent version of the struct size should match our definition.
    const_assert!(std::mem::size_of::<clone_args>() == CLONE_ARGS_SIZE_VER2 as usize);

    let clone_args = current_task.read_object_partial(user_clone_args, user_clone_args_size)?;
    do_clone(locked, current_task, &clone_args)
}

fn read_c_string_vector(
    mm: &CurrentTask,
    user_vector: UserRef<UserCString>,
    elem_limit: usize,
    vec_limit: usize,
) -> Result<(Vec<CString>, usize), Errno> {
    let mut user_current = user_vector;
    let mut vector: Vec<CString> = vec![];
    let mut vec_size: usize = 0;
    loop {
        let user_string = mm.read_object(user_current)?;
        if user_string.is_null() {
            break;
        }
        let string = mm.read_c_string_to_vec(user_string, elem_limit).map_err(|e| {
            if e.code == ENAMETOOLONG {
                errno!(E2BIG)
            } else {
                e
            }
        })?;
        let cstring = CString::new(string).map_err(|_| errno!(EINVAL))?;
        vec_size =
            vec_size.checked_add(cstring.as_bytes_with_nul().len()).ok_or_else(|| errno!(E2BIG))?;
        if vec_size > vec_limit {
            return error!(E2BIG);
        }
        vector.push(cstring);
        user_current = user_current.next();
    }
    Ok((vector, vec_size))
}

pub fn sys_execve(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    user_path: UserCString,
    user_argv: UserRef<UserCString>,
    user_environ: UserRef<UserCString>,
) -> Result<(), Errno> {
    sys_execveat(locked, current_task, FdNumber::AT_FDCWD, user_path, user_argv, user_environ, 0)
}

pub fn sys_execveat(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    dir_fd: FdNumber,
    user_path: UserCString,
    user_argv: UserRef<UserCString>,
    user_environ: UserRef<UserCString>,
    flags: u32,
) -> Result<(), Errno> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return error!(EINVAL);
    }

    // Calculate the limit for argv and environ size as 1/4 of the stack size, floored at 32 pages.
    // See the Limits sections in https://man7.org/linux/man-pages/man2/execve.2.html
    const PAGE_LIMIT: usize = 32;
    let page_limit_size: usize = PAGE_LIMIT * *PAGE_SIZE as usize;
    let rlimit = current_task.thread_group.get_rlimit(Resource::STACK);
    let stack_limit = rlimit / 4;
    let argv_env_limit = cmp::max(page_limit_size, stack_limit as usize);

    // The limit per argument or environment variable is 32 pages.
    // See the Limits sections in https://man7.org/linux/man-pages/man2/execve.2.html
    let (argv, argv_size) = if user_argv.is_null() {
        (Vec::new(), 0)
    } else {
        read_c_string_vector(current_task, user_argv, page_limit_size, argv_env_limit)?
    };

    let (environ, _) = if user_environ.is_null() {
        (Vec::new(), 0)
    } else {
        read_c_string_vector(
            current_task,
            user_environ,
            page_limit_size,
            argv_env_limit - argv_size,
        )?
    };

    let path = &current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;

    log_trace!(argv:?, environ:?, flags:?; "execveat({dir_fd}, {path})");

    let mut open_flags = OpenFlags::RDONLY;

    if flags & AT_SYMLINK_NOFOLLOW != 0 {
        open_flags |= OpenFlags::NOFOLLOW;
    }

    let executable = if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            // If AT_EMPTY_PATH is not set, this is an error.
            return error!(ENOENT);
        }

        // O_PATH allowed for:
        //
        //   Passing the file descriptor as the dirfd argument of
        //   openat() and the other "*at()" system calls.  This
        //   includes linkat(2) with AT_EMPTY_PATH (or via procfs
        //   using AT_SYMLINK_FOLLOW) even if the file is not a
        //   directory.
        //
        // See https://man7.org/linux/man-pages/man2/open.2.html
        let file = current_task.files.get_allowing_opath(dir_fd)?;

        // We are forced to reopen the file with O_RDONLY to get access to the underlying VMO.
        // Note that skip the access check in the arguments in case the file mode does
        // not actually have the read permission bit.
        //
        // This can happen because a file could have --x--x--x mode permissions and then
        // be opened with O_PATH. Internally, the file operations would all be stubbed out
        // for that file, which is undesirable here.
        //
        // See https://man7.org/linux/man-pages/man3/fexecve.3.html#DESCRIPTION
        file.name.open(
            locked,
            current_task,
            OpenFlags::RDONLY,
            AccessCheck::check_for(Access::EXEC),
        )?
    } else {
        current_task.open_file_at(
            locked,
            dir_fd,
            path.as_ref(),
            open_flags,
            FileMode::default(),
            ResolveFlags::empty(),
            AccessCheck::check_for(Access::EXEC),
        )?
    };

    // This path can affect script resolution (the path is appended to the script args)
    // and the auxiliary value `AT_EXECFN` from the syscall `getauxval()`
    let path = if dir_fd == FdNumber::AT_FDCWD {
        // The file descriptor is CWD, so the path is exactly
        // what the user specified.
        path.to_vec()
    } else {
        // The path is `/dev/fd/N/P` where N is the file descriptor
        // number and P is the user-provided path (if relative and non-empty).
        //
        // See https://man7.org/linux/man-pages/man2/execveat.2.html#NOTES
        match path.first() {
            Some(b'/') => {
                // The user-provided path is absolute, so dir_fd is ignored.
                path.to_vec()
            }
            Some(_) => {
                // User-provided path is relative, append it.
                let mut new_path = format!("/dev/fd/{}/", dir_fd.raw()).into_bytes();
                new_path.append(&mut path.to_vec());
                new_path
            }
            // User-provided path is empty
            None => format!("/dev/fd/{}", dir_fd.raw()).into_bytes(),
        }
    };

    let path = CString::new(path).map_err(|_| errno!(EINVAL))?;

    current_task.exec(locked, executable, path, argv, environ)?;
    Ok(())
}

pub fn sys_getcpu(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    cpu_out: UserRef<u32>,
    node_out: UserRef<u32>,
) -> Result<(), Errno> {
    // "When either cpu or node is NULL nothing is written to the respective pointer."
    // from https://man7.org/linux/man-pages/man2/getcpu.2.html
    if !cpu_out.is_null() {
        let thread_stats = current_task
            .thread
            .read()
            .as_ref()
            .expect("current thread is never None when executing")
            .get_stats()
            .map_err(|e| errno!(EINVAL, format!("getting thread stats failed {e:?}")))?;
        current_task.write_object(cpu_out, &thread_stats.last_scheduled_cpu)?;
    }
    if !node_out.is_null() {
        // Zircon does not yet have a concept of NUMA task scheduling, always tell userspace that
        // it's on the "first" node which should be true for non-NUMA systems.
        track_stub!(TODO("https://fxbug.dev/325643815"), "getcpu() numa node");
        current_task.write_object(node_out, &0)?;
    }
    Ok(())
}

pub fn sys_getpid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<pid_t, Errno> {
    Ok(current_task.get_pid())
}

pub fn sys_gettid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<pid_t, Errno> {
    Ok(current_task.get_tid())
}

pub fn sys_getppid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<pid_t, Errno> {
    Ok(current_task.thread_group.read().get_ppid())
}

fn get_task_if_owner_or_has_capabilities(
    current_task: &CurrentTask,
    pid: pid_t,
    capabilities: Capabilities,
) -> Result<WeakRef<Task>, Errno> {
    let weak = current_task.get_task(pid);
    let task_creds = Task::from_weak(&weak)?.creds();
    let current_creds = current_task.creds();
    if task_creds.euid != current_creds.euid {
        security::check_task_capable(current_task, capabilities)?;
    }
    Ok(weak)
}

fn get_task_or_current(current_task: &CurrentTask, pid: pid_t) -> WeakRef<Task> {
    if pid == 0 {
        current_task.weak_task()
    } else {
        // TODO(security): Should this use get_task_if_owner_or_has_capabilities() ?
        current_task.get_task(pid)
    }
}

pub fn sys_getsid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
) -> Result<pid_t, Errno> {
    let weak = get_task_or_current(current_task, pid);
    let target_task = Task::from_weak(&weak)?;
    security::check_task_getsid(current_task, &target_task)?;
    let sid = target_task.thread_group.read().process_group.session.leader;
    Ok(sid)
}

pub fn sys_getpgid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
) -> Result<pid_t, Errno> {
    let weak = get_task_or_current(current_task, pid);
    let task = Task::from_weak(&weak)?;

    security::check_getpgid_access(current_task, &task)?;
    let pgid = task.thread_group.read().process_group.leader;
    Ok(pgid)
}

pub fn sys_setpgid(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    pgid: pid_t,
) -> Result<(), Errno> {
    let weak = get_task_or_current(current_task, pid);
    let task = Task::from_weak(&weak)?;

    security::check_setpgid_access(current_task, &task)?;
    current_task.thread_group.setpgid(locked, &task, pgid)?;
    Ok(())
}

// A non-root process is allowed to set any of its three uids to the value of any other. The
// CAP_SETUID capability bypasses these checks and allows setting any uid to any integer. Likewise
// for gids.
fn new_uid_allowed(creds: &Credentials, uid: uid_t) -> bool {
    creds.has_capability(CAP_SETUID)
        || uid == creds.uid
        || uid == creds.euid
        || uid == creds.saved_uid
}

fn new_gid_allowed(creds: &Credentials, gid: gid_t) -> bool {
    creds.has_capability(CAP_SETGID)
        || gid == creds.gid
        || gid == creds.egid
        || gid == creds.saved_gid
}

pub fn sys_getuid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<uid_t, Errno> {
    Ok(current_task.creds().uid)
}

pub fn sys_getgid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<gid_t, Errno> {
    Ok(current_task.creds().gid)
}

pub fn sys_setuid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    uid: uid_t,
) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    if uid == gid_t::MAX {
        return error!(EINVAL);
    }
    if !new_uid_allowed(&creds, uid) {
        return error!(EPERM);
    }
    let prev = creds.copy_user_credentials();
    creds.euid = uid;
    creds.fsuid = uid;
    if creds.has_capability(CAP_SETUID) {
        creds.uid = uid;
        creds.saved_uid = uid;
    }

    creds.update_capabilities(prev);
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setgid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    gid: gid_t,
) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    if gid == gid_t::MAX {
        return error!(EINVAL);
    }
    if !new_gid_allowed(&creds, gid) {
        return error!(EPERM);
    }
    creds.egid = gid;
    creds.fsgid = gid;
    if creds.has_capability(CAP_SETGID) {
        creds.gid = gid;
        creds.saved_gid = gid;
    }
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_geteuid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<uid_t, Errno> {
    Ok(current_task.creds().euid)
}

pub fn sys_getegid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<gid_t, Errno> {
    Ok(current_task.creds().egid)
}

pub fn sys_setfsuid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fsuid: uid_t,
) -> Result<uid_t, Errno> {
    let mut creds = current_task.creds();
    let prev = creds.copy_user_credentials();
    if fsuid != u32::MAX && new_uid_allowed(&creds, fsuid) {
        creds.fsuid = fsuid;
        creds.update_capabilities(prev);
        current_task.set_creds(creds);
    }

    Ok(prev.fsuid)
}

pub fn sys_setfsgid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fsgid: gid_t,
) -> Result<gid_t, Errno> {
    let mut creds = current_task.creds();
    let prev = creds.copy_user_credentials();
    let prev_fsgid = creds.fsgid;

    if fsgid != u32::MAX && new_gid_allowed(&creds, fsgid) {
        creds.fsgid = fsgid;
        creds.update_capabilities(prev);
        current_task.set_creds(creds);
    }

    Ok(prev_fsgid)
}

pub fn sys_getresuid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ruid_addr: UserRef<uid_t>,
    euid_addr: UserRef<uid_t>,
    suid_addr: UserRef<uid_t>,
) -> Result<(), Errno> {
    let creds = current_task.creds();
    current_task.write_object(ruid_addr, &creds.uid)?;
    current_task.write_object(euid_addr, &creds.euid)?;
    current_task.write_object(suid_addr, &creds.saved_uid)?;
    Ok(())
}

pub fn sys_getresgid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    rgid_addr: UserRef<gid_t>,
    egid_addr: UserRef<gid_t>,
    sgid_addr: UserRef<gid_t>,
) -> Result<(), Errno> {
    let creds = current_task.creds();
    current_task.write_object(rgid_addr, &creds.gid)?;
    current_task.write_object(egid_addr, &creds.egid)?;
    current_task.write_object(sgid_addr, &creds.saved_gid)?;
    Ok(())
}

pub fn sys_setreuid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ruid: uid_t,
    euid: uid_t,
) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    let allowed = |uid| uid == u32::MAX || new_uid_allowed(&creds, uid);
    if !allowed(ruid) || !allowed(euid) {
        return error!(EPERM);
    }

    let prev = creds.copy_user_credentials();
    let mut is_ruid_set = false;
    if ruid != u32::MAX {
        creds.uid = ruid;
        is_ruid_set = true;
    }
    if euid != u32::MAX {
        creds.euid = euid;
        creds.fsuid = euid;
    }

    if is_ruid_set || prev.uid != euid {
        creds.saved_uid = creds.euid;
    }

    creds.update_capabilities(prev);
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setregid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    rgid: gid_t,
    egid: gid_t,
) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    let allowed = |gid| gid == u32::MAX || new_gid_allowed(&creds, gid);
    if !allowed(rgid) || !allowed(egid) {
        return error!(EPERM);
    }
    let previous_rgid = creds.gid;
    let mut is_rgid_set = false;
    if rgid != u32::MAX {
        creds.gid = rgid;
        is_rgid_set = true;
    }
    if egid != u32::MAX {
        creds.egid = egid;
        creds.fsgid = egid;
    }

    if is_rgid_set || previous_rgid != egid {
        creds.saved_gid = creds.egid;
    }

    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setresuid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ruid: uid_t,
    euid: uid_t,
    suid: uid_t,
) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    let allowed = |uid| uid == u32::MAX || new_uid_allowed(&creds, uid);
    if !allowed(ruid) || !allowed(euid) || !allowed(suid) {
        return error!(EPERM);
    }

    let prev = creds.copy_user_credentials();
    if ruid != u32::MAX {
        creds.uid = ruid;
    }
    if euid != u32::MAX {
        creds.euid = euid;
        creds.fsuid = euid;
    }
    if suid != u32::MAX {
        creds.saved_uid = suid;
    }
    creds.update_capabilities(prev);
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setresgid(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    rgid: gid_t,
    egid: gid_t,
    sgid: gid_t,
) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    let allowed = |gid| gid == u32::MAX || new_gid_allowed(&creds, gid);
    if !allowed(rgid) || !allowed(egid) || !allowed(sgid) {
        return error!(EPERM);
    }
    if rgid != u32::MAX {
        creds.gid = rgid;
    }
    if egid != u32::MAX {
        creds.egid = egid;
        creds.fsgid = egid;
    }
    if sgid != u32::MAX {
        creds.saved_gid = sgid;
    }
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_exit(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    code: i32,
) -> Result<(), Errno> {
    // Only change the current exit status if this has not been already set by exit_group, as
    // otherwise it has priority.
    current_task.write().set_exit_status_if_not_already(ExitStatus::Exit(code as u8));
    Ok(())
}

pub fn sys_exit_group(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    code: i32,
) -> Result<(), Errno> {
    current_task.thread_group_exit(locked, ExitStatus::Exit(code as u8));
    Ok(())
}

pub fn sys_sched_getscheduler(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
) -> Result<u32, Errno> {
    if pid < 0 {
        return error!(EINVAL);
    }

    let weak = get_task_or_current(current_task, pid);
    let target_task = Task::from_weak(&weak)?;
    security::check_getsched_access(current_task, target_task.as_ref())?;
    let current_policy = target_task.read().scheduler_policy;
    Ok(current_policy.raw_policy())
}

pub fn sys_sched_setscheduler(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    policy: u32,
    param: UserAddress,
) -> Result<(), Errno> {
    if pid < 0 || param.is_null() {
        return error!(EINVAL);
    }

    let weak = get_task_or_current(current_task, pid);
    let target_task = Task::from_weak(&weak)?;
    let rlimit = target_task.thread_group.get_rlimit(Resource::RTPRIO);

    security::check_setsched_access(current_task, &target_task)?;
    let param: sched_param = current_task.read_object(param.into())?;
    let policy = SchedulerPolicy::from_sched_params(policy, param, rlimit)?;
    target_task.set_scheduler_policy(policy)?;

    Ok(())
}

type CpuAffinityMask = u64;
const CPU_AFFINITY_MASK_SIZE: u32 = std::mem::size_of::<CpuAffinityMask>() as u32;
const NUM_CPUS_MAX: u32 = CPU_AFFINITY_MASK_SIZE * 8;

fn get_default_cpumask() -> CpuAffinityMask {
    match zx::system_get_num_cpus() {
        num_cpus if num_cpus > NUM_CPUS_MAX => {
            log_error!("num_cpus={}, greater than the {} max supported.", num_cpus, NUM_CPUS_MAX);
            CpuAffinityMask::MAX
        }
        NUM_CPUS_MAX => CpuAffinityMask::MAX,
        num_cpus => (1 << num_cpus) - 1,
    }
}

pub fn sys_sched_getaffinity(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    cpusetsize: u32,
    user_mask: UserAddress,
) -> Result<usize, Errno> {
    if pid < 0 {
        return error!(EINVAL);
    }
    if cpusetsize < CPU_AFFINITY_MASK_SIZE
        || cpusetsize % (std::mem::size_of::<usize>() as u32) != 0
    {
        return error!(EINVAL);
    }

    let weak = get_task_or_current(current_task, pid);
    let _task = Task::from_weak(&weak)?;

    // sched_setaffinity() is not implemented. Fake affinity mask based on the number of CPUs.
    let mask = get_default_cpumask();
    current_task.write_memory(user_mask, &mask.to_ne_bytes())?;
    track_stub!(TODO("https://fxbug.dev/322874659"), "sched_getaffinity");
    Ok(CPU_AFFINITY_MASK_SIZE as usize)
}

pub fn sys_sched_setaffinity(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    cpusetsize: u32,
    user_mask: UserAddress,
) -> Result<(), Errno> {
    security::check_task_capable(current_task, CAP_SYS_NICE)?;
    if pid < 0 {
        return error!(EINVAL);
    }
    let weak = get_task_or_current(current_task, pid);
    let _task = Task::from_weak(&weak)?;

    if cpusetsize < CPU_AFFINITY_MASK_SIZE {
        return error!(EINVAL);
    }

    let mask = current_task.read_object::<CpuAffinityMask>(user_mask.into())?;

    // Specified mask must include at least one valid CPU.
    if mask & get_default_cpumask() == 0 {
        return error!(EINVAL);
    }

    // Currently, we ignore the mask and act as if the system reset the mask
    // immediately to allowing all CPUs.
    track_stub!(TODO("https://fxbug.dev/322874889"), "sched_setaffinity");
    Ok(())
}

pub fn sys_sched_getparam(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    param: UserAddress,
) -> Result<(), Errno> {
    if pid < 0 || param.is_null() {
        return error!(EINVAL);
    }

    let weak = get_task_or_current(current_task, pid);
    let target_task = Task::from_weak(&weak)?;
    let param_value = target_task.read().scheduler_policy.raw_params();
    current_task.write_object(param.into(), &param_value)?;
    Ok(())
}

pub fn sys_sched_setparam(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    param: UserAddress,
) -> Result<(), Errno> {
    if pid < 0 || param.is_null() {
        return error!(EINVAL);
    }

    let new_params: sched_param = current_task.read_object(param.into())?;
    let weak = get_task_or_current(current_task, pid);
    let target_task = Task::from_weak(&weak)?;
    let current_policy = target_task.read().scheduler_policy;

    let rlimit = target_task.thread_group.get_rlimit(Resource::RTPRIO);

    let policy =
        SchedulerPolicy::from_sched_params(current_policy.raw_policy(), new_params, rlimit)?;
    target_task.set_scheduler_policy(policy)?;

    Ok(())
}

pub fn sys_sched_get_priority_min(
    _locked: &mut Locked<'_, Unlocked>,
    _ctx: &CurrentTask,
    policy: u32,
) -> Result<u8, Errno> {
    min_priority_for_sched_policy(policy)
}

pub fn sys_sched_get_priority_max(
    _locked: &mut Locked<'_, Unlocked>,
    _ctx: &CurrentTask,
    policy: u32,
) -> Result<u8, Errno> {
    max_priority_for_sched_policy(policy)
}

pub fn sys_ioprio_set(
    _locked: &mut Locked<'_, Unlocked>,
    _current_task: &mut CurrentTask,
    _which: i32,
    _who: i32,
    _ioprio: i32,
) -> Result<(), Errno> {
    track_stub!(TODO("https://fxbug.dev/297591758"), "ioprio_set()");
    error!(ENOSYS)
}

pub fn sys_prctl(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    option: u32,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> Result<SyscallResult, Errno> {
    match option {
        PR_SET_VMA => {
            if arg2 != PR_SET_VMA_ANON_NAME as u64 {
                track_stub!(TODO("https://fxbug.dev/322874826"), "prctl PR_SET_VMA", arg2);
                return error!(ENOSYS);
            }
            let addr = UserAddress::from(arg3);
            let length = arg4 as usize;
            let name_addr = UserAddress::from(arg5);
            let name = if name_addr.is_null() {
                None
            } else {
                let name = UserCString::new(UserAddress::from(arg5));
                let name = current_task.read_c_string_to_vec(name, 256).map_err(|e| {
                    // An overly long name produces EINVAL and not ENAMETOOLONG in Linux 5.15.
                    if e.code == ENAMETOOLONG {
                        errno!(EINVAL)
                    } else {
                        e
                    }
                })?;
                // Some characters are forbidden in VMA names.
                if name.iter().any(|b| {
                    matches!(b,
                        0..=0x1f |
                        0x7f..=0xff |
                        b'\\' | b'`' | b'$' | b'[' | b']'
                    )
                }) {
                    return error!(EINVAL);
                }
                Some(name)
            };
            current_task
                .mm()
                .ok_or_else(|| errno!(EINVAL))?
                .set_mapping_name(addr, length, name)?;
            Ok(().into())
        }
        PR_SET_DUMPABLE => {
            let mut dumpable =
                current_task.mm().ok_or_else(|| errno!(EINVAL))?.dumpable.lock(locked);
            *dumpable = if arg2 == 1 { DumpPolicy::User } else { DumpPolicy::Disable };
            Ok(().into())
        }
        PR_GET_DUMPABLE => {
            let dumpable = current_task.mm().ok_or_else(|| errno!(EINVAL))?.dumpable.lock(locked);
            Ok(match *dumpable {
                DumpPolicy::Disable => 0.into(),
                DumpPolicy::User => 1.into(),
            })
        }
        PR_SET_PDEATHSIG => {
            track_stub!(TODO("https://fxbug.dev/322874397"), "PR_SET_PDEATHSIG");
            Ok(().into())
        }
        PR_SET_NAME => {
            let addr = UserAddress::from(arg2);
            let mut name = current_task.read_memory_to_array::<16>(addr)?;
            set_zx_name(&fuchsia_runtime::thread_self(), &name);

            // The name is truncated to 16 bytes (including the nul)
            name[15] = 0;
            // this will succeed, because we set 0 at end above
            let string_end = name.iter().position(|&c| c == 0).unwrap();
            let name_str = CString::new(&mut name[0..string_end]).map_err(|_| errno!(EINVAL))?;
            current_task.set_command_name(name_str);
            Ok(0.into())
        }
        PR_GET_NAME => {
            let addr = UserAddress::from(arg2);
            current_task.write_memory(addr, current_task.command().to_bytes_with_nul())?;
            Ok(().into())
        }
        PR_SET_PTRACER => {
            let allowed_ptracers = if arg2 == PR_SET_PTRACER_ANY as u64 {
                PtraceAllowedPtracers::Any
            } else if arg2 == 0 {
                PtraceAllowedPtracers::None
            } else {
                if current_task.kernel().pids.read().get_task(arg2 as i32).upgrade().is_none() {
                    return error!(EINVAL);
                }
                PtraceAllowedPtracers::Some(arg2 as pid_t)
            };
            current_task.thread_group.write().allowed_ptracers = allowed_ptracers;
            Ok(().into())
        }
        PR_GET_KEEPCAPS => {
            Ok(current_task.creds().securebits.contains(SecureBits::KEEP_CAPS).into())
        }
        PR_SET_KEEPCAPS => {
            if arg2 != 0 && arg2 != 1 {
                return error!(EINVAL);
            }
            let mut creds = current_task.creds();
            creds.securebits.set(SecureBits::KEEP_CAPS, arg2 != 0);
            current_task.set_creds(creds);
            Ok(().into())
        }
        PR_SET_NO_NEW_PRIVS => {
            // If any args are set other than arg2 to 1, this should return einval
            if arg2 != 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return error!(EINVAL);
            }
            current_task.write().enable_no_new_privs();
            Ok(().into())
        }
        PR_GET_NO_NEW_PRIVS => {
            // If any args are set, this should return einval
            if arg2 != 0 || arg3 != 0 || arg4 != 0 {
                return error!(EINVAL);
            }
            Ok(current_task.read().no_new_privs().into())
        }
        PR_GET_SECCOMP => {
            if current_task.seccomp_filter_state.get() == SeccompStateValue::None {
                Ok(0.into())
            } else {
                Ok(2.into())
            }
        }
        PR_SET_SECCOMP => {
            if arg2 == SECCOMP_MODE_STRICT as u64 {
                return sys_seccomp(
                    locked,
                    current_task,
                    SECCOMP_SET_MODE_STRICT,
                    0,
                    UserAddress::NULL,
                );
            } else if arg2 == SECCOMP_MODE_FILTER as u64 {
                return sys_seccomp(locked, current_task, SECCOMP_SET_MODE_FILTER, 0, arg3.into());
            }
            Ok(().into())
        }
        PR_GET_CHILD_SUBREAPER => {
            let addr = UserAddress::from(arg2);
            #[allow(clippy::bool_to_int_with_if)]
            let value: i32 =
                if current_task.thread_group.read().is_child_subreaper { 1 } else { 0 };
            current_task.write_object(addr.into(), &value)?;
            Ok(().into())
        }
        PR_SET_CHILD_SUBREAPER => {
            current_task.thread_group.write().is_child_subreaper = arg2 != 0;
            Ok(().into())
        }
        PR_GET_SECUREBITS => {
            let value = current_task.creds().securebits.bits();
            Ok(value.into())
        }
        PR_SET_SECUREBITS => {
            // TODO(security): This does not yet respect locked flags.
            let mut creds = current_task.creds();
            security::check_task_capable(current_task, CAP_SETPCAP)?;

            let securebits = SecureBits::from_bits(arg2 as u32).ok_or_else(|| {
                track_stub!(TODO("https://fxbug.dev/322875244"), "PR_SET_SECUREBITS", arg2);
                errno!(ENOSYS)
            })?;
            creds.securebits = securebits;
            current_task.set_creds(creds);
            Ok(().into())
        }
        PR_CAPBSET_READ => {
            let has_cap = current_task.creds().cap_bounding.contains(Capabilities::try_from(arg2)?);
            Ok(has_cap.into())
        }
        PR_CAPBSET_DROP => {
            let mut creds = current_task.creds();
            security::check_task_capable(current_task, CAP_SETPCAP)?;

            creds.cap_bounding.remove(Capabilities::try_from(arg2)?);
            current_task.set_creds(creds);
            Ok(().into())
        }
        PR_CAP_AMBIENT => {
            let operation = arg2 as u32;
            let capability_arg = Capabilities::try_from(arg3)?;
            if arg4 != 0 || arg5 != 0 {
                return error!(EINVAL);
            }

            // TODO(security): We don't currently validate capabilities, but this should return an
            // error if the capability_arg is invalid.
            match operation {
                PR_CAP_AMBIENT_RAISE => {
                    let mut creds = current_task.creds();
                    if !(creds.cap_permitted.contains(capability_arg)
                        && creds.cap_inheritable.contains(capability_arg))
                    {
                        return error!(EPERM);
                    }
                    if creds.securebits.contains(SecureBits::NO_CAP_AMBIENT_RAISE)
                        || creds.securebits.contains(SecureBits::NO_CAP_AMBIENT_RAISE_LOCKED)
                    {
                        return error!(EPERM);
                    }

                    creds.cap_ambient.insert(capability_arg);
                    current_task.set_creds(creds);
                    Ok(().into())
                }
                PR_CAP_AMBIENT_LOWER => {
                    let mut creds = current_task.creds();
                    creds.cap_ambient.remove(capability_arg);
                    current_task.set_creds(creds);
                    Ok(().into())
                }
                PR_CAP_AMBIENT_IS_SET => {
                    let has_cap = current_task.creds().cap_ambient.contains(capability_arg);
                    Ok(has_cap.into())
                }
                PR_CAP_AMBIENT_CLEAR_ALL => {
                    if arg3 != 0 {
                        return error!(EINVAL);
                    }

                    let mut creds = current_task.creds();
                    creds.cap_ambient = Capabilities::empty();
                    current_task.set_creds(creds);
                    Ok(().into())
                }
                _ => error!(EINVAL),
            }
        }
        PR_SET_TIMERSLACK => {
            current_task.write().set_timerslack_ns(arg2);
            Ok(().into())
        }
        _ => {
            track_stub!(TODO("https://fxbug.dev/322874733"), "prctl fallthrough", option);
            error!(ENOSYS)
        }
    }
}

pub fn sys_ptrace(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    request: u32,
    pid: pid_t,
    addr: UserAddress,
    data: UserAddress,
) -> Result<SyscallResult, Errno> {
    match request {
        PTRACE_TRACEME => ptrace_traceme(current_task),
        PTRACE_ATTACH => ptrace_attach(locked, current_task, pid, PtraceAttachType::Attach, data),
        PTRACE_SEIZE => ptrace_attach(locked, current_task, pid, PtraceAttachType::Seize, data),
        _ => ptrace_dispatch(current_task, request, pid, addr, data),
    }
}

pub fn sys_set_tid_address(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_tid: UserRef<pid_t>,
) -> Result<pid_t, Errno> {
    current_task.write().clear_child_tid = user_tid;
    Ok(current_task.get_tid())
}

pub fn sys_getrusage(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    who: i32,
    user_usage: UserRef<rusage>,
) -> Result<(), Errno> {
    const RUSAGE_SELF: i32 = starnix_uapi::uapi::RUSAGE_SELF as i32;
    const RUSAGE_THREAD: i32 = starnix_uapi::uapi::RUSAGE_THREAD as i32;
    track_stub!(TODO("https://fxbug.dev/297370242"), "real rusage");
    let time_stats = match who {
        RUSAGE_CHILDREN => current_task.task.thread_group.read().children_time_stats,
        RUSAGE_SELF => current_task.task.thread_group.time_stats(),
        RUSAGE_THREAD => current_task.task.time_stats(),
        _ => return error!(EINVAL),
    };

    let usage = rusage {
        ru_utime: timeval_from_duration(time_stats.user_time),
        ru_stime: timeval_from_duration(time_stats.system_time),
        ..rusage::default()
    };
    current_task.write_object(user_usage, &usage)?;

    Ok(())
}

type PrLimitRef = MultiArchUserRef<uapi::rlimit, uapi::arch32::rlimit>;

pub fn sys_getrlimit(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    resource: u32,
    user_rlimit: PrLimitRef,
) -> Result<(), Errno> {
    do_prlimit64(locked, current_task, 0, resource, Default::default(), user_rlimit)
}

pub fn sys_setrlimit(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    resource: u32,
    user_rlimit: PrLimitRef,
) -> Result<(), Errno> {
    do_prlimit64(locked, current_task, 0, resource, user_rlimit, Default::default())
}

pub fn sys_prlimit64(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    user_resource: u32,
    new_limit_ref: UserRef<uapi::rlimit>,
    old_limit_ref: UserRef<uapi::rlimit>,
) -> Result<(), Errno> {
    do_prlimit64::<uapi::rlimit>(
        locked,
        current_task,
        pid,
        user_resource,
        new_limit_ref.into(),
        old_limit_ref.into(),
    )
}

pub fn do_prlimit64<T>(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    user_resource: u32,
    new_limit_ref: MultiArchUserRef<uapi::rlimit, T>,
    old_limit_ref: MultiArchUserRef<uapi::rlimit, T>,
) -> Result<(), Errno>
where
    T: FromBytes + IntoBytes + Immutable + From<uapi::rlimit> + Into<uapi::rlimit>,
{
    let weak = get_task_or_current(current_task, pid);
    let target_task = Task::from_weak(&weak)?;

    // To get or set the resource of a process other than itself, the caller must have either:
    // * the CAP_SYS_RESOURCE
    // * the same `uid`, `euid`, `saved_uid`, `gid`, `egid`, `saved_gid` as the target.
    if current_task.get_pid() != target_task.get_pid() {
        let current_creds = current_task.creds();
        if !current_creds.has_capability(CAP_SYS_RESOURCE) {
            let target_creds = target_task.creds();
            let creds_match = current_creds.uid == target_creds.uid
                && current_creds.euid == target_creds.euid
                && current_creds.saved_uid == target_creds.saved_uid
                && current_creds.gid == target_creds.gid
                && current_creds.egid == target_creds.egid
                && current_creds.saved_gid == target_creds.saved_gid;
            if !creds_match {
                return error!(EPERM);
            }
        }
        security::task_prlimit(
            current_task,
            &target_task,
            !old_limit_ref.is_null(),
            !new_limit_ref.is_null(),
        )?;
    }

    let resource = Resource::from_raw(user_resource)?;

    let old_limit = match resource {
        // TODO: Integrate Resource::STACK with generic ResourceLimits machinery.
        Resource::STACK => {
            if !new_limit_ref.is_null() {
                track_stub!(
                    TODO("https://fxbug.dev/322874791"),
                    "prlimit64 cannot set RLIMIT_STACK"
                );
            }
            // The stack size is fixed at the moment, but
            // if MAP_GROWSDOWN is implemented this should
            // report the limit that it can be grown.
            let mm_state = target_task.mm().ok_or_else(|| errno!(EINVAL))?.state.read();
            let stack_size = mm_state.stack_size as u64;
            rlimit { rlim_cur: stack_size, rlim_max: stack_size }
        }
        _ => {
            let new_limit = if new_limit_ref.is_null() {
                None
            } else {
                let new_limit = current_task.read_multi_arch_object(new_limit_ref)?;
                if new_limit.rlim_cur > new_limit.rlim_max {
                    return error!(EINVAL);
                }
                Some(new_limit)
            };
            target_task.thread_group.adjust_rlimits(current_task, resource, new_limit)?
        }
    };
    if !old_limit_ref.is_null() {
        current_task.write_multi_arch_object(old_limit_ref, old_limit)?;
    }
    Ok(())
}

pub fn sys_quotactl(
    _locked: &mut Locked<'_, Unlocked>,
    _current_task: &CurrentTask,
    _cmd: i32,
    _special: UserRef<c_char>,
    _id: i32,
    _addr: UserRef<c_char>,
) -> Result<SyscallResult, Errno> {
    track_stub!(TODO("https://fxbug.dev/297302197"), "quotacl()");
    error!(ENOSYS)
}

pub fn sys_capget(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_header: UserRef<__user_cap_header_struct>,
    user_data: UserRef<__user_cap_data_struct>,
) -> Result<(), Errno> {
    if user_data.is_null() {
        current_task.write_object(
            user_header,
            &__user_cap_header_struct { version: _LINUX_CAPABILITY_VERSION_3, pid: 0 },
        )?;
        return Ok(());
    }

    let mut header = current_task.read_object(user_header)?;
    if header.pid < 0 {
        return error!(EINVAL);
    }
    let weak = get_task_or_current(current_task, header.pid);
    let target_task = Task::from_weak(&weak)?;

    let (permitted, effective, inheritable) = {
        let creds = &target_task.creds();
        (creds.cap_permitted, creds.cap_effective, creds.cap_inheritable)
    };

    match header.version {
        _LINUX_CAPABILITY_VERSION_1 => {
            let data: [__user_cap_data_struct; 1] = [__user_cap_data_struct {
                effective: effective.as_abi_v1(),
                inheritable: inheritable.as_abi_v1(),
                permitted: permitted.as_abi_v1(),
            }];
            current_task.write_objects(user_data, &data)?;
        }
        _LINUX_CAPABILITY_VERSION_2 | _LINUX_CAPABILITY_VERSION_3 => {
            // Return 64 bit capabilities as two sets of 32 bit capabilities, little endian
            let (permitted, effective, inheritable) =
                (permitted.as_abi_v3(), effective.as_abi_v3(), inheritable.as_abi_v3());
            let data: [__user_cap_data_struct; 2] = [
                __user_cap_data_struct {
                    effective: effective.0,
                    inheritable: inheritable.0,
                    permitted: permitted.0,
                },
                __user_cap_data_struct {
                    effective: effective.1,
                    inheritable: inheritable.1,
                    permitted: permitted.1,
                },
            ];
            current_task.write_objects(user_data, &data)?;
        }
        _ => {
            header.version = _LINUX_CAPABILITY_VERSION_3;
            current_task.write_object(user_header, &header)?;
            return error!(EINVAL);
        }
    }
    Ok(())
}

pub fn sys_capset(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_header: UserRef<__user_cap_header_struct>,
    user_data: UserRef<__user_cap_data_struct>,
) -> Result<(), Errno> {
    let mut header = current_task.read_object(user_header)?;
    if header.pid != 0 && header.pid != current_task.id {
        return error!(EPERM);
    }
    let weak = get_task_or_current(current_task, header.pid);
    let target_task = Task::from_weak(&weak)?;

    let (new_permitted, new_effective, new_inheritable) = match header.version {
        _LINUX_CAPABILITY_VERSION_1 => {
            let data = current_task.read_object(user_data)?;
            (
                Capabilities::from_abi_v1(data.permitted),
                Capabilities::from_abi_v1(data.effective),
                Capabilities::from_abi_v1(data.inheritable),
            )
        }
        _LINUX_CAPABILITY_VERSION_2 | _LINUX_CAPABILITY_VERSION_3 => {
            let data =
                current_task.read_objects_to_array::<__user_cap_data_struct, 2>(user_data)?;
            (
                Capabilities::from_abi_v3((data[0].permitted, data[1].permitted)),
                Capabilities::from_abi_v3((data[0].effective, data[1].effective)),
                Capabilities::from_abi_v3((data[0].inheritable, data[1].inheritable)),
            )
        }
        _ => {
            header.version = _LINUX_CAPABILITY_VERSION_3;
            current_task.write_object(user_header, &header)?;
            return error!(EINVAL);
        }
    };

    // Permission checks. Copied out of TLPI section 39.7.
    let mut creds = target_task.creds();
    {
        log_trace!("Capabilities({{permitted={:?} from {:?}, effective={:?} from {:?}, inheritable={:?} from {:?}}}, bounding={:?})", new_permitted, creds.cap_permitted, new_effective, creds.cap_effective, new_inheritable, creds.cap_inheritable, creds.cap_bounding);
        if !creds.cap_inheritable.union(creds.cap_permitted).contains(new_inheritable) {
            security::check_task_capable(current_task, CAP_SETPCAP)?;
        }

        if !creds.cap_inheritable.union(creds.cap_bounding).contains(new_inheritable) {
            return error!(EPERM);
        }
        if !creds.cap_permitted.contains(new_permitted) {
            return error!(EPERM);
        }
        if !new_permitted.contains(new_effective) {
            return error!(EPERM);
        }
    }

    creds.cap_permitted = new_permitted;
    creds.cap_effective = new_effective;
    creds.cap_inheritable = new_inheritable;
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_seccomp(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    operation: u32,
    flags: u32,
    args: UserAddress,
) -> Result<SyscallResult, Errno> {
    match operation {
        SECCOMP_SET_MODE_STRICT => {
            if flags != 0 || args != UserAddress::NULL {
                return error!(EINVAL);
            }
            current_task.set_seccomp_state(SeccompStateValue::Strict)?;
            Ok(().into())
        }
        SECCOMP_SET_MODE_FILTER => {
            if flags
                & (SECCOMP_FILTER_FLAG_LOG
                    | SECCOMP_FILTER_FLAG_NEW_LISTENER
                    | SECCOMP_FILTER_FLAG_SPEC_ALLOW
                    | SECCOMP_FILTER_FLAG_TSYNC
                    | SECCOMP_FILTER_FLAG_TSYNC_ESRCH)
                != flags
            {
                return error!(EINVAL);
            }
            if (flags & SECCOMP_FILTER_FLAG_NEW_LISTENER != 0)
                && (flags & SECCOMP_FILTER_FLAG_TSYNC != 0)
                && (flags & SECCOMP_FILTER_FLAG_TSYNC_ESRCH == 0)
            {
                return error!(EINVAL);
            }
            let fprog: sock_fprog = current_task.read_object(UserRef::new(args))?;
            if u32::from(fprog.len) > BPF_MAXINSNS || fprog.len == 0 {
                return error!(EINVAL);
            }
            let code: Vec<sock_filter> =
                current_task.read_objects_to_vec(fprog.filter.into(), fprog.len as usize)?;

            if !current_task.read().no_new_privs()
                && !current_task.creds().has_capability(CAP_SYS_ADMIN)
            {
                return error!(EACCES);
            }
            current_task.add_seccomp_filter(code, flags)
        }
        SECCOMP_GET_ACTION_AVAIL => {
            if flags != 0 || args.is_null() {
                return error!(EINVAL);
            }
            let action: u32 = current_task.read_object(UserRef::new(args))?;
            SeccompAction::is_action_available(action)
        }
        SECCOMP_GET_NOTIF_SIZES => {
            if flags != 0 {
                return error!(EINVAL);
            }
            track_stub!(TODO("https://fxbug.dev/322874791"), "SECCOMP_GET_NOTIF_SIZES");
            error!(ENOSYS)
        }
        _ => {
            track_stub!(TODO("https://fxbug.dev/322874916"), "seccomp fallthrough", operation);
            error!(EINVAL)
        }
    }
}

pub fn sys_setgroups(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    size: usize,
    groups_addr: UserAddress,
) -> Result<(), Errno> {
    if size > NGROUPS_MAX as usize {
        return error!(EINVAL);
    }
    let groups = current_task.read_objects_to_vec::<gid_t>(groups_addr.into(), size)?;
    let mut creds = current_task.creds();
    if !creds.is_superuser() {
        return error!(EPERM);
    }
    creds.groups = groups;
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_getgroups(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    size: usize,
    groups_addr: UserAddress,
) -> Result<usize, Errno> {
    if size > NGROUPS_MAX as usize {
        return error!(EINVAL);
    }
    let creds = current_task.creds();
    if size != 0 {
        if size < creds.groups.len() {
            return error!(EINVAL);
        }
        current_task.write_memory(groups_addr, creds.groups.as_slice().as_bytes())?;
    }
    Ok(creds.groups.len())
}

pub fn sys_setsid(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<pid_t, Errno> {
    current_task.thread_group.setsid(locked)?;
    Ok(current_task.get_pid())
}

pub fn sys_getpriority(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    which: u32,
    who: i32,
) -> Result<u8, Errno> {
    match which {
        PRIO_PROCESS => {}
        _ => return error!(EINVAL),
    }
    track_stub!(TODO("https://fxbug.dev/322893809"), "getpriority permissions");
    let weak = get_task_or_current(current_task, who);
    let target_task = Task::from_weak(&weak)?;
    let state = target_task.read();
    Ok(state.scheduler_policy.raw_priority())
}

pub fn sys_setpriority(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    which: u32,
    who: i32,
    priority: i32,
) -> Result<(), Errno> {
    match which {
        PRIO_PROCESS => {}
        _ => return error!(EINVAL),
    }
    track_stub!(TODO("https://fxbug.dev/322894197"), "setpriority permissions");
    let weak = get_task_or_current(current_task, who);
    let target_task = Task::from_weak(&weak)?;
    security::check_setsched_access(current_task, &target_task)?;
    // The priority passed into setpriority is actually in the -19...20 range and is not
    // transformed into the 1...40 range. The man page is lying. (I sent a patch, so it might not
    // be lying anymore by the time you read this.)
    const MIN_PRIORITY: u8 = 1;
    const MID_PRIORITY: u8 = 20;
    const MAX_PRIORITY: u8 = 40;
    let priority = (MID_PRIORITY as i32).saturating_sub(priority);
    let max_priority =
        std::cmp::min(MAX_PRIORITY as u64, target_task.thread_group.get_rlimit(Resource::NICE));
    target_task
        .update_scheduler_nice(priority.clamp(MIN_PRIORITY as i32, max_priority as i32) as u8)?;
    Ok(())
}

pub fn sys_setns(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    ns_fd: FdNumber,
    ns_type: c_int,
) -> Result<(), Errno> {
    let file_handle = current_task.task.files.get(ns_fd)?;

    // From man pages this is not quite right because some namespace types require more capabilities
    // or require this capability in multiple namespaces, but it should cover our current test
    // cases and we can make this more nuanced once more namespace types are supported.
    security::check_task_capable(current_task, CAP_SYS_ADMIN)?;

    if let Some(mount_ns) = file_handle.downcast_file::<MountNamespaceFile>() {
        if !(ns_type == 0 || ns_type == CLONE_NEWNS as i32) {
            log_trace!("invalid type");
            return error!(EINVAL);
        }

        track_stub!(TODO("https://fxbug.dev/297312091"), "setns CLONE_FS limitations");
        current_task.task.fs().set_namespace(mount_ns.0.clone())?;
        return Ok(());
    }

    if let Some(_pidfd) = file_handle.downcast_file::<PidFdFileObject>() {
        track_stub!(TODO("https://fxbug.dev/297312844"), "setns w/ pidfd");
        return error!(ENOSYS);
    }

    track_stub!(TODO("https://fxbug.dev/322893829"), "unknown ns file for setns, see logs");
    log_info!("ns_fd was not a supported namespace file: {}", file_handle.ops_type_name());
    error!(EINVAL)
}

pub fn sys_unshare(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    flags: u32,
) -> Result<(), Errno> {
    const IMPLEMENTED_FLAGS: u32 = CLONE_FILES | CLONE_FS | CLONE_NEWNS | CLONE_NEWUTS;
    if flags & !IMPLEMENTED_FLAGS != 0 {
        track_stub!(TODO("https://fxbug.dev/322893372"), "unshare", flags & !IMPLEMENTED_FLAGS);
        return error!(EINVAL);
    }

    if (flags & CLONE_FILES) != 0 {
        current_task.files.unshare();
    }

    if (flags & CLONE_FS) != 0 {
        current_task.unshare_fs();
    }

    if (flags & CLONE_NEWNS) != 0 {
        security::check_task_capable(current_task, CAP_SYS_ADMIN)?;
        current_task.fs().unshare_namespace();
    }

    if (flags & CLONE_NEWUTS) != 0 {
        security::check_task_capable(current_task, CAP_SYS_ADMIN)?;
        // Fork the UTS namespace.
        let mut task_state = current_task.write();
        let new_uts_ns = task_state.uts_ns.read().clone();
        task_state.uts_ns = Arc::new(RwLock::new(new_uts_ns));
    }

    Ok(())
}

pub fn sys_swapon(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
    _flags: i32,
) -> Result<(), Errno> {
    const MAX_SWAPFILES: usize = 30; // See https://man7.org/linux/man-pages/man2/swapon.2.html

    security::check_task_capable(current_task, CAP_SYS_ADMIN)?;

    track_stub!(TODO("https://fxbug.dev/322893905"), "swapon validate flags");

    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;
    let file = current_task.open_file(locked, path.as_ref(), OpenFlags::RDWR)?;

    let node = file.node();
    let mode = node.info().mode;
    if !mode.is_reg() && !mode.is_blk() {
        return error!(EINVAL);
    }

    // We determined this magic number by using the mkswap tool and the file tool. The mkswap tool
    // populates a few bytes in the file, including a UUID, which can be replaced with zeros while
    // still being recognized by the file tool. This string appears at a fixed offset
    // (MAGIC_OFFSET) in the file, which looks quite like a magic number.
    const MAGIC_OFFSET: usize = 0xff6;
    let swap_magic = b"SWAPSPACE2";
    let mut buffer = VecOutputBuffer::new(swap_magic.len());
    if file.read_at(locked, current_task, MAGIC_OFFSET, &mut buffer)? != swap_magic.len()
        || buffer.data() != swap_magic
    {
        return error!(EINVAL);
    }

    let mut swap_files = current_task.kernel().swap_files.lock(locked);
    for swap_file in swap_files.iter() {
        if Arc::ptr_eq(swap_file.node(), file.node()) {
            return error!(EBUSY);
        }
    }
    if swap_files.len() >= MAX_SWAPFILES {
        return error!(EPERM);
    }
    swap_files.push(file);
    Ok(())
}

pub fn sys_swapoff(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_path: UserCString,
) -> Result<(), Errno> {
    security::check_task_capable(current_task, CAP_SYS_ADMIN)?;

    let path = current_task.read_c_string_to_vec(user_path, PATH_MAX as usize)?;
    let file = current_task.open_file(locked, path.as_ref(), OpenFlags::RDWR)?;

    let mut swap_files = current_task.kernel().swap_files.lock(locked);
    let original_length = swap_files.len();
    swap_files.retain(|swap_file| !Arc::ptr_eq(swap_file.node(), file.node()));
    if swap_files.len() == original_length {
        return error!(EINVAL);
    }
    Ok(())
}

#[derive(Default, Debug, IntoBytes, KnownLayout, FromBytes, Immutable)]
#[repr(C)]
struct KcmpParams {
    mask: usize,
    shuffle: usize,
}

static KCMP_PARAMS: LazyLock<KcmpParams> = LazyLock::new(|| {
    let mut params = KcmpParams::default();
    zx::cprng_draw(params.as_mut_bytes());
    // Ensure the shuffle is odd so that multiplying a usize by this value is a permutation.
    params.shuffle |= 1;
    params
});

fn obfuscate_value(value: usize) -> usize {
    let KcmpParams { mask, shuffle } = *KCMP_PARAMS;
    (value ^ mask).wrapping_mul(shuffle)
}

fn obfuscate_ptr<T>(ptr: *const T) -> usize {
    obfuscate_value(ptr as usize)
}

fn obfuscate_arc<T>(arc: &Arc<T>) -> usize {
    obfuscate_ptr(Arc::as_ptr(arc))
}

pub fn sys_kcmp(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid1: pid_t,
    pid2: pid_t,
    resource_type: u32,
    index1: u64,
    index2: u64,
) -> Result<u32, Errno> {
    let weak1 = get_task_if_owner_or_has_capabilities(current_task, pid1, CAP_SYS_PTRACE)?;
    let task1 = Task::from_weak(&weak1)?;
    let weak2 = get_task_if_owner_or_has_capabilities(current_task, pid2, CAP_SYS_PTRACE)?;
    let task2 = Task::from_weak(&weak2)?;
    let resource_type = KcmpResource::from_raw(resource_type)?;

    // Output encoding (see <https://man7.org/linux/man-pages/man2/kcmp.2.html>):
    //
    //   0  v1 is equal to v2; in other words, the two processes share the resource.
    //   1  v1 is less than v2.
    //   2  v1 is greater than v2.
    //   3  v1 is not equal to v2, but ordering information is unavailable.
    //
    fn encode_ordering(value: cmp::Ordering) -> u32 {
        match value {
            cmp::Ordering::Equal => 0,
            cmp::Ordering::Less => 1,
            cmp::Ordering::Greater => 2,
        }
    }

    match resource_type {
        KcmpResource::FILE => {
            fn get_file(task: &Task, index: u64) -> Result<FileHandle, Errno> {
                // TODO: Test whether O_PATH is allowed here. Conceptually, seems like
                //       O_PATH should be allowed, but we haven't tested it yet.
                task.files.get_allowing_opath(FdNumber::from_raw(
                    index.try_into().map_err(|_| errno!(EBADF))?,
                ))
            }
            let file1 = get_file(&task1, index1)?;
            let file2 = get_file(&task2, index2)?;
            Ok(encode_ordering(obfuscate_arc(&file1).cmp(&obfuscate_arc(&file2))))
        }
        KcmpResource::FILES => Ok(encode_ordering(
            obfuscate_value(task1.files.id().raw()).cmp(&obfuscate_value(task2.files.id().raw())),
        )),
        KcmpResource::FS => {
            Ok(encode_ordering(obfuscate_arc(&task1.fs()).cmp(&obfuscate_arc(&task2.fs()))))
        }
        KcmpResource::SIGHAND => Ok(encode_ordering(
            obfuscate_arc(&task1.thread_group.signal_actions)
                .cmp(&obfuscate_arc(&task2.thread_group.signal_actions)),
        )),
        KcmpResource::VM => Ok(encode_ordering(
            obfuscate_arc(task1.mm().ok_or_else(|| errno!(EINVAL))?)
                .cmp(&obfuscate_arc(task2.mm().ok_or_else(|| errno!(EINVAL))?)),
        )),
        _ => error!(EINVAL),
    }
}

pub fn sys_syslog(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    action_type: i32,
    address: UserAddress,
    length: i32,
) -> Result<i32, Errno> {
    let action = SyslogAction::try_from(action_type)?;
    let syslog = current_task.kernel().syslog.access(&current_task)?;
    match action {
        SyslogAction::Read => {
            if address.is_null() || length < 0 {
                return error!(EINVAL);
            }
            let mut output_buffer =
                UserBuffersOutputBuffer::unified_new_at(current_task, address, length as usize)?;
            syslog.blocking_read(locked, current_task, &mut output_buffer)
        }
        SyslogAction::ReadAll => {
            if address.is_null() || length < 0 {
                return error!(EINVAL);
            }
            let mut output_buffer =
                UserBuffersOutputBuffer::unified_new_at(current_task, address, length as usize)?;
            syslog.read_all(&mut output_buffer)
        }
        SyslogAction::SizeUnread => syslog.size_unread(),
        SyslogAction::SizeBuffer => syslog.size_buffer(),
        SyslogAction::Close | SyslogAction::Open => Ok(0),
        SyslogAction::ReadClear => {
            track_stub!(TODO("https://fxbug.dev/322894145"), "syslog: read clear");
            Ok(0)
        }
        SyslogAction::Clear => {
            track_stub!(TODO("https://fxbug.dev/322893673"), "syslog: clear");
            Ok(0)
        }
        SyslogAction::ConsoleOff => {
            track_stub!(TODO("https://fxbug.dev/322894399"), "syslog: console off");
            Ok(0)
        }
        SyslogAction::ConsoleOn => {
            track_stub!(TODO("https://fxbug.dev/322894106"), "syslog: console on");
            Ok(0)
        }
        SyslogAction::ConsoleLevel => {
            if length <= 0 || length >= 8 {
                return error!(EINVAL);
            }
            track_stub!(TODO("https://fxbug.dev/322894199"), "syslog: console level");
            Ok(0)
        }
    }
}

pub fn sys_vhangup(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
) -> Result<(), Errno> {
    security::check_task_capable(current_task, CAP_SYS_TTY_CONFIG)?;
    track_stub!(TODO("https://fxbug.dev/324079257"), "vhangup");
    Ok(())
}

// Syscalls for arch32 usage
#[cfg(feature = "arch32")]
mod arch32 {
    pub use super::{
        sys_getrlimit as sys_arch32_ugetrlimit, sys_setrlimit as sys_arch32_setrlimit,
    };
}

#[cfg(feature = "arch32")]
pub use arch32::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::syscalls::sys_munmap;
    use crate::testing::*;
    use starnix_syscalls::SUCCESS;
    use starnix_uapi::{SCHED_FIFO, SCHED_NORMAL};
    use std::{mem, u64};

    #[::fuchsia::test]
    async fn test_prctl_set_vma_anon_name() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let name_addr = mapped_address + 128u64;
        let name = "test-name\0";
        current_task.write_memory(name_addr, name.as_bytes()).expect("failed to write name");
        sys_prctl(
            &mut locked,
            &mut current_task,
            PR_SET_VMA,
            PR_SET_VMA_ANON_NAME as u64,
            mapped_address.ptr() as u64,
            32,
            name_addr.ptr() as u64,
        )
        .expect("failed to set name");
        assert_eq!(
            Some("test-name".into()),
            current_task
                .mm()
                .unwrap()
                .get_mapping_name(mapped_address + 24u64)
                .expect("failed to get address")
        );

        sys_munmap(&mut locked, &current_task, mapped_address, *PAGE_SIZE as usize)
            .expect("failed to unmap memory");
        assert_eq!(
            error!(EFAULT),
            current_task.mm().unwrap().get_mapping_name(mapped_address + 24u64)
        );
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_special_chars() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        for c in 1..255 {
            let vma_name = CString::new([c]).unwrap();
            current_task.write_memory(name_addr, vma_name.as_bytes_with_nul()).unwrap();

            let result = sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                *PAGE_SIZE,
                name_addr.ptr() as u64,
            );

            if c > 0x1f
                && c < 0x7f
                && c != b'\\'
                && c != b'`'
                && c != b'$'
                && c != b'['
                && c != b']'
            {
                assert_eq!(result, Ok(SUCCESS));
            } else {
                assert_eq!(result, Err(errno!(EINVAL)));
            }
        }
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_long() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let name_too_long = CString::new(vec![b'a'; 256]).unwrap();

        current_task.write_memory(name_addr, name_too_long.as_bytes_with_nul()).unwrap();

        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Err(errno!(EINVAL))
        );

        let name_just_long_enough = CString::new(vec![b'a'; 255]).unwrap();

        current_task.write_memory(name_addr, name_just_long_enough.as_bytes_with_nul()).unwrap();

        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Ok(SUCCESS)
        );
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_misaligned() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();
        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let name = CString::new("name").unwrap();
        current_task.write_memory(name_addr, name.as_bytes_with_nul()).unwrap();

        // Passing a misaligned pointer to the start of the named region fails.
        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                1 + mapping_addr.ptr() as u64,
                *PAGE_SIZE - 1,
                name_addr.ptr() as u64,
            ),
            Err(errno!(EINVAL))
        );

        // Passing an unaligned length does work, however.
        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                *PAGE_SIZE - 1,
                name_addr.ptr() as u64,
            ),
            Ok(SUCCESS)
        );
    }

    #[::fuchsia::test]
    async fn test_prctl_get_set_dumpable() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        sys_prctl(&mut locked, &mut current_task, PR_GET_DUMPABLE, 0, 0, 0, 0)
            .expect("failed to get dumpable");

        sys_prctl(&mut locked, &mut current_task, PR_SET_DUMPABLE, 1, 0, 0, 0)
            .expect("failed to set dumpable");
        sys_prctl(&mut locked, &mut current_task, PR_GET_DUMPABLE, 0, 0, 0, 0)
            .expect("failed to get dumpable");

        // SUID_DUMP_ROOT not supported.
        sys_prctl(&mut locked, &mut current_task, PR_SET_DUMPABLE, 2, 0, 0, 0)
            .expect("failed to set dumpable");
        sys_prctl(&mut locked, &mut current_task, PR_GET_DUMPABLE, 0, 0, 0, 0)
            .expect("failed to get dumpable");
    }

    #[::fuchsia::test]
    async fn test_sys_getsid() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        assert_eq!(
            current_task.get_tid(),
            sys_getsid(&mut locked, &current_task, 0).expect("failed to get sid")
        );

        let second_current = create_task(&mut locked, &kernel, "second task");

        assert_eq!(
            second_current.get_tid(),
            sys_getsid(&mut locked, &current_task, second_current.get_tid())
                .expect("failed to get sid")
        );
    }

    #[::fuchsia::test]
    async fn test_get_affinity_size() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let pid = current_task.get_pid();
        assert_eq!(
            sys_sched_getaffinity(&mut locked, &current_task, pid, 16, mapped_address),
            Ok(std::mem::size_of::<u64>())
        );
        assert_eq!(
            sys_sched_getaffinity(&mut locked, &current_task, pid, 1, mapped_address),
            error!(EINVAL)
        );
        assert_eq!(
            sys_sched_getaffinity(&mut locked, &current_task, pid, 9, mapped_address),
            error!(EINVAL)
        );
    }

    #[::fuchsia::test]
    async fn test_set_affinity_size() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task.write_memory(mapped_address, &[0xffu8]).expect("failed to cpumask");
        let pid = current_task.get_pid();
        assert_eq!(
            sys_sched_setaffinity(&mut locked, &current_task, pid, u32::MAX, mapped_address),
            Ok(())
        );
        assert_eq!(
            sys_sched_setaffinity(&mut locked, &current_task, pid, 1, mapped_address),
            error!(EINVAL)
        );
    }

    #[::fuchsia::test]
    async fn test_task_name() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();
        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let name = "my-task-name\0";
        current_task.write_memory(mapped_address, name.as_bytes()).expect("failed to write name");

        let result = sys_prctl(
            &mut locked,
            &mut current_task,
            PR_SET_NAME,
            mapped_address.ptr() as u64,
            0,
            0,
            0,
        )
        .unwrap();
        assert_eq!(SUCCESS, result);

        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let result = sys_prctl(
            &mut locked,
            &mut current_task,
            PR_GET_NAME,
            mapped_address.ptr() as u64,
            0,
            0,
            0,
        )
        .unwrap();
        assert_eq!(SUCCESS, result);

        let name_length = name.len();

        let out_name = current_task.read_memory_to_vec(mapped_address, name_length).unwrap();
        assert_eq!(name.as_bytes(), &out_name);
    }

    #[::fuchsia::test]
    async fn test_sched_get_priority_min_max() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let non_rt_min =
            sys_sched_get_priority_min(&mut locked, &current_task, SCHED_NORMAL).unwrap();
        assert_eq!(non_rt_min, 0);
        let non_rt_max =
            sys_sched_get_priority_max(&mut locked, &current_task, SCHED_NORMAL).unwrap();
        assert_eq!(non_rt_max, 0);

        let rt_min = sys_sched_get_priority_min(&mut locked, &current_task, SCHED_FIFO).unwrap();
        assert_eq!(rt_min, 1);
        let rt_max = sys_sched_get_priority_max(&mut locked, &current_task, SCHED_FIFO).unwrap();
        assert_eq!(rt_max, 99);

        let min_bad_policy_error =
            sys_sched_get_priority_min(&mut locked, &current_task, std::u32::MAX).unwrap_err();
        assert_eq!(min_bad_policy_error, errno!(EINVAL));

        let max_bad_policy_error =
            sys_sched_get_priority_max(&mut locked, &current_task, std::u32::MAX).unwrap_err();
        assert_eq!(max_bad_policy_error, errno!(EINVAL));
    }

    #[::fuchsia::test]
    async fn test_sched_setscheduler() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        current_task
            .thread_group
            .limits
            .lock()
            .set(Resource::RTPRIO, rlimit { rlim_cur: 255, rlim_max: 255 });

        let scheduler = sys_sched_getscheduler(&mut locked, &current_task, 0).unwrap();
        assert_eq!(scheduler, SCHED_NORMAL, "tasks should have normal scheduler by default");

        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let requested_params = sched_param { sched_priority: 15 };
        current_task.write_object(mapped_address.into(), &requested_params).unwrap();

        sys_sched_setscheduler(&mut locked, &current_task, 0, SCHED_FIFO, mapped_address).unwrap();

        let new_scheduler = sys_sched_getscheduler(&mut locked, &current_task, 0).unwrap();
        assert_eq!(new_scheduler, SCHED_FIFO, "task should have been assigned fifo scheduler");

        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        sys_sched_getparam(&mut locked, &current_task, 0, mapped_address).expect("sched_getparam");
        let param_value: sched_param =
            current_task.read_object(mapped_address.into()).expect("read_object");
        assert_eq!(param_value.sched_priority, 15);
    }

    #[::fuchsia::test]
    async fn test_sched_getparam() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mapped_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        sys_sched_getparam(&mut locked, &current_task, 0, mapped_address).expect("sched_getparam");
        let param_value: sched_param =
            current_task.read_object(mapped_address.into()).expect("read_object");
        assert_eq!(param_value.sched_priority, 0);
    }

    #[::fuchsia::test]
    async fn test_setuid() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        // Test for root.
        current_task.set_creds(Credentials::root());
        sys_setuid(&mut locked, &current_task, 42).expect("setuid");
        let mut creds = current_task.creds();
        assert_eq!(creds.euid, 42);
        assert_eq!(creds.uid, 42);
        assert_eq!(creds.saved_uid, 42);

        // Remove the CAP_SETUID capability to avoid overwriting permission checks.
        creds.cap_effective.remove(CAP_SETUID);
        current_task.set_creds(creds);

        // Test for non root, which task now is.
        assert_eq!(sys_setuid(&mut locked, &current_task, 0), error!(EPERM));
        assert_eq!(sys_setuid(&mut locked, &current_task, 43), error!(EPERM));

        sys_setuid(&mut locked, &current_task, 42).expect("setuid");
        let creds = current_task.creds();
        assert_eq!(creds.euid, 42);
        assert_eq!(creds.uid, 42);
        assert_eq!(creds.saved_uid, 42);

        // Change uid and saved_uid, and check that one can set the euid to these.
        let mut creds = current_task.creds();
        creds.uid = 41;
        creds.euid = 42;
        creds.saved_uid = 43;
        current_task.set_creds(creds);

        sys_setuid(&mut locked, &current_task, 41).expect("setuid");
        let creds = current_task.creds();
        assert_eq!(creds.euid, 41);
        assert_eq!(creds.uid, 41);
        assert_eq!(creds.saved_uid, 43);

        let mut creds = current_task.creds();
        creds.uid = 41;
        creds.euid = 42;
        creds.saved_uid = 43;
        current_task.set_creds(creds);

        sys_setuid(&mut locked, &current_task, 43).expect("setuid");
        let creds = current_task.creds();
        assert_eq!(creds.euid, 43);
        assert_eq!(creds.uid, 41);
        assert_eq!(creds.saved_uid, 43);
    }

    #[::fuchsia::test]
    async fn test_read_c_string_vector() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let arg_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let arg = b"test-arg\0";
        current_task.write_memory(arg_addr, arg).expect("failed to write test arg");
        let arg_usercstr = UserCString::new(arg_addr);
        let null_usercstr = UserCString::default();

        let argv_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_object(argv_addr.into(), &arg_usercstr)
            .expect("failed to write UserCString");
        current_task
            .write_object((argv_addr + mem::size_of::<UserCString>()).into(), &null_usercstr)
            .expect("failed to write UserCString");
        let argv_userref = UserRef::new(argv_addr);

        // The arguments size limit should include the null terminator.
        assert!(read_c_string_vector(&current_task, argv_userref, 100, arg.len()).is_ok());
        assert_eq!(
            read_c_string_vector(
                &current_task,
                argv_userref,
                100,
                std::str::from_utf8(arg).unwrap().trim_matches('\0').len()
            ),
            error!(E2BIG)
        );
    }
}
