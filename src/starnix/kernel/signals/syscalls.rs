// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use super::signal_handling::sys_restart_syscall;
use super::signalfd::SignalFd;
use crate::mm::{MemoryAccessor, MemoryAccessorExt};
use crate::security;
use crate::signals::{
    restore_from_signal_handler, send_signal, SignalDetail, SignalInfo, SignalInfoHeader,
    SI_HEADER_SIZE,
};
use crate::task::{
    CurrentTask, ProcessEntryRef, ProcessSelector, Task, TaskMutableState, ThreadGroup, WaitResult,
    WaitableChildResult, Waiter,
};
use crate::vfs::{FdFlags, FdNumber};

use starnix_logging::track_stub;
use starnix_sync::{Locked, Unlocked};
use starnix_syscalls::SyscallResult;
use starnix_types::ownership::{OwnedRef, TempRef, WeakRef};
use starnix_types::time::{duration_from_timespec, timeval_from_duration};
use starnix_uapi::errors::{Errno, ErrnoResultExt, EINTR, ETIMEDOUT};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::signals::{SigSet, Signal, UncheckedSignal, UNBLOCKABLE_SIGNALS};
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::{
    errno, error, pid_t, rusage, sigaction, sigaltstack, timespec, MINSIGSTKSZ, P_ALL, P_PGID,
    P_PID, P_PIDFD, SFD_CLOEXEC, SFD_NONBLOCK, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, SI_MAX_SIZE,
    SI_TKILL, SI_USER, SS_AUTODISARM, SS_DISABLE, SS_ONSTACK, WCONTINUED, WEXITED, WNOHANG,
    WNOWAIT, WSTOPPED, WUNTRACED, __WALL, __WCLONE,
};
use static_assertions::const_assert_eq;
use zerocopy::FromBytes;

// Rust will let us do this cast in a const assignment but not in a const generic constraint.
const SI_MAX_SIZE_AS_USIZE: usize = SI_MAX_SIZE as usize;

pub fn sys_rt_sigaction(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    signum: UncheckedSignal,
    user_action: UserRef<sigaction>,
    user_old_action: UserRef<sigaction>,
    sigset_size: usize,
) -> Result<(), Errno> {
    if sigset_size != std::mem::size_of::<SigSet>() {
        return error!(EINVAL);
    }

    let signal = Signal::try_from(signum)?;

    let new_signal_action = if !user_action.is_null() {
        // Actions can't be set for SIGKILL and SIGSTOP, but the actions for these signals can
        // still be returned in `user_old_action`, so only return early if the intention is to
        // set an action (i.e., the user_action is non-null).
        if signal.is_unblockable() {
            return error!(EINVAL);
        }

        let signal_action = current_task.read_object(user_action)?;
        Some(signal_action)
    } else {
        None
    };

    let signal_actions = &current_task.thread_group.signal_actions;
    let old_action = if let Some(new_signal_action) = new_signal_action {
        signal_actions.set(signal, new_signal_action)
    } else {
        signal_actions.get(signal)
    };

    if !user_old_action.is_null() {
        current_task.write_object(user_old_action, &old_action)?;
    }

    Ok(())
}

pub fn sys_rt_sigpending(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    set: UserRef<SigSet>,
    sigset_size: usize,
) -> Result<(), Errno> {
    if sigset_size != std::mem::size_of::<SigSet>() {
        return error!(EINVAL);
    }

    let signals = current_task.read().pending_signals();
    current_task.write_object(set, &signals)?;
    Ok(())
}

pub fn sys_rt_sigprocmask(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    how: u32,
    user_set: UserRef<SigSet>,
    user_old_set: UserRef<SigSet>,
    sigset_size: usize,
) -> Result<(), Errno> {
    if sigset_size != std::mem::size_of::<SigSet>() {
        return error!(EINVAL);
    }
    match how {
        SIG_BLOCK | SIG_UNBLOCK | SIG_SETMASK => (),
        _ => return error!(EINVAL),
    };

    // Read the new mask. This must be done before the old mask is written to `user_old_set`
    // since it might point to the same location as `user_set`.
    let mut new_mask = SigSet::default();
    if !user_set.is_null() {
        new_mask = current_task.read_object(user_set)?;
    }

    let mut state = current_task.write();
    let signal_mask = state.signal_mask();
    // If old_set is not null, store the previous value in old_set.
    if !user_old_set.is_null() {
        current_task.write_object(user_old_set, &signal_mask)?;
    }

    // If set is null, how is ignored and the mask is not updated.
    if user_set.is_null() {
        return Ok(());
    }

    let signal_mask = match how {
        SIG_BLOCK => signal_mask | new_mask,
        SIG_UNBLOCK => signal_mask & !new_mask,
        SIG_SETMASK => new_mask,
        // Arguments have already been verified, this should never match.
        _ => return error!(EINVAL),
    };
    state.set_signal_mask(signal_mask);

    Ok(())
}

pub fn sys_sigaltstack(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    user_ss: UserRef<sigaltstack>,
    user_old_ss: UserRef<sigaltstack>,
) -> Result<(), Errno> {
    let stack_pointer_register = current_task.thread_state.registers.stack_pointer_register();
    let mut state = current_task.write();
    let on_signal_stack = state.on_signal_stack(stack_pointer_register);

    let mut ss = sigaltstack::default();
    if !user_ss.is_null() {
        if on_signal_stack {
            return error!(EPERM);
        }
        ss = current_task.read_object(user_ss)?;
        if (ss.ss_flags & !((SS_AUTODISARM | SS_DISABLE) as i32)) != 0 {
            return error!(EINVAL);
        }
        if ss.ss_flags & (SS_DISABLE as i32) == 0 && ss.ss_size < MINSIGSTKSZ as u64 {
            return error!(ENOMEM);
        }
    }

    if !user_old_ss.is_null() {
        let mut old_ss = match state.sigaltstack() {
            Some(old_ss) => old_ss,
            None => sigaltstack { ss_flags: SS_DISABLE as i32, ..sigaltstack::default() },
        };
        if on_signal_stack {
            old_ss.ss_flags = SS_ONSTACK as i32;
        }
        current_task.write_object(user_old_ss, &old_ss)?;
    }

    if !user_ss.is_null() {
        if ss.ss_flags & (SS_DISABLE as i32) != 0 {
            state.set_sigaltstack(None);
        } else {
            state.set_sigaltstack(Some(ss));
        }
    }

    Ok(())
}

pub fn sys_rt_sigsuspend(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    user_mask: UserRef<SigSet>,
    sigset_size: usize,
) -> Result<(), Errno> {
    if sigset_size != std::mem::size_of::<SigSet>() {
        return error!(EINVAL);
    }
    let mask = current_task.read_object(user_mask)?;

    let waiter = Waiter::new();
    // ERESTARTNOHAND indicates that the error should be EINTR if
    // interrupted by a signal delivered to a user handler, and the syscall
    // should be restarted otherwise.
    current_task
        .wait_with_temporary_mask(locked, mask, |locked, current_task| {
            waiter.wait(locked, current_task)
        })
        .map_eintr(|| errno!(ERESTARTNOHAND))
}

pub fn sys_rt_sigtimedwait(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    set_addr: UserRef<SigSet>,
    siginfo_addr: UserAddress,
    timeout_addr: UserRef<timespec>,
    sigset_size: usize,
) -> Result<Signal, Errno> {
    if sigset_size != std::mem::size_of::<SigSet>() {
        return error!(EINVAL);
    }

    // Signals in `set_addr` are what we are waiting for.
    let set = current_task.read_object(set_addr)?;
    // Attempts to wait for `UNBLOCKABLE_SIGNALS` will be ignored.
    let unblock = set & !UNBLOCKABLE_SIGNALS;
    let deadline = if timeout_addr.is_null() {
        zx::MonotonicInstant::INFINITE
    } else {
        let timeout = current_task.read_object(timeout_addr)?;
        zx::MonotonicInstant::after(duration_from_timespec(timeout)?)
    };

    let signal_info = loop {
        let waiter;

        {
            let mut task_state = current_task.write();
            // If one of the signals in set is already pending for the calling thread,
            // sigwaitinfo() will return immediately.
            if let Some(signal) = task_state.take_signal_with_mask(!unblock) {
                break signal;
            }

            waiter = Waiter::new();
            task_state.wait_on_signal(&waiter);
        }

        // A new signal is enqueued when it's masked in the SignalState. So we need to invert
        // the SigSet to block them.
        let tmp_mask = current_task.read().signal_mask() & !unblock;

        // Wait for a timeout or a new signal.
        let waiter_result =
            current_task.wait_with_temporary_mask(locked, tmp_mask, |locked, current_task| {
                waiter.wait_until(locked, current_task, deadline)
            });

        // Restore mask after timeout or get a new signal.
        current_task.write().restore_signal_mask();

        if let Err(e) = waiter_result {
            if e == EINTR {
                // Check if EINTR was returned for a signal we were waiting for.
                if let Some(signal) = current_task.write().take_signal_with_mask(!unblock) {
                    break signal;
                }
            } else if e == ETIMEDOUT {
                return error!(EAGAIN);
            }

            return Err(e);
        }
    };

    if !siginfo_addr.is_null() {
        current_task.write_memory(siginfo_addr, &signal_info.as_siginfo_bytes())?;
    }

    Ok(signal_info.signal)
}

pub fn sys_signalfd4(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    mask_addr: UserRef<SigSet>,
    mask_size: usize,
    flags: u32,
) -> Result<FdNumber, Errno> {
    if flags & !(SFD_CLOEXEC | SFD_NONBLOCK) != 0 {
        return error!(EINVAL);
    }
    if mask_size != std::mem::size_of::<SigSet>() {
        return error!(EINVAL);
    }
    let mask = current_task.read_object(mask_addr)?;

    if fd.raw() != -1 {
        let file = current_task.files.get(fd)?;
        let file = file.downcast_file::<SignalFd>().ok_or_else(|| errno!(EINVAL))?;
        file.set_mask(mask);
        Ok(fd)
    } else {
        let signalfd = SignalFd::new_file(current_task, mask, flags);
        let flags = if flags & SFD_CLOEXEC != 0 { FdFlags::CLOEXEC } else { FdFlags::empty() };
        let fd = current_task.add_file(signalfd, flags)?;
        Ok(fd)
    }
}

fn send_unchecked_signal(
    current_task: &CurrentTask,
    target: &Task,
    unchecked_signal: UncheckedSignal,
    si_code: i32,
) -> Result<(), Errno> {
    if !current_task.can_signal(&target, unchecked_signal) {
        return error!(EPERM);
    }

    // 0 is a sentinel value used to do permission checks.
    if unchecked_signal.is_zero() {
        return Ok(());
    }

    let signal = Signal::try_from(unchecked_signal)?;
    security::check_signal_access(current_task, &target, signal)?;

    send_signal(
        target,
        SignalInfo {
            code: si_code,
            detail: SignalDetail::Kill {
                pid: current_task.thread_group.leader,
                uid: current_task.creds().uid,
            },
            ..SignalInfo::default(signal)
        },
    )
}

fn send_unchecked_signal_info(
    current_task: &CurrentTask,
    target: &Task,
    unchecked_signal: UncheckedSignal,
    siginfo_ref: UserAddress,
) -> Result<(), Errno> {
    if !current_task.can_signal(&target, unchecked_signal) {
        return error!(EPERM);
    }

    // 0 is a sentinel value used to do permission checks.
    if unchecked_signal.is_zero() {
        // Check we can read siginfo.
        current_task.read_memory_to_array::<SI_MAX_SIZE_AS_USIZE>(siginfo_ref)?;
        return Ok(());
    }

    let signal = Signal::try_from(unchecked_signal)?;
    security::check_signal_access(current_task, &target, signal)?;

    let siginfo = read_siginfo(current_task, signal, siginfo_ref)?;
    if target.get_pid() != current_task.get_pid() && (siginfo.code >= 0 || siginfo.code == SI_TKILL)
    {
        return error!(EINVAL);
    }

    send_signal(&target, siginfo)
}

pub fn sys_kill(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pid: pid_t,
    unchecked_signal: UncheckedSignal,
) -> Result<(), Errno> {
    let pids = current_task.kernel().pids.read();
    match pid {
        pid if pid > 0 => {
            // "If pid is positive, then signal sig is sent to the process with
            // the ID specified by pid."
            let target_thread_group = {
                match pids.get_process(pid) {
                    Some(ProcessEntryRef::Process(process)) => process,

                    // Zombies cannot receive signals. Just ignore it.
                    Some(ProcessEntryRef::Zombie(_zombie)) => return Ok(()),

                    // If we don't have process with `pid` then check if there is a task with
                    // the `pid`.
                    None => {
                        let weak_task = pids.get_task(pid);
                        let task = Task::from_weak(&weak_task)?;
                        TempRef::into_static(OwnedRef::temp(&task.thread_group))
                    }
                }
            };

            target_thread_group.send_signal_unchecked(current_task, unchecked_signal)?;
        }
        pid if pid == -1 => {
            // "If pid equals -1, then sig is sent to every process for which
            // the calling process has permission to send signals, except for
            // process 1 (init), but ... POSIX.1-2001 requires that kill(-1,sig)
            // send sig to all processes that the calling process may send
            // signals to, except possibly for some implementation-defined
            // system processes. Linux allows a process to signal itself, but on
            // Linux the call kill(-1,sig) does not signal the calling process."

            let thread_groups = pids.get_thread_groups();
            signal_thread_groups(
                current_task,
                unchecked_signal,
                thread_groups.into_iter().filter(|thread_group| {
                    if current_task.thread_group == *thread_group {
                        return false;
                    }
                    if thread_group.leader == 1 {
                        return false;
                    }
                    true
                }),
            )?;
        }
        _ => {
            // "If pid equals 0, then sig is sent to every process in the
            // process group of the calling process."
            //
            // "If pid is less than -1, then sig is sent to every process in the
            // process group whose ID is -pid."
            let process_group_id = match pid {
                0 => current_task.thread_group.read().process_group.leader,
                _ => negate_pid(pid)?,
            };

            let process_group = pids.get_process_group(process_group_id);
            let thread_groups = process_group.iter().flat_map(|pg| {
                pg.read(locked).thread_groups().map(TempRef::into_static).collect::<Vec<_>>()
            });
            signal_thread_groups(current_task, unchecked_signal, thread_groups)?;
        }
    };

    Ok(())
}

fn verify_tgid_for_task(task: &Task, tgid: pid_t) -> Result<(), Errno> {
    let pids = task.thread_group.kernel.pids.read();
    let thread_group = match pids.get_process(tgid) {
        Some(ProcessEntryRef::Process(proc)) => proc,
        Some(ProcessEntryRef::Zombie(_)) => return error!(EINVAL),
        None => return error!(ESRCH),
    };
    if task.thread_group != thread_group {
        return error!(EINVAL);
    } else {
        Ok(())
    }
}

pub fn sys_tkill(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    tid: pid_t,
    unchecked_signal: UncheckedSignal,
) -> Result<(), Errno> {
    // Linux returns EINVAL when the tgid or tid <= 0.
    if tid <= 0 {
        return error!(EINVAL);
    }
    let thread_weak = current_task.get_task(tid);
    let thread = Task::from_weak(&thread_weak)?;
    send_unchecked_signal(current_task, &thread, unchecked_signal, SI_TKILL)
}

pub fn sys_tgkill(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    tgid: pid_t,
    tid: pid_t,
    unchecked_signal: UncheckedSignal,
) -> Result<(), Errno> {
    // Linux returns EINVAL when the tgid or tid <= 0.
    if tgid <= 0 || tid <= 0 {
        return error!(EINVAL);
    }
    let weak_target = current_task.get_task(tid);
    let thread = Task::from_weak(&weak_target)?;
    verify_tgid_for_task(&thread, tgid)?;
    send_unchecked_signal(current_task, &thread, unchecked_signal, SI_TKILL)
}

pub fn sys_rt_sigreturn(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
) -> Result<SyscallResult, Errno> {
    restore_from_signal_handler(current_task)?;
    Ok(current_task.thread_state.registers.return_register().into())
}

pub fn read_siginfo(
    current_task: &CurrentTask,
    signal: Signal,
    siginfo_ref: UserAddress,
) -> Result<SignalInfo, Errno> {
    let siginfo_mem = current_task.read_memory_to_array::<SI_MAX_SIZE_AS_USIZE>(siginfo_ref)?;
    let header = SignalInfoHeader::read_from_bytes(&siginfo_mem[..SI_HEADER_SIZE]).unwrap();

    if header.signo != 0 && header.signo != signal.number() {
        return error!(EINVAL);
    }

    let mut bytes = [0u8; SI_MAX_SIZE as usize - SI_HEADER_SIZE];
    bytes.copy_from_slice(&siginfo_mem[SI_HEADER_SIZE..SI_MAX_SIZE as usize]);
    let details = SignalDetail::Raw { data: bytes };

    Ok(SignalInfo { signal, errno: header.errno, code: header.code, detail: details, force: false })
}

pub fn sys_rt_sigqueueinfo(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    tgid: pid_t,
    unchecked_signal: UncheckedSignal,
    siginfo_ref: UserAddress,
) -> Result<(), Errno> {
    let weak_task = current_task.kernel().pids.read().get_task(tgid);
    let task = &Task::from_weak(&weak_task)?;
    task.thread_group.send_signal_unchecked_with_info(current_task, unchecked_signal, siginfo_ref)
}

pub fn sys_rt_tgsigqueueinfo(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    tgid: pid_t,
    tid: pid_t,
    unchecked_signal: UncheckedSignal,
    siginfo_ref: UserAddress,
) -> Result<(), Errno> {
    let thread_weak = current_task.get_task(tid);
    let thread = Task::from_weak(&thread_weak)?;
    verify_tgid_for_task(&thread, tgid)?;
    send_unchecked_signal_info(current_task, &thread, unchecked_signal, siginfo_ref)
}

pub fn sys_pidfd_send_signal(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    pidfd: FdNumber,
    unchecked_signal: UncheckedSignal,
    siginfo_ref: UserAddress,
    flags: u32,
) -> Result<(), Errno> {
    if flags != 0 {
        return error!(EINVAL);
    }

    let file = current_task.files.get(pidfd)?;
    let target = current_task.get_task(file.as_pid()?);
    let target = target.upgrade().ok_or_else(|| errno!(ESRCH))?;

    if siginfo_ref.is_null() {
        send_unchecked_signal(current_task, &target, unchecked_signal, SI_USER as i32)
    } else {
        send_unchecked_signal_info(current_task, &target, unchecked_signal, siginfo_ref)
    }
}

/// Sends a signal to all thread groups in `thread_groups`.
///
/// # Parameters
/// - `task`: The task that is sending the signal.
/// - `unchecked_signal`: The signal that is to be sent. Unchecked, since `0` is a sentinel value
/// where rights are to be checked but no signal is actually sent.
/// - `thread_groups`: The thread groups to signal.
///
/// # Returns
/// Returns Ok(()) if at least one signal was sent, otherwise the last error that was encountered.
fn signal_thread_groups<F>(
    current_task: &CurrentTask,
    unchecked_signal: UncheckedSignal,
    thread_groups: F,
) -> Result<(), Errno>
where
    F: IntoIterator<Item: AsRef<ThreadGroup>>,
{
    let mut last_error = None;
    let mut sent_signal = false;

    // This loop keeps track of whether a signal was sent, so that "on
    // success (at least one signal was sent), zero is returned."
    for thread_group in thread_groups.into_iter() {
        match thread_group.as_ref().send_signal_unchecked(current_task, unchecked_signal) {
            Ok(_) => sent_signal = true,
            Err(errno) => last_error = Some(errno),
        }
    }

    if sent_signal {
        Ok(())
    } else {
        Err(last_error.unwrap_or_else(|| errno!(ESRCH)))
    }
}

/// The generic options for both waitid and wait4.
#[derive(Debug)]
pub struct WaitingOptions {
    /// Wait for a process that has exited.
    pub wait_for_exited: bool,
    /// Wait for a process in the stop state.
    pub wait_for_stopped: bool,
    /// Wait for a process that was continued.
    pub wait_for_continued: bool,
    /// Block the wait until a process matches.
    pub block: bool,
    /// Do not clear the waitable state.
    pub keep_waitable_state: bool,
    /// Wait for all children processes.
    pub wait_for_all: bool,
    /// Wait for children who deliver no signal or a signal other than SIGCHLD, ignored if wait_for_all is true
    pub wait_for_clone: bool,
    /// If present, and the target is ptraced by the waiter, wait for processes
    /// to do anything.
    pub waiter: Option<pid_t>,
}

impl WaitingOptions {
    fn new(options: u32) -> Self {
        const_assert_eq!(WUNTRACED, WSTOPPED);
        Self {
            wait_for_exited: options & WEXITED > 0,
            wait_for_stopped: options & WSTOPPED > 0,
            wait_for_continued: options & WCONTINUED > 0,
            block: options & WNOHANG == 0,
            keep_waitable_state: options & WNOWAIT > 0,
            wait_for_all: options & __WALL > 0,
            wait_for_clone: options & __WCLONE > 0,
            waiter: None,
        }
    }

    /// Build a `WaitingOptions` from the waiting flags of waitid.
    pub fn new_for_waitid(options: u32) -> Result<Self, Errno> {
        if options & !(__WCLONE | __WALL | WNOHANG | WNOWAIT | WSTOPPED | WEXITED | WCONTINUED) != 0
        {
            track_stub!(TODO("https://fxbug.dev/322874788"), "waitid options", options);
            return error!(EINVAL);
        }
        if options & (WEXITED | WSTOPPED | WCONTINUED) == 0 {
            return error!(EINVAL);
        }
        Ok(Self::new(options))
    }

    /// Build a `WaitingOptions` from the waiting flags of wait4.
    pub fn new_for_wait4(options: u32, waiter_pid: pid_t) -> Result<Self, Errno> {
        if options & !(__WCLONE | __WALL | WNOHANG | WUNTRACED | WCONTINUED) != 0 {
            track_stub!(TODO("https://fxbug.dev/322874017"), "wait4 options", options);
            return error!(EINVAL);
        }
        let mut result = Self::new(options | WEXITED);
        result.waiter = Some(waiter_pid);
        Ok(result)
    }
}

/// Waits on the task with `pid` to exit or change state.
///
/// - `current_task`: The current task.
/// - `pid`: The id of the task to wait on.
/// - `options`: The options passed to the wait syscall.
fn wait_on_pid(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    selector: ProcessSelector,
    options: &WaitingOptions,
) -> Result<Option<WaitResult>, Errno> {
    let waiter = Waiter::new();
    loop {
        {
            let mut pids = current_task.kernel().pids.write();
            // Waits and notifies on a given task need to be done atomically
            // with respect to changes to the task's waitable state; otherwise,
            // we see missing notifications. We do that by holding the task lock.
            // This next line checks for waitable traces without holding the
            // task lock, because constructing WaitResult objects requires
            // holding all sorts of locks that are incompatible with holding the
            // task lock.  We therefore have to check to see if a tracee has
            // become waitable again, after we acquire the lock.
            if let Some(tracee) =
                current_task.thread_group.get_waitable_ptracee(selector, options, &mut pids)
            {
                return Ok(Some(tracee));
            }
            {
                let mut thread_group = current_task.thread_group.write();

                // Per the above, see if traced tasks have become waitable. If they have, release
                // the lock and retry getting waitable tracees.
                let mut has_waitable_tracee = false;
                let mut has_any_tracee = false;
                current_task.thread_group.get_ptracees_and(
                    selector,
                    &pids,
                    &mut |task: WeakRef<Task>, task_state: &TaskMutableState| {
                        if let Some(ptrace) = &task_state.ptrace {
                            has_any_tracee = true;
                            ptrace.tracer_waiters().wait_async(&waiter);
                            if let Some(task_ref) = task.upgrade() {
                                if ptrace.is_waitable(task_ref.load_stopped(), options) {
                                    has_waitable_tracee = true;
                                }
                            }
                        }
                    },
                );
                if has_waitable_tracee || thread_group.zombie_ptracees.has_match(&selector, &pids) {
                    continue;
                }
                match thread_group.get_waitable_child(selector, options, &mut pids) {
                    WaitableChildResult::ReadyNow(child) => {
                        return Ok(Some(child));
                    }
                    WaitableChildResult::ShouldWait => (),
                    WaitableChildResult::NoneFound => {
                        if !has_any_tracee {
                            return error!(ECHILD);
                        }
                    }
                }
                thread_group.child_status_waiters.wait_async(&waiter);
            }
        }

        if !options.block {
            return Ok(None);
        }
        waiter.wait(locked, current_task).map_eintr(|| errno!(ERESTARTSYS))?;
    }
}

pub fn sys_waitid(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    id_type: u32,
    id: i32,
    user_info: UserAddress,
    options: u32,
    user_rusage: UserRef<rusage>,
) -> Result<(), Errno> {
    let mut waiting_options = WaitingOptions::new_for_waitid(options)?;

    let task_selector = match id_type {
        P_PID => ProcessSelector::Pid(id),
        P_ALL => ProcessSelector::Any,
        P_PGID => ProcessSelector::Pgid(if id == 0 {
            current_task.thread_group.read().process_group.leader
        } else {
            id
        }),
        P_PIDFD => {
            let fd = FdNumber::from_raw(id);
            let file = current_task.files.get(fd)?;
            if file.flags().contains(OpenFlags::NONBLOCK) {
                waiting_options.block = false;
            }
            ProcessSelector::Pid(file.as_pid()?)
        }
        _ => return error!(EINVAL),
    };

    // wait_on_pid returns None if the task was not waited on. In that case, we don't write out a
    // siginfo. This seems weird but is the correct behavior according to the waitid(2) man page.
    if let Some(waitable_process) =
        wait_on_pid(locked, current_task, task_selector, &waiting_options)?
    {
        if !user_rusage.is_null() {
            let usage = rusage {
                ru_utime: timeval_from_duration(waitable_process.time_stats.user_time),
                ru_stime: timeval_from_duration(waitable_process.time_stats.system_time),
                ..Default::default()
            };

            track_stub!(TODO("https://fxbug.dev/322874712"), "real rusage from waitid");
            current_task.write_object(user_rusage, &usage)?;
        }

        if !user_info.is_null() {
            let siginfo = waitable_process.as_signal_info();
            current_task.write_memory(user_info, &siginfo.as_siginfo_bytes())?;
        }
    } else if id_type == P_PIDFD {
        // From <https://man7.org/linux/man-pages/man2/pidfd_open.2.html>:
        //
        //   PIDFD_NONBLOCK
        //     Return a nonblocking file descriptor.  If the process
        //     referred to by the file descriptor has not yet terminated,
        //     then an attempt to wait on the file descriptor using
        //     waitid(2) will immediately return the error EAGAIN rather
        //     than blocking.
        return error!(EAGAIN);
    }

    Ok(())
}

pub fn sys_wait4(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    raw_selector: pid_t,
    user_wstatus: UserRef<i32>,
    options: u32,
    user_rusage: UserRef<rusage>,
) -> Result<pid_t, Errno> {
    let waiting_options = WaitingOptions::new_for_wait4(options, current_task.get_pid())?;

    let selector = if raw_selector == 0 {
        ProcessSelector::Pgid(current_task.thread_group.read().process_group.leader)
    } else if raw_selector == -1 {
        ProcessSelector::Any
    } else if raw_selector > 0 {
        ProcessSelector::Pid(raw_selector)
    } else if raw_selector < -1 {
        ProcessSelector::Pgid(negate_pid(raw_selector)?)
    } else {
        track_stub!(
            TODO("https://fxbug.dev/322874213"),
            "wait4 with selector",
            raw_selector as u64
        );
        return error!(ENOSYS);
    };

    if let Some(waitable_process) = wait_on_pid(locked, current_task, selector, &waiting_options)? {
        let status = waitable_process.exit_info.status.wait_status();

        if !user_rusage.is_null() {
            track_stub!(TODO("https://fxbug.dev/322874768"), "real rusage from wait4");
            let usage = rusage {
                ru_utime: timeval_from_duration(waitable_process.time_stats.user_time),
                ru_stime: timeval_from_duration(waitable_process.time_stats.system_time),
                ..Default::default()
            };
            current_task.write_object(user_rusage, &usage)?;
        }

        if !user_wstatus.is_null() {
            current_task.write_object(user_wstatus, &status)?;
        }

        Ok(waitable_process.pid)
    } else {
        Ok(0)
    }
}

// Negates the `pid` safely or fails with `ESRCH` (negation operation panics for `i32::MIN`).
fn negate_pid(pid: pid_t) -> Result<pid_t, Errno> {
    pid.checked_neg().ok_or_else(|| errno!(ESRCH))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::PAGE_SIZE;
    use crate::signals::send_standard_signal;
    use crate::signals::testing::dequeue_signal_for_test;
    use crate::task::{ExitStatus, ProcessExitInfo};
    use crate::testing::*;
    use starnix_types::math::round_up_to_system_page_size;
    use starnix_uapi::auth::Credentials;
    use starnix_uapi::errors::ERESTARTSYS;
    use starnix_uapi::signals::{
        SIGCHLD, SIGHUP, SIGINT, SIGIO, SIGKILL, SIGRTMIN, SIGSEGV, SIGSTOP, SIGTERM, SIGTRAP,
        SIGUSR1,
    };
    use starnix_uapi::{uaddr, uid_t, SI_QUEUE};
    use zerocopy::IntoBytes;

    #[cfg(target_arch = "x86_64")]
    #[::fuchsia::test]
    async fn test_sigaltstack() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let user_ss = UserRef::<sigaltstack>::new(addr);
        let nullptr = UserRef::<sigaltstack>::default();

        // Check that the initial state is disabled.
        sys_sigaltstack(&mut locked, &current_task, nullptr, user_ss)
            .expect("failed to call sigaltstack");
        let mut ss = current_task.read_object(user_ss).expect("failed to read struct");
        assert!(ss.ss_flags & (SS_DISABLE as i32) != 0);

        // Install a sigaltstack and read it back out.
        ss.ss_sp = uaddr { addr: 0x7FFFF };
        ss.ss_size = 0x1000;
        ss.ss_flags = SS_AUTODISARM as i32;
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr)
            .expect("failed to call sigaltstack");
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<sigaltstack>()])
            .expect("failed to clear struct");
        sys_sigaltstack(&mut locked, &current_task, nullptr, user_ss)
            .expect("failed to call sigaltstack");
        let another_ss = current_task.read_object(user_ss).expect("failed to read struct");
        assert_eq!(ss.as_bytes(), another_ss.as_bytes());

        // Disable the sigaltstack and read it back out.
        let ss = sigaltstack { ss_flags: SS_DISABLE as i32, ..sigaltstack::default() };
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr)
            .expect("failed to call sigaltstack");
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<sigaltstack>()])
            .expect("failed to clear struct");
        sys_sigaltstack(&mut locked, &current_task, nullptr, user_ss)
            .expect("failed to call sigaltstack");
        let ss = current_task.read_object(user_ss).expect("failed to read struct");
        assert!(ss.ss_flags & (SS_DISABLE as i32) != 0);
    }

    #[::fuchsia::test]
    async fn test_sigaltstack_invalid_size() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let user_ss = UserRef::<sigaltstack>::new(addr);
        let nullptr = UserRef::<sigaltstack>::default();

        // Check that the initial state is disabled.
        sys_sigaltstack(&mut locked, &current_task, nullptr, user_ss)
            .expect("failed to call sigaltstack");
        let mut ss = current_task.read_object(user_ss).expect("failed to read struct");
        assert!(ss.ss_flags & (SS_DISABLE as i32) != 0);

        // Try to install a sigaltstack with an invalid size.
        let sigaltstack_addr_size =
            round_up_to_system_page_size(MINSIGSTKSZ as usize).expect("failed to round up");
        let sigaltstack_addr = map_memory(
            &mut locked,
            &current_task,
            UserAddress::default(),
            sigaltstack_addr_size as u64,
        );
        ss.ss_sp = sigaltstack_addr.into();
        ss.ss_flags = 0;
        ss.ss_size = MINSIGSTKSZ as u64 - 1;
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        assert_eq!(sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr), error!(ENOMEM));
    }

    #[cfg(target_arch = "x86_64")]
    #[::fuchsia::test]
    async fn test_sigaltstack_active_stack() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let user_ss = UserRef::<sigaltstack>::new(addr);
        let nullptr = UserRef::<sigaltstack>::default();

        // Check that the initial state is disabled.
        sys_sigaltstack(&mut locked, &current_task, nullptr, user_ss)
            .expect("failed to call sigaltstack");
        let mut ss = current_task.read_object(user_ss).expect("failed to read struct");
        assert!(ss.ss_flags & (SS_DISABLE as i32) != 0);

        // Try to install a sigaltstack.
        let sigaltstack_addr_size =
            round_up_to_system_page_size(MINSIGSTKSZ as usize).expect("failed to round up");
        let sigaltstack_addr = map_memory(
            &mut locked,
            &current_task,
            UserAddress::default(),
            sigaltstack_addr_size as u64,
        );
        ss.ss_sp = sigaltstack_addr.into();
        ss.ss_flags = 0;
        ss.ss_size = sigaltstack_addr_size as u64;
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr)
            .expect("failed to call sigaltstack");

        // Changing the sigaltstack while we are there should be an error.
        current_task.thread_state.registers.rsp =
            (sigaltstack_addr + sigaltstack_addr_size).ptr() as u64;
        ss.ss_flags = SS_DISABLE as i32;
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        assert_eq!(sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr), error!(EPERM));

        // However, setting the rsp to a different value outside the alt stack should allow us to
        // disable it.
        current_task.thread_state.registers.rsp =
            (sigaltstack_addr + sigaltstack_addr_size + 0x1000usize).ptr() as u64;
        let ss = sigaltstack { ss_flags: SS_DISABLE as i32, ..sigaltstack::default() };
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr)
            .expect("failed to call sigaltstack");
    }

    #[cfg(target_arch = "x86_64")]
    #[::fuchsia::test]
    async fn test_sigaltstack_active_stack_saturates() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let user_ss = UserRef::<sigaltstack>::new(addr);
        let nullptr = UserRef::<sigaltstack>::default();

        // Check that the initial state is disabled.
        sys_sigaltstack(&mut locked, &current_task, nullptr, user_ss)
            .expect("failed to call sigaltstack");
        let mut ss = current_task.read_object(user_ss).expect("failed to read struct");
        assert!(ss.ss_flags & (SS_DISABLE as i32) != 0);

        // Try to install a sigaltstack that takes the whole memory.
        let sigaltstack_addr_size =
            round_up_to_system_page_size(MINSIGSTKSZ as usize).expect("failed to round up");
        let sigaltstack_addr = map_memory(
            &mut locked,
            &current_task,
            UserAddress::default(),
            sigaltstack_addr_size as u64,
        );
        ss.ss_sp = sigaltstack_addr.into();
        ss.ss_flags = 0;
        ss.ss_size = u64::MAX;
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr)
            .expect("failed to call sigaltstack");

        // Changing the sigaltstack while we are there should be an error.
        current_task.thread_state.registers.rsp =
            (sigaltstack_addr + sigaltstack_addr_size).ptr() as u64;
        ss.ss_flags = SS_DISABLE as i32;
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        assert_eq!(sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr), error!(EPERM));

        // However, setting the rsp to a low value should work (it doesn't wrap-around).
        current_task.thread_state.registers.rsp = 0u64;
        let ss = sigaltstack { ss_flags: SS_DISABLE as i32, ..sigaltstack::default() };
        current_task.write_object(user_ss, &ss).expect("failed to write struct");
        sys_sigaltstack(&mut locked, &current_task, user_ss, nullptr)
            .expect("failed to call sigaltstack");
    }

    /// It is invalid to call rt_sigprocmask with a sigsetsize that does not match the size of
    /// SigSet.
    #[::fuchsia::test]
    async fn test_sigprocmask_invalid_size() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let set = UserRef::<SigSet>::default();
        let old_set = UserRef::<SigSet>::default();
        let how = 0;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>() * 2
            ),
            error!(EINVAL)
        );
        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>() / 2
            ),
            error!(EINVAL)
        );
    }

    /// It is invalid to call rt_sigprocmask with a bad `how`.
    #[::fuchsia::test]
    async fn test_sigprocmask_invalid_how() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let set = UserRef::<SigSet>::new(addr);
        let old_set = UserRef::<SigSet>::default();
        let how = SIG_SETMASK | SIG_UNBLOCK | SIG_BLOCK;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            error!(EINVAL)
        );
    }

    /// It is valid to call rt_sigprocmask with a null value for set. In that case, old_set should
    /// contain the current signal mask.
    #[::fuchsia::test]
    async fn test_sigprocmask_null_set() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let original_mask = SigSet::from(SIGTRAP);
        {
            current_task.write().set_signal_mask(original_mask);
        }

        let set = UserRef::<SigSet>::default();
        let old_set = UserRef::<SigSet>::new(addr);
        let how = SIG_SETMASK;

        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>()])
            .expect("failed to clear struct");

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );

        let old_mask = current_task.read_object(old_set).expect("failed to read mask");
        assert_eq!(old_mask, original_mask);
    }

    /// It is valid to call rt_sigprocmask with null values for both set and old_set.
    /// In this case, how should be ignored and the set remains the same.
    #[::fuchsia::test]
    async fn test_sigprocmask_null_set_and_old_set() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let original_mask = SigSet::from(SIGTRAP);
        {
            current_task.write().set_signal_mask(original_mask);
        }

        let set = UserRef::<SigSet>::default();
        let old_set = UserRef::<SigSet>::default();
        let how = SIG_SETMASK;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );
        assert_eq!(current_task.read().signal_mask(), original_mask);
    }

    /// Calling rt_sigprocmask with SIG_SETMASK should set the mask to the provided set.
    #[::fuchsia::test]
    async fn test_sigprocmask_setmask() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>() * 2])
            .expect("failed to clear struct");

        let original_mask = SigSet::from(SIGTRAP);
        {
            current_task.write().set_signal_mask(original_mask);
        }

        let new_mask = SigSet::from(SIGIO);
        let set = UserRef::<SigSet>::new(addr);
        current_task.write_object(set, &new_mask).expect("failed to set mask");

        let old_set = UserRef::<SigSet>::new(addr + std::mem::size_of::<SigSet>());
        let how = SIG_SETMASK;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );

        let old_mask = current_task.read_object(old_set).expect("failed to read mask");
        assert_eq!(old_mask, original_mask);
        assert_eq!(current_task.read().signal_mask(), new_mask);
    }

    /// Calling st_sigprocmask with a how of SIG_BLOCK should add to the existing set.
    #[::fuchsia::test]
    async fn test_sigprocmask_block() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>() * 2])
            .expect("failed to clear struct");

        let original_mask = SigSet::from(SIGTRAP);
        {
            current_task.write().set_signal_mask(original_mask);
        }

        let new_mask = SigSet::from(SIGIO);
        let set = UserRef::<SigSet>::new(addr);
        current_task.write_object(set, &new_mask).expect("failed to set mask");

        let old_set = UserRef::<SigSet>::new(addr + std::mem::size_of::<SigSet>());
        let how = SIG_BLOCK;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );

        let old_mask = current_task.read_object(old_set).expect("failed to read mask");
        assert_eq!(old_mask, original_mask);
        assert_eq!(current_task.read().signal_mask(), new_mask | original_mask);
    }

    /// Calling st_sigprocmask with a how of SIG_UNBLOCK should remove from the existing set.
    #[::fuchsia::test]
    async fn test_sigprocmask_unblock() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>() * 2])
            .expect("failed to clear struct");

        let original_mask = SigSet::from(SIGTRAP) | SigSet::from(SIGIO);
        {
            current_task.write().set_signal_mask(original_mask);
        }

        let new_mask = SigSet::from(SIGTRAP);
        let set = UserRef::<SigSet>::new(addr);
        current_task.write_object(set, &new_mask).expect("failed to set mask");

        let old_set = UserRef::<SigSet>::new(addr + std::mem::size_of::<SigSet>());
        let how = SIG_UNBLOCK;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );

        let old_mask = current_task.read_object(old_set).expect("failed to read mask");
        assert_eq!(old_mask, original_mask);
        assert_eq!(current_task.read().signal_mask(), SIGIO.into());
    }

    /// It's ok to call sigprocmask to unblock a signal that is not set.
    #[::fuchsia::test]
    async fn test_sigprocmask_unblock_not_set() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>() * 2])
            .expect("failed to clear struct");

        let original_mask = SigSet::from(SIGIO);
        {
            current_task.write().set_signal_mask(original_mask);
        }

        let new_mask = SigSet::from(SIGTRAP);
        let set = UserRef::<SigSet>::new(addr);
        current_task.write_object(set, &new_mask).expect("failed to set mask");

        let old_set = UserRef::<SigSet>::new(addr + std::mem::size_of::<SigSet>());
        let how = SIG_UNBLOCK;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );

        let old_mask = current_task.read_object(old_set).expect("failed to read mask");
        assert_eq!(old_mask, original_mask);
        assert_eq!(current_task.read().signal_mask(), original_mask);
    }

    /// It's not possible to block SIGKILL or SIGSTOP.
    #[::fuchsia::test]
    async fn test_sigprocmask_kill_stop() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>() * 2])
            .expect("failed to clear struct");

        let original_mask = SigSet::from(SIGIO);
        {
            current_task.write().set_signal_mask(original_mask);
        }

        let new_mask = UNBLOCKABLE_SIGNALS;
        let set = UserRef::<SigSet>::new(addr);
        current_task.write_object(set, &new_mask).expect("failed to set mask");

        let old_set = UserRef::<SigSet>::new(addr + std::mem::size_of::<SigSet>());
        let how = SIG_BLOCK;

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                how,
                set,
                old_set,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );

        let old_mask = current_task.read_object(old_set).expect("failed to read mask");
        assert_eq!(old_mask, original_mask);
        assert_eq!(current_task.read().signal_mask(), original_mask);
    }

    #[::fuchsia::test]
    async fn test_sigaction_invalid_signal() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        assert_eq!(
            sys_rt_sigaction(
                &mut locked,
                &current_task,
                UncheckedSignal::from(SIGKILL),
                // The signal is only checked when the action is set (i.e., action is non-null).
                UserRef::<sigaction>::new(UserAddress::from(10)),
                UserRef::<sigaction>::default(),
                std::mem::size_of::<SigSet>(),
            ),
            error!(EINVAL)
        );
        assert_eq!(
            sys_rt_sigaction(
                &mut locked,
                &current_task,
                UncheckedSignal::from(SIGSTOP),
                // The signal is only checked when the action is set (i.e., action is non-null).
                UserRef::<sigaction>::new(UserAddress::from(10)),
                UserRef::<sigaction>::default(),
                std::mem::size_of::<SigSet>(),
            ),
            error!(EINVAL)
        );
        assert_eq!(
            sys_rt_sigaction(
                &mut locked,
                &current_task,
                UncheckedSignal::from(Signal::NUM_SIGNALS + 1),
                // The signal is only checked when the action is set (i.e., action is non-null).
                UserRef::<sigaction>::new(UserAddress::from(10)),
                UserRef::<sigaction>::default(),
                std::mem::size_of::<SigSet>(),
            ),
            error!(EINVAL)
        );
    }

    #[::fuchsia::test]
    async fn test_sigaction_old_value_set() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<sigaction>()])
            .expect("failed to clear struct");

        let org_mask = SigSet::from(SIGHUP) | SigSet::from(SIGINT);
        let original_action = sigaction { sa_mask: org_mask.into(), ..sigaction::default() };

        {
            current_task.thread_group.signal_actions.set(SIGHUP, original_action);
        }

        let old_action_ref = UserRef::<sigaction>::new(addr);
        assert_eq!(
            sys_rt_sigaction(
                &mut locked,
                &current_task,
                UncheckedSignal::from(SIGHUP),
                UserRef::<sigaction>::default(),
                old_action_ref,
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );

        let old_action = current_task.read_object(old_action_ref).expect("failed to read action");
        assert_eq!(old_action.as_bytes(), original_action.as_bytes());
    }

    #[::fuchsia::test]
    async fn test_sigaction_new_value_set() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<sigaction>()])
            .expect("failed to clear struct");

        let org_mask = SigSet::from(SIGHUP) | SigSet::from(SIGINT);
        let original_action = sigaction { sa_mask: org_mask.into(), ..sigaction::default() };
        let set_action_ref = UserRef::<sigaction>::new(addr);
        current_task.write_object(set_action_ref, &original_action).expect("failed to set action");

        assert_eq!(
            sys_rt_sigaction(
                &mut locked,
                &current_task,
                UncheckedSignal::from(SIGINT),
                set_action_ref,
                UserRef::<sigaction>::default(),
                std::mem::size_of::<SigSet>(),
            ),
            Ok(())
        );

        assert_eq!(
            current_task.thread_group.signal_actions.get(SIGINT).as_bytes(),
            original_action.as_bytes()
        );
    }

    /// A task should be able to signal itself.
    #[::fuchsia::test]
    async fn test_kill_same_task() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        assert_eq!(sys_kill(&mut locked, &current_task, current_task.id, SIGINT.into()), Ok(()));
    }

    /// A task should be able to signal its own thread group.
    #[::fuchsia::test]
    async fn test_kill_own_thread_group() {
        let (_kernel, init_task, mut locked) = create_kernel_task_and_unlocked();
        let task1 = init_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task1.thread_group.setsid(&mut locked).expect("setsid");
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));

        assert_eq!(sys_kill(&mut locked, &task1, 0, SIGINT.into()), Ok(()));
        assert_eq!(task1.read().queued_signal_count(SIGINT), 1);
        assert_eq!(task2.read().queued_signal_count(SIGINT), 1);
        assert_eq!(init_task.read().queued_signal_count(SIGINT), 0);
    }

    /// A task should be able to signal a thread group.
    #[::fuchsia::test]
    async fn test_kill_thread_group() {
        let (_kernel, init_task, mut locked) = create_kernel_task_and_unlocked();
        let task1 = init_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task1.thread_group.setsid(&mut locked).expect("setsid");
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));

        assert_eq!(sys_kill(&mut locked, &task1, -task1.id, SIGINT.into()), Ok(()));
        assert_eq!(task1.read().queued_signal_count(SIGINT), 1);
        assert_eq!(task2.read().queued_signal_count(SIGINT), 1);
        assert_eq!(init_task.read().queued_signal_count(SIGINT), 0);
    }

    /// A task should be able to signal everything but init and itself.
    #[::fuchsia::test]
    async fn test_kill_all() {
        let (_kernel, init_task, mut locked) = create_kernel_task_and_unlocked();
        let task1 = init_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task1.thread_group.setsid(&mut locked).expect("setsid");
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));

        assert_eq!(sys_kill(&mut locked, &task1, -1, SIGINT.into()), Ok(()));
        assert_eq!(task1.read().queued_signal_count(SIGINT), 0);
        assert_eq!(task2.read().queued_signal_count(SIGINT), 1);
        assert_eq!(init_task.read().queued_signal_count(SIGINT), 0);
    }

    /// A task should not be able to signal a nonexistent task.
    #[::fuchsia::test]
    async fn test_kill_inexistant_task() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        assert_eq!(sys_kill(&mut locked, &current_task, 9, SIGINT.into()), error!(ESRCH));
    }

    /// A task should not be able to signal a task owned by another uid.
    #[::fuchsia::test]
    async fn test_kill_invalid_task() {
        let (_kernel, task1, mut locked) = create_kernel_task_and_unlocked();
        // Task must not have the kill capability.
        task1.set_creds(Credentials::with_ids(1, 1));
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task2.set_creds(Credentials::with_ids(2, 2));

        assert!(!task1.can_signal(&task2, SIGINT.into()));
        assert_eq!(sys_kill(&mut locked, &task2, task1.id, SIGINT.into()), error!(EPERM));
        assert_eq!(task1.read().queued_signal_count(SIGINT), 0);
    }

    /// A task should not be able to signal a task owned by another uid in a thead group.
    #[::fuchsia::test]
    async fn test_kill_invalid_task_in_thread_group() {
        let (_kernel, init_task, mut locked) = create_kernel_task_and_unlocked();
        let task1 = init_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task1.thread_group.setsid(&mut locked).expect("setsid");
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task2.thread_group.setsid(&mut locked).expect("setsid");
        task2.set_creds(Credentials::with_ids(2, 2));

        assert!(!task2.can_signal(&task1, SIGINT.into()));
        assert_eq!(sys_kill(&mut locked, &task2, -task1.id, SIGINT.into()), error!(EPERM));
        assert_eq!(task1.read().queued_signal_count(SIGINT), 0);
    }

    /// A task should not be able to send an invalid signal.
    #[::fuchsia::test]
    async fn test_kill_invalid_signal() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        assert_eq!(
            sys_kill(&mut locked, &current_task, current_task.id, UncheckedSignal::from(75)),
            error!(EINVAL)
        );
    }

    /// Sending a blocked signal should result in a pending signal.
    #[::fuchsia::test]
    async fn test_blocked_signal_pending() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>() * 2])
            .expect("failed to clear struct");

        let new_mask = SigSet::from(SIGIO);
        let set = UserRef::<SigSet>::new(addr);
        current_task.write_object(set, &new_mask).expect("failed to set mask");

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                SIG_BLOCK,
                set,
                UserRef::default(),
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );
        assert_eq!(sys_kill(&mut locked, &current_task, current_task.id, SIGIO.into()), Ok(()));
        assert_eq!(current_task.read().queued_signal_count(SIGIO), 1);

        // A second signal should not increment the number of pending signals.
        assert_eq!(sys_kill(&mut locked, &current_task, current_task.id, SIGIO.into()), Ok(()));
        assert_eq!(current_task.read().queued_signal_count(SIGIO), 1);
    }

    /// More than one instance of a real-time signal can be blocked.
    #[::fuchsia::test]
    async fn test_blocked_real_time_signal_pending() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(addr, &[0u8; std::mem::size_of::<SigSet>() * 2])
            .expect("failed to clear struct");

        let new_mask = SigSet::from(starnix_uapi::signals::SIGRTMIN);
        let set = UserRef::<SigSet>::new(addr);
        current_task.write_object(set, &new_mask).expect("failed to set mask");

        assert_eq!(
            sys_rt_sigprocmask(
                &mut locked,
                &current_task,
                SIG_BLOCK,
                set,
                UserRef::default(),
                std::mem::size_of::<SigSet>()
            ),
            Ok(())
        );
        assert_eq!(sys_kill(&mut locked, &current_task, current_task.id, SIGRTMIN.into()), Ok(()));
        assert_eq!(current_task.read().queued_signal_count(starnix_uapi::signals::SIGRTMIN), 1);

        // A second signal should increment the number of pending signals.
        assert_eq!(sys_kill(&mut locked, &current_task, current_task.id, SIGRTMIN.into()), Ok(()));
        assert_eq!(current_task.read().queued_signal_count(starnix_uapi::signals::SIGRTMIN), 2);
    }

    #[::fuchsia::test]
    async fn test_suspend() {
        let (kernel, mut init_task, mut locked) = create_kernel_task_and_unlocked();
        let init_task_weak = init_task.weak_task();
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(0);

        let thread = kernel.kthreads.spawner().spawn_and_get_result(move |locked, current_task| {
            let init_task_temp = init_task_weak.upgrade().expect("Task must be alive");

            // Wait for the init task to be suspended.
            let mut suspended = false;
            while !suspended {
                suspended = init_task_temp.read().is_blocked();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            // Signal the suspended task with a signal that is not blocked (only SIGHUP in this test).
            let _ =
                sys_kill(locked, current_task, init_task_temp.id, UncheckedSignal::from(SIGHUP));

            // Wait for the sigsuspend to complete.
            rx.recv().expect("receive");
            assert!(!init_task_temp.read().is_blocked());
        });

        let addr = map_memory(&mut locked, &init_task, UserAddress::default(), *PAGE_SIZE);
        let user_ref = UserRef::<SigSet>::new(addr);

        let sigset = !SigSet::from(SIGHUP);
        init_task.write_object(user_ref, &sigset).expect("failed to set action");

        assert_eq!(
            sys_rt_sigsuspend(&mut locked, &mut init_task, user_ref, std::mem::size_of::<SigSet>()),
            error!(ERESTARTNOHAND)
        );
        tx.send(()).expect("send");
        thread.await.expect("join");
    }

    /// Waitid does not support all options.
    #[::fuchsia::test]
    async fn test_waitid_options() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let id = 1;
        assert_eq!(
            sys_waitid(
                &mut locked,
                &current_task,
                P_PID,
                id,
                UserAddress::default(),
                0,
                UserRef::default()
            ),
            error!(EINVAL)
        );
        assert_eq!(
            sys_waitid(
                &mut locked,
                &current_task,
                P_PID,
                id,
                UserAddress::default(),
                0xffff,
                UserRef::default()
            ),
            error!(EINVAL)
        );
    }

    /// Wait4 does not support all options.
    #[::fuchsia::test]
    async fn test_wait4_options() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let id = 1;
        assert_eq!(
            sys_wait4(
                &mut locked,
                &current_task,
                id,
                UserRef::default(),
                WEXITED,
                UserRef::default()
            ),
            error!(EINVAL)
        );
        assert_eq!(
            sys_wait4(
                &mut locked,
                &current_task,
                id,
                UserRef::default(),
                WNOWAIT,
                UserRef::default()
            ),
            error!(EINVAL)
        );
        assert_eq!(
            sys_wait4(
                &mut locked,
                &current_task,
                id,
                UserRef::default(),
                0xffff,
                UserRef::default()
            ),
            error!(EINVAL)
        );
    }

    #[::fuchsia::test]
    async fn test_echild_when_no_zombie() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        // Send the signal to the task.
        assert!(sys_kill(
            &mut locked,
            &current_task,
            current_task.get_pid(),
            UncheckedSignal::from(SIGCHLD)
        )
        .is_ok());
        // Verify that ECHILD is returned because there is no zombie process and no children to
        // block waiting for.
        assert_eq!(
            wait_on_pid(
                &mut locked,
                &current_task,
                ProcessSelector::Any,
                &WaitingOptions::new_for_wait4(0, 0).expect("WaitingOptions")
            ),
            error!(ECHILD)
        );
    }

    #[::fuchsia::test]
    async fn test_no_error_when_zombie() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let child = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let expected_result = WaitResult {
            pid: child.id,
            uid: 0,
            exit_info: ProcessExitInfo { status: ExitStatus::Exit(1), exit_signal: Some(SIGCHLD) },
            time_stats: Default::default(),
        };
        child.thread_group.exit(&mut locked, ExitStatus::Exit(1), None);
        std::mem::drop(child);

        assert_eq!(
            wait_on_pid(
                &mut locked,
                &current_task,
                ProcessSelector::Any,
                &WaitingOptions::new_for_wait4(0, 0).expect("WaitingOptions")
            ),
            Ok(Some(expected_result))
        );
    }

    #[::fuchsia::test]
    async fn test_waiting_for_child() {
        let (_kernel, task, mut locked) = create_kernel_task_and_unlocked();

        let child = task
            .clone_task(&mut locked, 0, Some(SIGCHLD), UserRef::default(), UserRef::default())
            .expect("clone_task");

        // No child is currently terminated.
        assert_eq!(
            wait_on_pid(
                &mut locked,
                &task,
                ProcessSelector::Any,
                &WaitingOptions::new_for_wait4(WNOHANG, 0).expect("WaitingOptions")
            ),
            Ok(None)
        );

        let thread = std::thread::spawn({
            let task = task.weak_task();
            move || {
                // Create child
                let mut locked = Unlocked::new();
                let task = task.upgrade().expect("task must be alive");
                let child: AutoReleasableTask = child.into();
                // Wait for the main thread to be blocked on waiting for a child.
                while !task.read().is_blocked() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                child.thread_group.exit(&mut locked, ExitStatus::Exit(0), None);
                child.id
            }
        });

        // Block until child is terminated.
        let waited_child = wait_on_pid(
            &mut locked,
            &task,
            ProcessSelector::Any,
            &WaitingOptions::new_for_wait4(0, 0).expect("WaitingOptions"),
        )
        .expect("wait_on_pid")
        .unwrap();

        // Child is deleted, the thread must be able to terminate.
        let child_id = thread.join().expect("join");
        assert_eq!(waited_child.pid, child_id);
    }

    #[::fuchsia::test]
    async fn test_waiting_for_child_with_signal_pending() {
        let (_kernel, task, mut locked) = create_kernel_task_and_unlocked();

        // Register a signal action to ensure that the `SIGUSR1` signal interrupts the task.
        task.thread_group.signal_actions.set(
            SIGUSR1,
            sigaction { sa_handler: uaddr { addr: 0xDEADBEEF }, ..sigaction::default() },
        );

        // Start a child task. This will ensure that `wait_on_pid` tries to wait for the child.
        let _child = task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));

        // Send a signal to the task. `wait_on_pid` should realize there is a signal pending when
        // entering a wait and return with `EINTR`.
        send_standard_signal(&task, SignalInfo::default(SIGUSR1));

        let errno = wait_on_pid(
            &mut locked,
            &task,
            ProcessSelector::Any,
            &WaitingOptions::new_for_wait4(0, 0).expect("WaitingOptions"),
        )
        .expect_err("wait_on_pid");
        assert_eq!(errno, ERESTARTSYS);
    }

    #[::fuchsia::test]
    async fn test_sigkill() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mut child = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));

        // Send SIGKILL to the child. As kill is handled immediately, no need to dequeue signals.
        send_standard_signal(&child, SignalInfo::default(SIGKILL));
        dequeue_signal_for_test(&mut locked, &mut child);
        std::mem::drop(child);

        // Retrieve the exit status.
        let address = map_memory(
            &mut locked,
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<i32>() as u64,
        );
        let address_ref = UserRef::<i32>::new(address);
        sys_wait4(&mut locked, &current_task, -1, address_ref, 0, UserRef::default())
            .expect("wait4");
        let wstatus = current_task.read_object(address_ref).expect("read memory");
        assert_eq!(wstatus, SIGKILL.number() as i32);
    }

    fn test_exit_status_for_signal(sig: Signal, wait_status: i32, exit_signal: Option<Signal>) {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mut child = current_task.clone_task_for_test(&mut locked, 0, exit_signal);

        // Send the signal to the child.
        send_standard_signal(&child, SignalInfo::default(sig));
        dequeue_signal_for_test(&mut locked, &mut child);
        std::mem::drop(child);

        // Retrieve the exit status.
        let address = map_memory(
            &mut locked,
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<i32>() as u64,
        );
        let address_ref = UserRef::<i32>::new(address);
        sys_wait4(&mut locked, &current_task, -1, address_ref, 0, UserRef::default())
            .expect("wait4");
        let wstatus = current_task.read_object(address_ref).expect("read memory");
        assert_eq!(wstatus, wait_status);
    }

    #[::fuchsia::test]
    async fn test_exit_status() {
        // Default action is Terminate
        test_exit_status_for_signal(SIGTERM, SIGTERM.number() as i32, Some(SIGCHLD));
        // Default action is CoreDump
        test_exit_status_for_signal(SIGSEGV, (SIGSEGV.number() as i32) | 0x80, Some(SIGCHLD));
    }

    #[::fuchsia::test]
    async fn test_wait4_by_pgid() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let child1 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let child1_pid = child1.id;
        child1.thread_group.exit(&mut locked, ExitStatus::Exit(42), None);
        std::mem::drop(child1);
        let child2 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        child2.thread_group.setsid(&mut locked).expect("setsid");
        let child2_pid = child2.id;
        child2.thread_group.exit(&mut locked, ExitStatus::Exit(42), None);
        std::mem::drop(child2);

        assert_eq!(
            sys_wait4(
                &mut locked,
                &current_task,
                -child2_pid,
                UserRef::default(),
                0,
                UserRef::default()
            ),
            Ok(child2_pid)
        );
        assert_eq!(
            sys_wait4(&mut locked, &current_task, 0, UserRef::default(), 0, UserRef::default()),
            Ok(child1_pid)
        );
    }

    #[::fuchsia::test]
    async fn test_waitid_by_pgid() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let child1 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let child1_pid = child1.id;
        child1.thread_group.exit(&mut locked, ExitStatus::Exit(42), None);
        std::mem::drop(child1);
        let child2 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        child2.thread_group.setsid(&mut locked).expect("setsid");
        let child2_pid = child2.id;
        child2.thread_group.exit(&mut locked, ExitStatus::Exit(42), None);
        std::mem::drop(child2);

        let address = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        assert_eq!(
            sys_waitid(
                &mut locked,
                &current_task,
                P_PGID,
                child2_pid,
                address,
                WEXITED,
                UserRef::default()
            ),
            Ok(())
        );
        // The previous wait matched child2, only child1 should be in the available zombies.
        assert_eq!(current_task.thread_group.read().zombie_children[0].pid, child1_pid);

        assert_eq!(
            sys_waitid(&mut locked, &current_task, P_PGID, 0, address, WEXITED, UserRef::default()),
            Ok(())
        );
    }

    #[::fuchsia::test]
    async fn test_sigqueue() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let current_uid = current_task.creds().uid;
        let current_pid = current_task.get_pid();

        const TEST_VALUE: u64 = 101;

        // Taken from gVisor of SignalInfo in  //pkg/abi/linux/signal.go
        const PID_DATA_OFFSET: usize = SI_HEADER_SIZE;
        const UID_DATA_OFFSET: usize = SI_HEADER_SIZE + 4;
        const VALUE_DATA_OFFSET: usize = SI_HEADER_SIZE + 8;

        let mut data = vec![0u8; SI_MAX_SIZE as usize];
        let header = SignalInfoHeader {
            signo: SIGIO.number(),
            code: SI_QUEUE,
            ..SignalInfoHeader::default()
        };
        let _ = header.write_to(&mut data[..SI_HEADER_SIZE]);
        data[PID_DATA_OFFSET..PID_DATA_OFFSET + 4].copy_from_slice(&current_pid.to_ne_bytes());
        data[UID_DATA_OFFSET..UID_DATA_OFFSET + 4].copy_from_slice(&current_uid.to_ne_bytes());
        data[VALUE_DATA_OFFSET..VALUE_DATA_OFFSET + 8].copy_from_slice(&TEST_VALUE.to_ne_bytes());

        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task.write_memory(addr, &data).unwrap();
        let second_current = create_task(&mut locked, &kernel, "second task");
        let second_pid = second_current.get_pid();
        let second_tid = second_current.get_tid();
        assert_eq!(second_current.read().queued_signal_count(SIGIO), 0);

        assert_eq!(
            sys_rt_tgsigqueueinfo(
                &mut locked,
                &current_task,
                second_pid,
                second_tid,
                UncheckedSignal::from(SIGIO),
                addr
            ),
            Ok(())
        );
        assert_eq!(second_current.read().queued_signal_count(SIGIO), 1);

        let signal = SignalInfo {
            code: SI_USER as i32,
            detail: SignalDetail::Kill {
                pid: current_task.thread_group.leader,
                uid: current_task.creds().uid,
            },
            ..SignalInfo::default(SIGIO)
        };
        let queued_signal = second_current.write().take_specific_signal(signal);
        if let Some(sig) = queued_signal {
            assert_eq!(sig.signal, SIGIO);
            assert_eq!(sig.errno, 0);
            assert_eq!(sig.code, SI_QUEUE);
            if let SignalDetail::Raw { data } = sig.detail {
                // offsets into the raw portion of the signal info
                let offset_pid = PID_DATA_OFFSET - SI_HEADER_SIZE;
                let offset_uid = UID_DATA_OFFSET - SI_HEADER_SIZE;
                let offset_value = VALUE_DATA_OFFSET - SI_HEADER_SIZE;
                let pid =
                    pid_t::from_ne_bytes(data[offset_pid..offset_pid + 4].try_into().unwrap());
                let uid =
                    uid_t::from_ne_bytes(data[offset_uid..offset_uid + 4].try_into().unwrap());
                let value =
                    u64::from_ne_bytes(data[offset_value..offset_value + 8].try_into().unwrap());
                assert_eq!(pid, current_pid);
                assert_eq!(uid, current_uid);
                assert_eq!(value, TEST_VALUE);
            } else {
                panic!("incorrect signal detail");
            }
        } else {
            panic!("expected a queued signal");
        }
    }
}
