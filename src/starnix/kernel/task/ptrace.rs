// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    not_implemented,
    signals::{send_signal, SignalInfo},
    task::waiter::WaitQueue,
    task::StopState,
    task::{CurrentTask, Task, ThreadGroup},
    types::*,
};

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
    pub waitable: Option<SignalInfo>,
}

impl PtraceState {
    pub fn new(pid: pid_t) -> Self {
        PtraceState {
            pid,
            tracee_waiters: WaitQueue::default(),
            tracer_waiters: WaitQueue::default(),
            waitable: None,
        }
    }
}

/// Continues the target thread, optionally detaching from it.
fn ptrace_cont(tracee: &Task, data: &UserAddress, detach: bool) -> Result<(), Errno> {
    let data = data.ptr() as u64;
    if data != 0 {
        let signal = Signal::try_from(UncheckedSignal::new(data))?;
        let siginfo = SignalInfo::default(signal);
        send_signal(&tracee, siginfo);
    }
    let mut state = tracee.write();
    if state.stopped.is_waking_or_awake() {
        if detach {
            state.set_ptrace(None)?;
        }
        return error!(EIO);
    }
    state.set_stopped(StopState::Waking, None);
    if detach {
        state.set_ptrace(None)?;
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
    _addr: UserAddress,
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
            send_signal(&tracee, siginfo);
            Ok(())
        }
        PTRACE_INTERRUPT => {
            not_implemented!("ptrace interrupt not implemented");
            error!(ENOSYS)
        }
        PTRACE_CONT => {
            ptrace_cont(&tracee, &data, false)?;
            Ok(())
        }
        PTRACE_DETACH => ptrace_detach(current_task.thread_group.as_ref(), tracee.as_ref(), &data),
        _ => {
            not_implemented!("ptrace: {} not implemented", request);
            error!(ENOSYS)
        }
    }

    // The remaining requests (to be added) require the thread to be stopped.
}

fn do_attach(thread_group: &ThreadGroup, task: WeakRef<Task>) -> Result<(), Errno> {
    if let Some(task_ref) = task.upgrade() {
        thread_group.ptracees.lock().insert(task_ref.get_tid(), (&task_ref).into());
        task_ref.write().set_ptrace(Some(thread_group.leader))?;
        return Ok(());
    }
    // The tracee is either the current thread, or there is a live ref to it outside
    // this function.
    unreachable!("Tracee thread not found");
}

pub fn ptrace_traceme(current_task: &mut CurrentTask) -> Result<(), Errno> {
    let parent = current_task.thread_group.read().parent.clone();
    if let Some(parent) = parent {
        let task_ref = OwnedRef::temp(&current_task.task);
        do_attach(&parent, (&task_ref).into())
    } else {
        error!(EPERM)
    }
}

pub fn ptrace_attach(current_task: &mut CurrentTask, pid: pid_t) -> Result<(), Errno> {
    let weak_task = current_task.kernel().pids.read().get_task(pid);
    let tracee = weak_task.upgrade().ok_or_else(|| errno!(ESRCH))?;

    if tracee.thread_group == current_task.thread_group {
        return error!(EPERM);
    }

    current_task.check_ptrace_access_mode(PTRACE_MODE_ATTACH_REALCREDS, &tracee)?;
    do_attach(&current_task.thread_group, weak_task.clone())?;
    send_signal(&tracee, SignalInfo::default(SIGSTOP));
    Ok(())
}
