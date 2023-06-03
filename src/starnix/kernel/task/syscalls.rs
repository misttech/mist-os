// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::AsHandleRef;

use static_assertions::const_assert;
use std::ffi::CString;
use std::sync::Arc;
use zerocopy::AsBytes;

use crate::auth::{Credentials, SecureBits};
use crate::execution::*;
use crate::fs::*;
use crate::lock::RwLock;
use crate::logging::{log_error, log_trace};
use crate::mm::*;
use crate::syscalls::*;
use crate::task::*;

pub fn do_clone(current_task: &CurrentTask, args: &clone_args) -> Result<pid_t, Errno> {
    let child_exit_signal = if args.exit_signal == 0 {
        None
    } else {
        Some(Signal::try_from(UncheckedSignal::new(args.exit_signal))?)
    };

    let mut new_task = current_task.clone_task(
        args.flags,
        child_exit_signal,
        UserRef::<pid_t>::new(UserAddress::from(args.parent_tid)),
        UserRef::<pid_t>::new(UserAddress::from(args.child_tid)),
    )?;
    let tid = new_task.id;

    // Clone the registers, setting the result register to 0 for the return value from clone in the
    // cloned process.
    new_task.registers = current_task.registers;
    new_task.registers.set_return_register(0);

    if args.stack != 0 {
        // In clone() the `stack` argument points to the top of the stack, while in clone3()
        // `stack` points to the bottom of the stack. Therefore, in clone3() we need to add
        // `stack_size` to calculate the stack pointer. Note that in clone() `stack_size` is 0.
        new_task.registers.set_stack_pointer_register(args.stack.wrapping_add(args.stack_size));
    }
    if args.flags & (CLONE_SETTLS as u64) != 0 {
        new_task.registers.set_thread_pointer_register(args.tls);
    }

    let task_ref = new_task.task.clone(); // Keep reference for later waiting.

    execute_task(new_task, |_| {});

    if args.flags & (CLONE_VFORK as u64) != 0 {
        task_ref.wait_for_execve()?;
    }
    Ok(tid)
}

pub fn sys_clone3(
    current_task: &CurrentTask,
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

    let clone_args = current_task.mm.read_object_partial(user_clone_args, user_clone_args_size)?;
    do_clone(current_task, &clone_args)
}

fn read_c_string_vector(
    mm: &MemoryManager,
    user_vector: UserRef<UserCString>,
    buf: &mut [u8],
) -> Result<Vec<CString>, Errno> {
    let mut user_current = user_vector;
    let mut vector: Vec<CString> = vec![];
    loop {
        let user_string = mm.read_object(user_current)?;
        if user_string.is_null() {
            break;
        }
        let string = mm.read_c_string(user_string, buf)?;
        vector.push(CString::new(string).map_err(|_| errno!(EINVAL))?);
        user_current = user_current.next();
    }
    Ok(vector)
}

pub fn sys_execve(
    current_task: &mut CurrentTask,
    user_path: UserCString,
    user_argv: UserRef<UserCString>,
    user_environ: UserRef<UserCString>,
) -> Result<(), Errno> {
    sys_execveat(current_task, FdNumber::AT_FDCWD, user_path, user_argv, user_environ, 0)
}

pub fn sys_execveat(
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

    let mut buf = [0u8; PATH_MAX as usize];
    // TODO: What is the maximum size for an argument?
    let argv = if user_argv.is_null() {
        Vec::new()
    } else {
        read_c_string_vector(&current_task.mm, user_argv, &mut buf)?
    };
    let environ = if user_environ.is_null() {
        Vec::new()
    } else {
        read_c_string_vector(&current_task.mm, user_environ, &mut buf)?
    };

    let path = current_task.mm.read_c_string(user_path, &mut buf)?;

    log_trace!(
        "execveat({}, {}, argv={:?}, environ={:?}, flags={})",
        dir_fd,
        String::from_utf8_lossy(path),
        argv,
        environ,
        flags
    );

    let mut open_flags = OpenFlags::RDONLY;

    if flags & AT_SYMLINK_NOFOLLOW != 0 {
        open_flags |= OpenFlags::NOFOLLOW;
    }

    let executable = if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            // If AT_EMPTY_PATH is not set, this is an error.
            return error!(ENOENT);
        }

        let file = current_task.files.get(dir_fd)?;

        // We are forced to reopen the file with O_RDONLY to get access to the underlying VMO.
        // Note that we set `check_access` to false in the arguments in case the file mode does
        // not actually have the read permission bit.
        //
        // This can happen because a file could have --x--x--x mode permissions and then
        // be opened with O_PATH. Internally, the file operations would all be stubbed out
        // for that file, which is undesirable here.
        //
        // See https://man7.org/linux/man-pages/man3/fexecve.3.html#DESCRIPTION
        file.name.open(current_task, OpenFlags::RDONLY, false)?
    } else {
        current_task.open_file_at(dir_fd, path, open_flags, FileMode::default())?
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

    current_task.exec(executable, path, argv, environ)?;
    Ok(())
}

pub fn sys_getcpu(
    current_task: &CurrentTask,
    cpu_out: UserRef<u32>,
    node_out: UserRef<u32>,
) -> Result<(), Errno> {
    // TODO(https://fxbug.dev/76948) make this a real implementation
    let fake_cpu_and_node = std::u32::MAX;
    current_task.mm.write_object(cpu_out, &fake_cpu_and_node)?;
    current_task.mm.write_object(node_out, &fake_cpu_and_node)?;
    Ok(())
}

pub fn sys_getpid(current_task: &CurrentTask) -> Result<pid_t, Errno> {
    Ok(current_task.get_pid())
}

pub fn sys_gettid(current_task: &CurrentTask) -> Result<pid_t, Errno> {
    Ok(current_task.get_tid())
}

pub fn sys_getppid(current_task: &CurrentTask) -> Result<pid_t, Errno> {
    Ok(current_task.thread_group.read().get_ppid())
}

fn get_task_or_current(current_task: &CurrentTask, pid: pid_t) -> Result<Arc<Task>, Errno> {
    if pid == 0 {
        Ok(current_task.task_arc_clone())
    } else {
        current_task.get_task(pid).ok_or_else(|| errno!(ESRCH))
    }
}

pub fn sys_getsid(current_task: &CurrentTask, pid: pid_t) -> Result<pid_t, Errno> {
    Ok(get_task_or_current(current_task, pid)?.thread_group.read().process_group.session.leader)
}

pub fn sys_getpgid(current_task: &CurrentTask, pid: pid_t) -> Result<pid_t, Errno> {
    Ok(get_task_or_current(current_task, pid)?.thread_group.read().process_group.leader)
}

pub fn sys_setpgid(current_task: &CurrentTask, pid: pid_t, pgid: pid_t) -> Result<(), Errno> {
    let task = get_task_or_current(current_task, pid)?;
    current_task.thread_group.setpgid(&task, pgid)?;
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

pub fn sys_getuid(current_task: &CurrentTask) -> Result<uid_t, Errno> {
    Ok(current_task.creds().uid)
}

pub fn sys_getgid(current_task: &CurrentTask) -> Result<gid_t, Errno> {
    Ok(current_task.creds().gid)
}

pub fn sys_setuid(current_task: &CurrentTask, uid: uid_t) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    if uid == gid_t::MAX {
        return error!(EINVAL);
    }
    if !new_uid_allowed(&creds, uid) {
        return error!(EPERM);
    }

    let prev_uid = creds.uid;
    let prev_euid = creds.euid;
    let prev_saved_uid = creds.saved_uid;
    let has_cap_setuid = creds.has_capability(CAP_SETUID);
    creds.euid = uid;
    if has_cap_setuid {
        creds.uid = uid;
        creds.saved_uid = uid;
    }

    creds.update_capabilities(prev_uid, prev_euid, prev_saved_uid);
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setgid(current_task: &CurrentTask, gid: gid_t) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    if gid == gid_t::MAX {
        return error!(EINVAL);
    }
    if !new_gid_allowed(&creds, gid) {
        return error!(EPERM);
    }
    creds.egid = gid;
    if creds.has_capability(CAP_SETGID) {
        creds.gid = gid;
        creds.saved_gid = gid;
    }
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_geteuid(current_task: &CurrentTask) -> Result<uid_t, Errno> {
    Ok(current_task.creds().euid)
}

pub fn sys_getegid(current_task: &CurrentTask) -> Result<gid_t, Errno> {
    Ok(current_task.creds().egid)
}

pub fn sys_getresuid(
    current_task: &CurrentTask,
    ruid_addr: UserRef<uid_t>,
    euid_addr: UserRef<uid_t>,
    suid_addr: UserRef<uid_t>,
) -> Result<(), Errno> {
    let creds = current_task.creds();
    current_task.mm.write_object(ruid_addr, &creds.uid)?;
    current_task.mm.write_object(euid_addr, &creds.euid)?;
    current_task.mm.write_object(suid_addr, &creds.saved_uid)?;
    Ok(())
}

pub fn sys_getresgid(
    current_task: &CurrentTask,
    rgid_addr: UserRef<gid_t>,
    egid_addr: UserRef<gid_t>,
    sgid_addr: UserRef<gid_t>,
) -> Result<(), Errno> {
    let creds = current_task.creds();
    current_task.mm.write_object(rgid_addr, &creds.gid)?;
    current_task.mm.write_object(egid_addr, &creds.egid)?;
    current_task.mm.write_object(sgid_addr, &creds.saved_gid)?;
    Ok(())
}

pub fn sys_setreuid(current_task: &CurrentTask, ruid: uid_t, euid: uid_t) -> Result<(), Errno> {
    let mut creds = current_task.creds();
    let allowed = |uid| uid == u32::MAX || new_uid_allowed(&creds, uid);
    if !allowed(ruid) || !allowed(euid) {
        return error!(EPERM);
    }

    let prev_ruid = creds.uid;
    let prev_euid = creds.euid;
    let prev_saved_uid = creds.saved_uid;
    let mut is_ruid_set = false;
    if ruid != u32::MAX {
        creds.uid = ruid;
        is_ruid_set = true;
    }
    if euid != u32::MAX {
        creds.euid = euid;
    }

    if is_ruid_set || prev_ruid != euid {
        creds.saved_uid = creds.euid;
    }

    creds.update_capabilities(prev_ruid, prev_euid, prev_saved_uid);
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setregid(current_task: &CurrentTask, rgid: gid_t, egid: gid_t) -> Result<(), Errno> {
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
    }

    if is_rgid_set || previous_rgid != egid {
        creds.saved_gid = creds.egid;
    }

    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setresuid(
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

    let prev_ruid = creds.uid;
    let prev_euid = creds.euid;
    let prev_saved_uid = creds.saved_uid;
    if ruid != u32::MAX {
        creds.uid = ruid;
    }
    if euid != u32::MAX {
        creds.euid = euid;
    }
    if suid != u32::MAX {
        creds.saved_uid = suid;
    }
    creds.update_capabilities(prev_ruid, prev_euid, prev_saved_uid);
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_setresgid(
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
    }
    if sgid != u32::MAX {
        creds.saved_gid = sgid;
    }
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_exit(current_task: &CurrentTask, code: i32) -> Result<(), Errno> {
    // Only change the current exit status if this has not been already set by exit_group, as
    // otherwise it has priority.
    current_task.write().exit_status.get_or_insert(ExitStatus::Exit(code as u8));
    Ok(())
}

pub fn sys_exit_group(current_task: &CurrentTask, code: i32) -> Result<(), Errno> {
    current_task.thread_group.exit(ExitStatus::Exit(code as u8));
    Ok(())
}

pub fn sys_sched_getscheduler(current_task: &CurrentTask, pid: pid_t) -> Result<u32, Errno> {
    if pid < 0 {
        return error!(EINVAL);
    }

    let target_task = get_task_or_current(current_task, pid)?;
    let current_policy = target_task.read().scheduler_policy;
    Ok(current_policy.raw_policy())
}

pub fn sys_sched_setscheduler(
    current_task: &CurrentTask,
    pid: pid_t,
    policy: u32,
    param: UserAddress,
) -> Result<(), Errno> {
    if pid < 0 || param.is_null() {
        return error!(EINVAL);
    }

    let target_task = get_task_or_current(current_task, pid)?;
    let rlimit = target_task.thread_group.get_rlimit(Resource::RTPRIO);

    let param: sched_param = current_task.mm.read_object(param.into())?;
    let policy = SchedulerPolicy::from_raw(policy, param, rlimit)?;
    // TODO(https://fxbug.dev/123174) make zircon aware of this update
    target_task.write().scheduler_policy = policy;

    Ok(())
}

type CpuAffinityMask = u64;
const CPU_AFFINITY_MASK_SIZE: u32 = std::mem::size_of::<CpuAffinityMask>() as u32;
const NUM_CPUS_MAX: u32 = CPU_AFFINITY_MASK_SIZE * 8;

fn get_default_cpumask() -> CpuAffinityMask {
    match fuchsia_zircon::system_get_num_cpus() {
        num_cpus if num_cpus > NUM_CPUS_MAX => {
            log_error!("num_cpus={}, greater than the {} max supported.", num_cpus, NUM_CPUS_MAX);
            CpuAffinityMask::MAX
        }
        NUM_CPUS_MAX => CpuAffinityMask::MAX,
        num_cpus => (1 << num_cpus) - 1,
    }
}

pub fn sys_sched_getaffinity(
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

    let _task = get_task_or_current(current_task, pid)?;

    // sched_setaffinity() is not implemented. Fake affinity mask based on the number of CPUs.
    let mask = get_default_cpumask();
    current_task.mm.write_memory(user_mask, &mask.to_ne_bytes())?;
    Ok(CPU_AFFINITY_MASK_SIZE as usize)
}

pub fn sys_sched_setaffinity(
    current_task: &CurrentTask,
    pid: pid_t,
    cpusetsize: u32,
    user_mask: UserAddress,
) -> Result<(), Errno> {
    if pid < 0 {
        return error!(EINVAL);
    }
    let _task = get_task_or_current(current_task, pid)?;

    if cpusetsize < CPU_AFFINITY_MASK_SIZE {
        return error!(EINVAL);
    }

    let mut mask: CpuAffinityMask = 0;
    current_task.mm.read_memory(user_mask, mask.as_bytes_mut())?;

    // Specified mask must include at least one valid CPU.
    if mask & get_default_cpumask() == 0 {
        return error!(EINVAL);
    }

    // Currently, we ignore the mask and act as if the system reset the mask
    // immediately to allowing all CPUs.
    Ok(())
}

pub fn sys_sched_getparam(
    current_task: &CurrentTask,
    pid: pid_t,
    param: UserAddress,
) -> Result<(), Errno> {
    if pid < 0 || param.is_null() {
        return error!(EINVAL);
    }

    let target_task = get_task_or_current(current_task, pid)?;
    let param_value = target_task.read().scheduler_policy.raw_params();
    current_task.mm.write_object(param.into(), &param_value)?;
    Ok(())
}

pub fn sys_sched_get_priority_min(_ctx: &CurrentTask, policy: u32) -> Result<i32, Errno> {
    min_priority_for_sched_policy(policy)
}

pub fn sys_sched_get_priority_max(_ctx: &CurrentTask, policy: u32) -> Result<i32, Errno> {
    max_priority_for_sched_policy(policy)
}

pub fn sys_prctl(
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
                not_implemented!("prctl: PR_SET_VMA: Unknown arg2: 0x{:x}", arg2);
                return error!(ENOSYS);
            }
            let addr = UserAddress::from(arg3);
            let length = arg4 as usize;
            let name_addr = UserAddress::from(arg5);
            let name = if name_addr.is_null() {
                None
            } else {
                let name = UserCString::new(UserAddress::from(arg5));
                let mut buf = [0u8; 256];
                // An overly long name produces EINVAL and not ENAMETOOLONG in Linux 5.15.
                let name = current_task.mm.read_c_string(name, &mut buf).map_err(|e| {
                    if e == errno!(ENAMETOOLONG) {
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
                Some(name.to_vec())
            };
            current_task.mm.set_mapping_name(addr, length, name)?;
            Ok(().into())
        }
        PR_SET_DUMPABLE => {
            let mut dumpable = current_task.mm.dumpable.lock();
            *dumpable = if arg2 == 1 { DumpPolicy::User } else { DumpPolicy::Disable };
            Ok(().into())
        }
        PR_GET_DUMPABLE => {
            let dumpable = current_task.mm.dumpable.lock();
            Ok(match *dumpable {
                DumpPolicy::Disable => 0.into(),
                DumpPolicy::User => 1.into(),
            })
        }
        PR_SET_PDEATHSIG => {
            not_implemented!("PR_SET_PDEATHSIG");
            Ok(().into())
        }
        PR_SET_NAME => {
            let addr = UserAddress::from(arg2);
            let mut name = [0u8; 16];
            current_task.mm.read_memory(addr, &mut name)?;
            // The name is truncated to 16 bytes (including the nul)
            name[15] = 0;
            // this will succeed, because we set 0 at end above
            let string_end = name.iter().position(|&c| c == 0).unwrap();

            let name_str = CString::new(&mut name[0..string_end]).map_err(|_| errno!(EINVAL))?;
            let thread = current_task.thread.read();
            if let Some(thread) = &*thread {
                thread.set_name(&name_str).map_err(|_| errno!(EINVAL))?;
            }
            current_task.set_command_name(name_str);
            crate::logging::set_current_task_info(current_task);
            Ok(0.into())
        }
        PR_GET_NAME => {
            let addr = UserAddress::from(arg2);
            current_task.mm.write_memory(addr, current_task.command().to_bytes_with_nul())?;
            Ok(().into())
        }
        PR_SET_PTRACER => {
            // PR_SET_PTRACER_ANY is defined as ((unsigned long) -1),
            // which is not understood by bindgen.
            if arg2 == u64::MAX {
                // Callers (especially debuggerd) expect EINVAL if the
                // kernel does not support PTRACER_ANY.
                return error!(EINVAL);
            }
            not_implemented!("prctl(PR_SET_PTRACER, {})", arg2);
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
                // arg3 should always be null, but that's checked in sys_seccomp.
                return sys_seccomp(current_task, SECCOMP_SET_MODE_STRICT, 0, arg3.into());
            } else if arg2 == SECCOMP_MODE_FILTER as u64 {
                return sys_seccomp(current_task, SECCOMP_SET_MODE_FILTER, 0, arg3.into());
            }
            Ok(().into())
        }
        PR_GET_CHILD_SUBREAPER => {
            let addr = UserAddress::from(arg2);
            #[allow(clippy::bool_to_int_with_if)]
            let value: i32 =
                if current_task.thread_group.read().is_child_subreaper { 1 } else { 0 };
            current_task.mm.write_object(addr.into(), &value)?;
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
            if !creds.has_capability(CAP_SETPCAP) {
                return error!(EPERM);
            }

            let securebits = SecureBits::from_bits(arg2 as u32).ok_or_else(|| {
                not_implemented!("PR_SET_SECUREBITS: bits 0x{:x}", arg2);
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
            if !creds.has_capability(CAP_SETPCAP) {
                return error!(EPERM);
            }

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
        _ => {
            not_implemented!("prctl: Unknown option: 0x{:x}", option);
            error!(ENOSYS)
        }
    }
}

pub fn sys_set_tid_address(
    current_task: &CurrentTask,
    user_tid: UserRef<pid_t>,
) -> Result<pid_t, Errno> {
    current_task.write().clear_child_tid = user_tid;
    Ok(current_task.get_tid())
}

pub fn sys_getrusage(
    current_task: &CurrentTask,
    who: i32,
    user_usage: UserRef<rusage>,
) -> Result<(), Errno> {
    const RUSAGE_SELF: i32 = crate::types::uapi::RUSAGE_SELF as i32;
    const RUSAGE_THREAD: i32 = crate::types::uapi::RUSAGE_THREAD as i32;
    // TODO(fxb/76811): Implement proper rusage.
    match who {
        RUSAGE_CHILDREN => (),
        RUSAGE_SELF => (),
        RUSAGE_THREAD => (),
        _ => return error!(EINVAL),
    };

    if !user_usage.is_null() {
        let usage = rusage::default();
        current_task.mm.write_object(user_usage, &usage)?;
    }

    Ok(())
}

pub fn sys_getrlimit(
    current_task: &CurrentTask,
    resource: u32,
    user_rlimit: UserRef<rlimit>,
) -> Result<(), Errno> {
    sys_prlimit64(current_task, 0, resource, Default::default(), user_rlimit)
}

pub fn sys_setrlimit(
    current_task: &CurrentTask,
    resource: u32,
    user_rlimit: UserRef<rlimit>,
) -> Result<(), Errno> {
    sys_prlimit64(current_task, 0, resource, user_rlimit, Default::default())
}

pub fn sys_prlimit64(
    current_task: &CurrentTask,
    pid: pid_t,
    user_resource: u32,
    user_new_limit: UserRef<rlimit>,
    user_old_limit: UserRef<rlimit>,
) -> Result<(), Errno> {
    // TODO: Lookup tasks by pid.
    if pid != 0 {
        not_implemented!("prlimit64 with non 0 pid");
        return error!(ENOSYS);
    }
    let task = &current_task.task;

    let resource = Resource::from_raw(user_resource)?;

    let maybe_new_limit = if !user_new_limit.is_null() {
        let new_limit = current_task.mm.read_object(user_new_limit)?;
        if new_limit.rlim_cur > new_limit.rlim_max {
            return error!(EINVAL);
        }
        Some(new_limit)
    } else {
        None
    };

    let old_limit = match resource {
        // TODO: Integrate Resource::STACK with generic ResourceLimits machinery.
        Resource::STACK => {
            if maybe_new_limit.is_some() {
                not_implemented!("prlimit64 cannot set RLIMIT_STACK");
            }
            // The stack size is fixed at the moment, but
            // if MAP_GROWSDOWN is implemented this should
            // report the limit that it can be grown.
            let mm_state = task.mm.state.read();
            let stack_size = mm_state.stack_size as u64;
            rlimit { rlim_cur: stack_size, rlim_max: stack_size }
        }
        _ => task.thread_group.adjust_rlimits(current_task, resource, maybe_new_limit)?,
    };

    if !user_old_limit.is_null() {
        current_task.mm.write_object(user_old_limit, &old_limit)?;
    }
    Ok(())
}

pub fn sys_capget(
    current_task: &CurrentTask,
    user_header: UserRef<__user_cap_header_struct>,
    user_data: UserRef<__user_cap_data_struct>,
) -> Result<(), Errno> {
    if user_data.is_null() {
        current_task.mm.write_object(
            user_header,
            &__user_cap_header_struct { version: _LINUX_CAPABILITY_VERSION_3, pid: 0 },
        )?;
        return Ok(());
    }

    let header = current_task.mm.read_object(user_header)?;
    let target_task: Arc<Task> = match header.pid {
        0 => current_task.task_arc_clone(),
        pid => current_task.get_task(pid).ok_or_else(|| errno!(EINVAL))?,
    };

    let (permitted, effective, inheritable) = {
        let creds = &target_task.creds();
        (creds.cap_permitted, creds.cap_effective, creds.cap_inheritable)
    };

    match header.version {
        _LINUX_CAPABILITY_VERSION_3 => {
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
            current_task.mm.write_objects(user_data, &data)?;
        }
        _ => return error!(EINVAL),
    }
    Ok(())
}

pub fn sys_capset(
    current_task: &CurrentTask,
    user_header: UserRef<__user_cap_header_struct>,
    user_data: UserRef<__user_cap_data_struct>,
) -> Result<(), Errno> {
    let header = current_task.mm.read_object(user_header)?;
    let target_task: Arc<Task> = match header.pid {
        0 => current_task.task_arc_clone(),
        pid if pid == current_task.id => current_task.task_arc_clone(),
        _pid => return error!(EINVAL),
    };

    let (new_permitted, new_effective, new_inheritable) = match header.version {
        _LINUX_CAPABILITY_VERSION_3 => {
            let mut data: [__user_cap_data_struct; 2] = Default::default();
            current_task.mm.read_objects(user_data, &mut data)?;
            (
                Capabilities::from_abi_v3((data[0].permitted, data[1].permitted)),
                Capabilities::from_abi_v3((data[0].effective, data[1].effective)),
                Capabilities::from_abi_v3((data[0].inheritable, data[1].inheritable)),
            )
        }
        _ => return error!(EINVAL),
    };

    // Permission checks. Copied out of TLPI section 39.7.
    let mut creds = target_task.creds();
    {
        log_trace!("Capabilities({{permitted={:?} from {:?}, effective={:?} from {:?}, inheritable={:?} from {:?}}}, bounding={:?})", new_permitted, creds.cap_permitted, new_effective, creds.cap_effective, new_inheritable, creds.cap_inheritable, creds.cap_bounding);
        if !creds.has_capability(CAP_SETPCAP)
            && !creds.cap_inheritable.union(creds.cap_permitted).contains(new_inheritable)
        {
            return error!(EPERM);
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
            if !current_task.read().no_new_privs()
                && !current_task.creds().has_capability(CAP_SYS_ADMIN)
            {
                return error!(EACCES);
            }
            if args.is_null() {
                return error!(EFAULT);
            }
            current_task.add_seccomp_filter(args, flags)
        }
        SECCOMP_GET_ACTION_AVAIL => {
            if flags != 0 || args.is_null() {
                return error!(EINVAL);
            }
            let action: u32 = current_task.mm.read_object(UserRef::new(args))?;
            SeccompState::is_action_available(action)
        }
        SECCOMP_GET_NOTIF_SIZES => {
            if flags != 0 {
                return error!(EINVAL);
            }
            not_implemented!("seccomp not implemented");
            error!(ENOSYS)
        }
        _ => error!(EINVAL),
    }
}

pub fn sys_setgroups(
    current_task: &CurrentTask,
    size: usize,
    groups_addr: UserAddress,
) -> Result<(), Errno> {
    if size > NGROUPS_MAX as usize {
        return error!(EINVAL);
    }
    let mut groups: Vec<gid_t> = vec![0; size];
    current_task.mm.read_memory(groups_addr, groups.as_mut_slice().as_bytes_mut())?;
    let mut creds = current_task.creds();
    if !creds.is_superuser() {
        return error!(EPERM);
    }
    creds.groups = groups;
    current_task.set_creds(creds);
    Ok(())
}

pub fn sys_getgroups(
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
        current_task.mm.write_memory(groups_addr, creds.groups.as_slice().as_bytes())?;
    }
    Ok(creds.groups.len())
}

pub fn sys_setsid(current_task: &CurrentTask) -> Result<pid_t, Errno> {
    current_task.thread_group.setsid()?;
    Ok(current_task.get_pid())
}

pub fn sys_getpriority(current_task: &CurrentTask, which: u32, who: i32) -> Result<u8, Errno> {
    match which {
        PRIO_PROCESS => {}
        _ => return error!(EINVAL),
    }
    // TODO(tbodt): check permissions
    let target_task = get_task_or_current(current_task, who)?;
    let state = target_task.read();
    Ok(state.priority)
}

pub fn sys_setpriority(
    current_task: &CurrentTask,
    which: u32,
    who: i32,
    priority: i32,
) -> Result<(), Errno> {
    match which {
        PRIO_PROCESS => {}
        _ => return error!(EINVAL),
    }
    // TODO(tbodt): check permissions
    let target_task = get_task_or_current(current_task, who)?;
    // The priority passed into setpriority is actually in the -19...20 range and is not
    // transformed into the 1...40 range. The man page is lying. (I sent a patch, so it might not
    // be lying anymore by the time you read this.)
    let priority = 20 - priority;
    let max_priority = std::cmp::min(40, target_task.thread_group.get_rlimit(Resource::NICE));
    target_task.write().priority = priority.clamp(1, max_priority as i32) as u8;
    Ok(())
}

pub fn sys_unshare(current_task: &CurrentTask, flags: u32) -> Result<(), Errno> {
    const IMPLEMENTED_FLAGS: u32 = CLONE_FILES | CLONE_NEWNS | CLONE_NEWUTS;
    if flags & !IMPLEMENTED_FLAGS != 0 {
        not_implemented!("unshare does not implement flags: 0x{:x}", flags & !IMPLEMENTED_FLAGS);
        return error!(EINVAL);
    }

    if (flags & CLONE_FILES) != 0 {
        current_task.files.unshare();
    }

    if (flags & CLONE_NEWNS) != 0 {
        if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
            return error!(EPERM);
        }
        current_task.fs().unshare_namespace();
    }

    if (flags & CLONE_NEWUTS) != 0 {
        if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
            return error!(EPERM);
        }
        // Fork the UTS namespace.
        let mut task_state = current_task.write();
        let new_uts_ns = task_state.uts_ns.read().clone();
        task_state.uts_ns = Arc::new(RwLock::new(new_uts_ns));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::syscalls::sys_munmap;
    use crate::testing::*;
    use std::u64;

    #[::fuchsia::test]
    async fn test_prctl_set_vma_anon_name() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let name_addr = mapped_address + 128u64;
        let name = "test-name\0";
        current_task.mm.write_memory(name_addr, name.as_bytes()).expect("failed to write name");
        sys_prctl(
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
                .mm
                .get_mapping_name(mapped_address + 24u64)
                .expect("failed to get address")
        );

        sys_munmap(&current_task, mapped_address, *PAGE_SIZE as usize)
            .expect("failed to unmap memory");
        assert_eq!(error!(EFAULT), current_task.mm.get_mapping_name(mapped_address + 24u64));
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_special_chars() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        let mapping_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        for c in 1..255 {
            let vma_name = CString::new([c]).unwrap();
            current_task.mm.write_memory(name_addr, vma_name.as_bytes_with_nul()).unwrap();

            let result = sys_prctl(
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
        let (_kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        let mapping_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        let name_too_long = CString::new(vec![b'a'; 256]).unwrap();

        current_task.mm.write_memory(name_addr, name_too_long.as_bytes_with_nul()).unwrap();

        assert_eq!(
            sys_prctl(
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

        current_task.mm.write_memory(name_addr, name_just_long_enough.as_bytes_with_nul()).unwrap();

        assert_eq!(
            sys_prctl(
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
        let (_kernel, mut current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        let mapping_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        let name = CString::new("name").unwrap();
        mm.write_memory(name_addr, name.as_bytes_with_nul()).unwrap();

        // Passing a misaligned pointer to the start of the named region fails.
        assert_eq!(
            sys_prctl(
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
        let (_kernel, mut current_task) = create_kernel_and_task();

        sys_prctl(&mut current_task, PR_GET_DUMPABLE, 0, 0, 0, 0).expect("failed to get dumpable");

        sys_prctl(&mut current_task, PR_SET_DUMPABLE, 1, 0, 0, 0).expect("failed to set dumpable");
        sys_prctl(&mut current_task, PR_GET_DUMPABLE, 0, 0, 0, 0).expect("failed to get dumpable");

        // SUID_DUMP_ROOT not supported.
        sys_prctl(&mut current_task, PR_SET_DUMPABLE, 2, 0, 0, 0).expect("failed to set dumpable");
        sys_prctl(&mut current_task, PR_GET_DUMPABLE, 0, 0, 0, 0).expect("failed to get dumpable");
    }

    #[::fuchsia::test]
    async fn test_sys_getsid() {
        let (kernel, current_task) = create_kernel_and_task();

        assert_eq!(
            current_task.get_tid(),
            sys_getsid(&current_task, 0).expect("failed to get sid")
        );

        let second_current = create_task(&kernel, "second task");

        assert_eq!(
            second_current.get_tid(),
            sys_getsid(&current_task, second_current.get_tid()).expect("failed to get sid")
        );
    }

    #[::fuchsia::test]
    async fn test_get_affinity_size() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let pid = current_task.get_pid();
        assert_eq!(
            sys_sched_getaffinity(&current_task, pid, 16, mapped_address),
            Ok(std::mem::size_of::<u64>())
        );
        assert_eq!(sys_sched_getaffinity(&current_task, pid, 1, mapped_address), error!(EINVAL));
        assert_eq!(sys_sched_getaffinity(&current_task, pid, 9, mapped_address), error!(EINVAL));
    }

    #[::fuchsia::test]
    async fn test_set_affinity_size() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        current_task.mm.write_memory(mapped_address, &[0xffu8]).expect("failed to cpumask");
        let pid = current_task.get_pid();
        assert_eq!(sys_sched_setaffinity(&current_task, pid, u32::MAX, mapped_address), Ok(()));
        assert_eq!(sys_sched_setaffinity(&current_task, pid, 1, mapped_address), error!(EINVAL));
    }

    #[::fuchsia::test]
    async fn test_task_name() {
        let (_kernel, mut current_task) = create_kernel_and_task();
        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let name = "my-task-name\0";
        current_task
            .mm
            .write_memory(mapped_address, name.as_bytes())
            .expect("failed to write name");

        let result =
            sys_prctl(&mut current_task, PR_SET_NAME, mapped_address.ptr() as u64, 0, 0, 0)
                .unwrap();
        assert_eq!(SUCCESS, result);

        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let result =
            sys_prctl(&mut current_task, PR_GET_NAME, mapped_address.ptr() as u64, 0, 0, 0)
                .unwrap();
        assert_eq!(SUCCESS, result);

        let name_length = name.len();
        let mut out_name = vec![0u8; name_length];

        current_task.mm.read_memory(mapped_address, &mut out_name).unwrap();
        assert_eq!(name.as_bytes(), &out_name);
    }

    #[::fuchsia::test]
    async fn test_sched_get_priority_min_max() {
        let (_kernel, current_task) = create_kernel_and_task();
        let non_rt_min = sys_sched_get_priority_min(&current_task, SCHED_NORMAL).unwrap();
        assert_eq!(non_rt_min, 0);
        let non_rt_max = sys_sched_get_priority_max(&current_task, SCHED_NORMAL).unwrap();
        assert_eq!(non_rt_max, 0);

        let rt_min = sys_sched_get_priority_min(&current_task, SCHED_FIFO).unwrap();
        assert_eq!(rt_min, 1);
        let rt_max = sys_sched_get_priority_max(&current_task, SCHED_FIFO).unwrap();
        assert_eq!(rt_max, 99);

        let min_bad_policy_error =
            sys_sched_get_priority_min(&current_task, std::u32::MAX).unwrap_err();
        assert_eq!(min_bad_policy_error, errno!(EINVAL));

        let max_bad_policy_error =
            sys_sched_get_priority_max(&current_task, std::u32::MAX).unwrap_err();
        assert_eq!(max_bad_policy_error, errno!(EINVAL));
    }

    #[::fuchsia::test]
    async fn test_sched_setscheduler() {
        let (_kernel, current_task) = create_kernel_and_task();

        current_task
            .thread_group
            .write()
            .limits
            .set(Resource::RTPRIO, rlimit { rlim_cur: 255, rlim_max: 255 });

        let scheduler = sys_sched_getscheduler(&current_task, 0).unwrap();
        assert_eq!(scheduler, SCHED_NORMAL, "tasks should have normal scheduler by default");

        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let requested_params = sched_param { sched_priority: 15 };
        current_task.mm.write_object(mapped_address.into(), &requested_params).unwrap();

        sys_sched_setscheduler(&current_task, 0, SCHED_FIFO, mapped_address).unwrap();

        let new_scheduler = sys_sched_getscheduler(&current_task, 0).unwrap();
        assert_eq!(new_scheduler, SCHED_FIFO, "task should have been assigned fifo scheduler");

        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        sys_sched_getparam(&current_task, 0, mapped_address).expect("sched_getparam");
        let param_value: sched_param =
            current_task.mm.read_object(mapped_address.into()).expect("read_object");
        assert_eq!(param_value.sched_priority, 15);
    }

    #[::fuchsia::test]
    async fn test_sched_getparam() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mapped_address = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        sys_sched_getparam(&current_task, 0, mapped_address).expect("sched_getparam");
        let param_value: sched_param =
            current_task.mm.read_object(mapped_address.into()).expect("read_object");
        assert_eq!(param_value.sched_priority, 0);
    }

    #[::fuchsia::test]
    async fn test_setuid() {
        let (_kernel, current_task) = create_kernel_and_task();
        // Test for root.
        current_task.set_creds(Credentials::root());
        sys_setuid(&current_task, 42).expect("setuid");
        let mut creds = current_task.creds();
        assert_eq!(creds.euid, 42);
        assert_eq!(creds.uid, 42);
        assert_eq!(creds.saved_uid, 42);

        // Remove the CAP_SETUID capability to avoid overwriting permission checks.
        creds.cap_effective.remove(CAP_SETUID);
        current_task.set_creds(creds);

        // Test for non root, which task now is.
        assert_eq!(sys_setuid(&current_task, 0), error!(EPERM));
        assert_eq!(sys_setuid(&current_task, 43), error!(EPERM));

        sys_setuid(&current_task, 42).expect("setuid");
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

        sys_setuid(&current_task, 41).expect("setuid");
        let creds = current_task.creds();
        assert_eq!(creds.euid, 41);
        assert_eq!(creds.uid, 41);
        assert_eq!(creds.saved_uid, 43);

        let mut creds = current_task.creds();
        creds.uid = 41;
        creds.euid = 42;
        creds.saved_uid = 43;
        current_task.set_creds(creds);

        sys_setuid(&current_task, 43).expect("setuid");
        let creds = current_task.creds();
        assert_eq!(creds.euid, 43);
        assert_eq!(creds.uid, 41);
        assert_eq!(creds.saved_uid, 43);
    }
}
