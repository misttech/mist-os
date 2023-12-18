// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::{DumpPolicy, MemoryAccessorExt},
    signals::{
        send_signal_first, send_standard_signal, syscalls::WaitingOptions, SignalDetail,
        SignalInfo, SignalInfoHeader, SI_HEADER_SIZE,
    },
    task::{waiter::WaitQueue, CurrentTask, Kernel, StopState, Task, ThreadGroup},
    vfs::parse_unsigned_file,
};
use starnix_logging::not_implemented;
use starnix_sync::{LockBefore, Locked, MmDumpable};
use starnix_uapi::{
    auth::{CAP_SYS_PTRACE, PTRACE_MODE_ATTACH_REALCREDS},
    errno, error,
    errors::Errno,
    ownership::{OwnedRef, WeakRef},
    pid_t,
    signals::{SigSet, Signal, UncheckedSignal, SIGKILL, SIGSTOP, SIGTRAP},
    user_address::{UserAddress, UserRef},
    PTRACE_CONT, PTRACE_DETACH, PTRACE_EVENT_STOP, PTRACE_GETSIGINFO, PTRACE_GETSIGMASK,
    PTRACE_INTERRUPT, PTRACE_KILL, PTRACE_LISTEN, PTRACE_PEEKDATA, PTRACE_PEEKTEXT,
    PTRACE_POKEDATA, PTRACE_POKETEXT, PTRACE_SETSIGINFO, PTRACE_SETSIGMASK, SI_MAX_SIZE,
};
use std::sync::{atomic::Ordering, Arc};
use zerocopy::FromBytes;

/// For most of the time, for the purposes of ptrace, a tracee is either "going"
/// or "stopped".  However, after certain ptrace calls, there are special rules
/// to be followed.
#[derive(Clone, Default, PartialEq)]
pub enum PtraceStatus {
    /// Proceed as otherwise indicated by the task's stop status.
    #[default]
    Default,
    /// Resuming after a ptrace_cont with a signal, so do not stop for signal-delivery-stop
    Continuing,
    /// "The state of the tracee after PTRACE_LISTEN is somewhat of a
    /// gray area: it is not in any ptrace-stop (ptrace commands won't work on it,
    /// and it will deliver waitpid(2) notifications), but it also may be considered
    /// "stopped" because it is not executing instructions (is not scheduled), and
    /// if it was in group-stop before PTRACE_LISTEN, it will not respond to signals
    /// until SIGCONT is received."
    Listening,
}

/// Indicates the way that ptrace attached to the task.
#[derive(Copy, Clone, PartialEq)]
pub enum PtraceAttachType {
    /// Attached with PTRACE_ATTACH
    Attach,
    /// Attached with PTRACE_SEIZE
    Seize,
}

/// Per-task ptrace-related state
pub struct PtraceState {
    /// The pid of the tracer
    pub pid: pid_t,

    /// The tracee waits on this WaitQueue to find out when it should stop or wake
    /// for ptrace-related shenanigans.
    pub tracee_waiters: WaitQueue,

    /// The tracer waits on this WaitQueue to find out if the tracee has done
    /// something worth being notified about.
    pub tracer_waiters: WaitQueue,

    /// The signal that caused the task to enter the given state (for
    /// signal-delivery-stop)
    pub last_signal: Option<SignalInfo>,

    /// Whether waitpid() will return the last signal.  The presence of last_signal
    /// can't be used for that, because that needs to be saved for GETSIGINFO.
    pub last_signal_waitable: bool,

    /// Whether the attach was a seize or an attach.  There are a few subtle
    /// differences in behavior of the different attach types - see ptrace(2).
    pub attach_type: PtraceAttachType,

    /// Indicates whether the last ptrace call put this thread into a state with
    /// special semantics for stopping behavior.
    pub stop_status: PtraceStatus,
}

impl PtraceState {
    pub fn new(pid: pid_t, attach_type: PtraceAttachType) -> Self {
        PtraceState {
            pid,
            tracee_waiters: WaitQueue::default(),
            tracer_waiters: WaitQueue::default(),
            last_signal: None,
            last_signal_waitable: false,
            attach_type,
            stop_status: PtraceStatus::default(),
        }
    }

    pub fn is_seized(&self) -> bool {
        self.attach_type == PtraceAttachType::Seize
    }

    pub fn is_waitable(&self, stop: StopState, options: &WaitingOptions) -> bool {
        if self.stop_status == PtraceStatus::Listening {
            // Waiting for any change of state
            return self.last_signal_waitable;
        }
        if !options.wait_for_continued && !stop.is_stopping_or_stopped() {
            // Only waiting for stops, but is not stopped.
            return false;
        }
        self.last_signal_waitable && !stop.is_in_progress()
    }

    pub fn set_last_signal(&mut self, mut signal: Option<SignalInfo>) {
        if let Some(ref mut siginfo) = signal {
            // We don't want waiters to think the process was unstopped because
            // of a sigkill. They will get woken when the process dies.
            if siginfo.signal == SIGKILL {
                return;
            }
            if self.is_seized() {
                siginfo.code |= (PTRACE_EVENT_STOP << 8) as i32;
            }
            self.last_signal_waitable = true;
            self.last_signal = signal;
        }
    }

    pub fn get_last_signal(&mut self, keep_signal_waitable: bool) -> Option<SignalInfo> {
        self.last_signal_waitable = keep_signal_waitable;
        self.last_signal.clone()
    }
}

/// Scope definitions for Yama.  For full details, see ptrace(2).
/// 1 means tracer needs to have CAP_SYS_PTRACE or be a parent / child
/// process. This is the default.
const RESTRICTED_SCOPE: u8 = 1;
/// 2 means tracer needs to have CAP_SYS_PTRACE
const ADMIN_ONLY_SCOPE: u8 = 2;
/// 3 means no process can attach.
const NO_ATTACH_SCOPE: u8 = 3;

// PR_SET_PTRACER_ANY is defined as ((unsigned long) -1),
// which is not understood by bindgen.
pub const PR_SET_PTRACER_ANY: i32 = -1;

/// Indicates processes specifically allowed to trace a given process if using
/// RESTRICTED_SCOPE.  Used by prctl(PR_SET_PTRACER).
#[derive(Copy, Clone, Default, PartialEq)]
pub enum PtraceAllowedPtracers {
    #[default]
    None,
    Some(pid_t),
    Any,
}

/// Continues the target thread, optionally detaching from it.
fn ptrace_cont(tracee: &Task, data: &UserAddress, detach: bool) -> Result<(), Errno> {
    let data = data.ptr() as u64;
    let new_state;
    let mut siginfo = if data != 0 {
        let signal = Signal::try_from(UncheckedSignal::new(data))?;
        Some(SignalInfo::default(signal))
    } else {
        None
    };

    let mut state = tracee.write();
    let is_listen = state.is_ptrace_listening();

    if tracee.load_stopped().is_waking_or_awake() && !is_listen {
        if detach {
            state.set_ptrace(None)?;
        }
        return error!(EIO);
    }

    if !state.can_accept_ptrace_commands() && !detach {
        return error!(ESRCH);
    }

    if let Some(ref mut ptrace) = &mut state.ptrace {
        if data != 0 {
            new_state = PtraceStatus::Continuing;
            if let Some(ref mut last_signal) = &mut ptrace.last_signal {
                if let Some(si) = siginfo {
                    let new_signal = si.signal;
                    last_signal.signal = new_signal;
                }
                siginfo = Some(last_signal.clone());
            }
        } else {
            new_state = PtraceStatus::Default;
            ptrace.last_signal = None;
        }
        ptrace.stop_status = new_state;

        if is_listen {
            state.notify_ptracees();
        }
    }

    state.set_stopped(StopState::Waking, None);

    if detach {
        state.set_ptrace(None)?;
    }
    drop(state);
    tracee.thread_group.set_stopped(StopState::Waking, None, false);

    if let Some(siginfo) = siginfo {
        // siginfo is replacing the signal that caused the ptrace-stop we're currently
        // in, so has to go first.
        send_signal_first(&tracee, siginfo);
    }
    Ok(())
}

fn ptrace_interrupt(tracee: &Task) -> Result<(), Errno> {
    let mut state = tracee.write();
    if let Some(ref mut ptrace) = &mut state.ptrace {
        if !ptrace.is_seized() {
            return error!(EIO);
        }
        let status = ptrace.stop_status.clone();
        ptrace.stop_status = PtraceStatus::Default;
        if status == PtraceStatus::Listening {
            let signal = ptrace.last_signal.clone();
            // "If the tracee was already stopped by a signal and PTRACE_LISTEN
            // was sent to it, the tracee stops with PTRACE_EVENT_STOP and
            // WSTOPSIG(status) returns the stop signal"
            state.set_stopped(StopState::PtraceEventStopped, signal);
        } else {
            state.set_stopped(StopState::PtraceEventStopped, Some(SignalInfo::default(SIGTRAP)));
            drop(state);
            tracee.interrupt();
        }
    }
    Ok(())
}

fn ptrace_listen(tracee: &Task) -> Result<(), Errno> {
    let mut state = tracee.write();
    if let Some(ref mut ptrace) = &mut state.ptrace {
        if !ptrace.is_seized()
            || (ptrace.last_signal_waitable
                && ptrace
                    .last_signal
                    .as_ref()
                    .map_or(false, |ls| ls.code >> 8 != PTRACE_EVENT_STOP as i32))
        {
            return error!(EIO);
        }
        ptrace.stop_status = PtraceStatus::Listening;
    }
    Ok(())
}

pub fn ptrace_detach(
    thread_group: &ThreadGroup,
    tracee: &Task,
    data: &UserAddress,
) -> Result<(), Errno> {
    if let Err(x) = ptrace_cont(&tracee, &data, true) {
        return Err(x);
    }
    thread_group.ptracees.lock().remove(&tracee.get_tid());
    Ok(())
}

/// For all ptrace requests that require an attached tracee
pub fn ptrace_dispatch(
    current_task: &mut CurrentTask,
    request: u32,
    pid: pid_t,
    addr: UserAddress,
    data: UserAddress,
) -> Result<(), Errno> {
    let weak_init = current_task.kernel().pids.read().get_task(pid);
    let tracee = weak_init.upgrade().ok_or_else(|| errno!(ESRCH))?;

    if let Some(ptrace) = &tracee.read().ptrace {
        if ptrace.pid != current_task.get_tid() {
            return error!(ESRCH);
        }
    }

    // These requests may be run without the thread in a stop state, or
    // check the stop state themselves.
    match request {
        PTRACE_KILL => {
            let mut siginfo = SignalInfo::default(SIGKILL);
            siginfo.code = (linux_uapi::SIGTRAP | PTRACE_KILL << 8) as i32;
            send_standard_signal(&tracee, siginfo);
            return Ok(());
        }
        PTRACE_INTERRUPT => {
            ptrace_interrupt(tracee.as_ref())?;
            return Ok(());
        }
        PTRACE_LISTEN => {
            ptrace_listen(&tracee)?;
            return Ok(());
        }
        PTRACE_CONT => {
            ptrace_cont(&tracee, &data, false)?;
            return Ok(());
        }
        PTRACE_DETACH => {
            return ptrace_detach(current_task.thread_group.as_ref(), tracee.as_ref(), &data)
        }
        _ => {}
    }

    // The remaining requests (to be added) require the thread to be stopped.
    let mut state = tracee.write();
    if !state.can_accept_ptrace_commands() {
        return error!(ESRCH);
    }

    match request {
        PTRACE_PEEKDATA | PTRACE_PEEKTEXT => {
            // NB: The behavior of the syscall is different from the behavior in ptrace(2),
            // which is provided by libc.
            let src: UserRef<usize> = UserRef::from(addr);
            let val = tracee.mm().vmo_read_object(src)?;

            let dst: UserRef<usize> = UserRef::from(data);
            current_task.mm().write_object(dst, &val)?;
            Ok(())
        }
        PTRACE_POKEDATA | PTRACE_POKETEXT => {
            let ptr: UserRef<usize> = UserRef::from(addr);
            let val = data.ptr() as usize;
            tracee.mm().vmo_write_object(ptr, &val)?;
            Ok(())
        }
        PTRACE_SETSIGMASK => {
            // addr is the size of the buffer pointed to
            // by data, but has to be sizeof(sigset_t).
            if addr.ptr() != std::mem::size_of::<SigSet>() {
                return error!(EINVAL);
            }
            // sigset comes from *data.
            let src: UserRef<SigSet> = UserRef::from(data);
            let val = current_task.mm().read_object(src)?;
            state.signals.set_mask(val);

            Ok(())
        }
        PTRACE_GETSIGMASK => {
            // addr is the size of the buffer pointed to
            // by data, but has to be sizeof(sigset_t).
            if addr.ptr() != std::mem::size_of::<SigSet>() {
                return error!(EINVAL);
            }
            // sigset goes in *data.
            let dst: UserRef<SigSet> = UserRef::from(data);
            let val = state.signals.mask();
            current_task.mm().write_object(dst, &val)?;
            Ok(())
        }
        PTRACE_GETSIGINFO => {
            let dst: UserRef<u8> = UserRef::from(data);
            if let Some(ptrace) = &state.ptrace {
                if let Some(signal) = ptrace.last_signal.as_ref() {
                    current_task.mm().write_objects(dst, &signal.as_siginfo_bytes())?;
                } else {
                    return error!(EINVAL);
                }
            }
            Ok(())
        }
        PTRACE_SETSIGINFO => {
            // Rust will let us do this cast in a const assignment but not in a
            // const generic constraint.
            const SI_MAX_SIZE_AS_USIZE: usize = SI_MAX_SIZE as usize;

            let siginfo_mem = current_task.read_memory_to_array::<SI_MAX_SIZE_AS_USIZE>(data)?;
            let header = SignalInfoHeader::read_from(&siginfo_mem[..SI_HEADER_SIZE]).unwrap();

            let mut bytes = [0u8; SI_MAX_SIZE as usize - SI_HEADER_SIZE];
            bytes.copy_from_slice(&siginfo_mem[SI_HEADER_SIZE..SI_MAX_SIZE as usize]);
            let details = SignalDetail::Raw { data: bytes };
            let unchecked_signal = UncheckedSignal::new(header.signo as u64);
            let signal = Signal::try_from(unchecked_signal)?;

            let siginfo = SignalInfo {
                signal,
                errno: header.errno,
                code: header.code,
                detail: details,
                force: false,
            };
            if let Some(ref mut ptrace) = &mut state.ptrace {
                ptrace.last_signal = Some(siginfo);
            }
            Ok(())
        }
        _ => {
            not_implemented!("ptrace", request);
            error!(ENOSYS)
        }
    }
}

fn do_attach(
    thread_group: &ThreadGroup,
    task: WeakRef<Task>,
    attach_type: PtraceAttachType,
) -> Result<(), Errno> {
    if let Some(task_ref) = task.upgrade() {
        thread_group.ptracees.lock().insert(task_ref.get_tid(), (&task_ref).into());
        task_ref.write().set_ptrace(Some(PtraceState::new(thread_group.leader, attach_type)))?;
        return Ok(());
    }
    // The tracee is either the current thread, or there is a live ref to it outside
    // this function.
    unreachable!("Tracee thread not found");
}

fn check_caps_for_attach(ptrace_scope: u8, current_task: &CurrentTask) -> Result<(), Errno> {
    if ptrace_scope == ADMIN_ONLY_SCOPE && !current_task.creds().has_capability(CAP_SYS_PTRACE) {
        // Admin only use of ptrace
        return error!(EPERM);
    }
    if ptrace_scope == NO_ATTACH_SCOPE {
        // No use of ptrace
        return error!(EPERM);
    }
    Ok(())
}

pub fn ptrace_traceme(current_task: &mut CurrentTask) -> Result<(), Errno> {
    let ptrace_scope = current_task.kernel().ptrace_scope.load(Ordering::Relaxed);
    check_caps_for_attach(ptrace_scope, current_task)?;

    let parent = current_task.thread_group.read().parent.clone();
    if let Some(parent) = parent {
        let task_ref = OwnedRef::temp(&current_task.task);
        do_attach(&parent, (&task_ref).into(), PtraceAttachType::Attach)
    } else {
        error!(EPERM)
    }
}

pub fn ptrace_attach<L>(
    locked: &mut Locked<'_, L>,
    current_task: &mut CurrentTask,
    pid: pid_t,
    attach_type: PtraceAttachType,
) -> Result<(), Errno>
where
    L: LockBefore<MmDumpable>,
{
    let ptrace_scope = current_task.kernel().ptrace_scope.load(Ordering::Relaxed);
    check_caps_for_attach(ptrace_scope, current_task)?;

    let weak_task = current_task.kernel().pids.read().get_task(pid);
    let tracee = weak_task.upgrade().ok_or_else(|| errno!(ESRCH))?;

    if tracee.thread_group == current_task.thread_group {
        return error!(EPERM);
    }

    if *tracee.mm().dumpable.lock(locked) == DumpPolicy::Disable {
        return error!(EPERM);
    }

    if ptrace_scope == RESTRICTED_SCOPE {
        // This only allows us to attach to descendants and tasks that have
        // explicitly allowlisted us with PR_SET_PTRACER.
        let mut ttg = tracee.thread_group.read().parent.clone();
        let mut is_parent = false;
        let my_pid = current_task.thread_group.leader;
        while let Some(target) = ttg {
            if target.as_ref().leader == my_pid {
                is_parent = true;
                break;
            }
            ttg = target.read().parent.clone();
        }
        if !is_parent {
            match tracee.thread_group.read().allowed_ptracers {
                PtraceAllowedPtracers::None => return error!(EPERM),
                PtraceAllowedPtracers::Some(pid) => {
                    if my_pid != pid {
                        return error!(EPERM);
                    }
                }
                PtraceAllowedPtracers::Any => {}
            }
        }
    }

    current_task.check_ptrace_access_mode(locked, PTRACE_MODE_ATTACH_REALCREDS, &tracee)?;
    do_attach(&current_task.thread_group, weak_task.clone(), attach_type)?;
    if attach_type == PtraceAttachType::Attach {
        send_standard_signal(&tracee, SignalInfo::default(SIGSTOP));
    }
    Ok(())
}

pub fn ptrace_set_scope(kernel: &Arc<Kernel>, data: &[u8]) -> Result<(), Errno> {
    loop {
        let ptrace_scope = kernel.ptrace_scope.load(Ordering::Relaxed);
        if let Ok(val) = parse_unsigned_file::<u8>(data) {
            // Legal values are 0<=val<=3, unless scope is NO_ATTACH - see Yama
            // documentation.
            if ptrace_scope == NO_ATTACH_SCOPE && val != NO_ATTACH_SCOPE {
                return error!(EINVAL);
            }
            if val <= NO_ATTACH_SCOPE {
                match kernel.ptrace_scope.compare_exchange(
                    ptrace_scope,
                    val,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Ok(()),
                    Err(_) => continue,
                }
            }
        }
        return error!(EINVAL);
    }
}

pub fn ptrace_get_scope(kernel: &Arc<Kernel>) -> Vec<u8> {
    let mut scope = kernel.ptrace_scope.load(Ordering::Relaxed).to_string();
    scope.push('\n');
    scope.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        task::syscalls::sys_prctl,
        testing::{create_kernel_task_and_unlocked, create_task},
    };
    use starnix_uapi::PR_SET_PTRACER;

    #[::fuchsia::test]
    async fn test_set_ptracer() {
        let (kernel, mut tracee, mut locked) = create_kernel_task_and_unlocked();
        let mut tracer = create_task(&kernel, "tracer");
        kernel.ptrace_scope.store(RESTRICTED_SCOPE, Ordering::Relaxed);
        assert_eq!(
            sys_prctl(&mut locked, &mut tracee, PR_SET_PTRACER, 0xFFF, 0, 0, 0),
            error!(EINVAL)
        );

        assert_eq!(
            ptrace_attach(
                &mut locked,
                &mut tracer,
                tracee.as_ref().task.id,
                PtraceAttachType::Attach
            ),
            error!(EPERM)
        );

        assert!(sys_prctl(
            &mut locked,
            &mut tracee,
            PR_SET_PTRACER,
            tracer.thread_group.leader as u64,
            0,
            0,
            0
        )
        .is_ok());

        let mut not_tracer = create_task(&kernel, "not-tracer");
        assert_eq!(
            ptrace_attach(
                &mut locked,
                &mut not_tracer,
                tracee.as_ref().task.id,
                PtraceAttachType::Attach
            ),
            error!(EPERM)
        );

        assert!(ptrace_attach(
            &mut locked,
            &mut tracer,
            tracee.as_ref().task.id,
            PtraceAttachType::Attach
        )
        .is_ok());
    }

    #[::fuchsia::test]
    async fn test_set_ptracer_any() {
        let (kernel, mut tracee, mut locked) = create_kernel_task_and_unlocked();
        let mut tracer = create_task(&kernel, "tracer");
        kernel.ptrace_scope.store(RESTRICTED_SCOPE, Ordering::Relaxed);
        assert_eq!(
            sys_prctl(&mut locked, &mut tracee, PR_SET_PTRACER, 0xFFF, 0, 0, 0),
            error!(EINVAL)
        );

        assert_eq!(
            ptrace_attach(
                &mut locked,
                &mut tracer,
                tracee.as_ref().task.id,
                PtraceAttachType::Attach
            ),
            error!(EPERM)
        );

        assert!(sys_prctl(
            &mut locked,
            &mut tracee,
            PR_SET_PTRACER,
            PR_SET_PTRACER_ANY as u64,
            0,
            0,
            0
        )
        .is_ok());

        assert!(ptrace_attach(
            &mut locked,
            &mut tracer,
            tracee.as_ref().task.id,
            PtraceAttachType::Attach
        )
        .is_ok());
    }
}
