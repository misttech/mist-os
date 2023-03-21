// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logging::{log_trace, log_warn};
use crate::signals::*;
use crate::syscalls::SyscallResult;
use crate::task::*;
use crate::types::*;

pub fn send_signal(task: &Task, siginfo: SignalInfo) {
    let mut task_state = task.write();

    let action_is_masked = task_state.signals.mask().has_signal(siginfo.signal);
    let sigaction = task.thread_group.signal_actions.get(siginfo.signal);
    let action = action_for_signal(&siginfo, sigaction);

    // Enqueue a masked signal, since it can be unmasked later, but don't enqueue an ignored
    // signal, since it cannot. See SigtimedwaitTest.IgnoredUnmaskedSignal gvisor test.
    if action != DeliveryAction::Ignore || action_is_masked {
        task_state.signals.enqueue(siginfo.clone());
    }

    drop(task_state);

    if !action_is_masked && action.must_interrupt(sigaction) {
        // Wake the task. Note that any potential signal handler will be executed before
        // the task returns from the suspend (from the perspective of user space).
        task.interrupt();
    }

    // Unstop the process for SIGCONT. Also unstop for SIGKILL, the only signal that can interrupt
    // a stopped process.
    if siginfo.signal == SIGCONT || siginfo.signal == SIGKILL {
        task.thread_group.set_stopped(false, siginfo);
    }
}

pub fn force_signal(current_task: &CurrentTask, mut siginfo: SignalInfo) {
    log_trace!(current_task, "forced signal {:?}", siginfo);
    siginfo.force = true;
    send_signal(current_task, siginfo)
}

/// Represents the action to take when signal is delivered.
///
/// See https://man7.org/linux/man-pages/man7/signal.7.html.
#[derive(Debug, PartialEq)]
enum DeliveryAction {
    Ignore,
    CallHandler,
    Terminate,
    CoreDump,
    Stop,
    Continue,
}

impl DeliveryAction {
    /// Returns whether the target task must be interrupted to execute the action.
    ///
    /// The task will not be interrupted if the signal is the action is the Continue action, or if
    /// the action is Ignore and the user specifically requested to ignore the signal.
    pub fn must_interrupt(&self, sigaction: sigaction_t) -> bool {
        match *self {
            Self::Continue => false,
            Self::Ignore => sigaction.sa_handler == SIG_IGN,
            _ => true,
        }
    }
}

fn action_for_signal(siginfo: &SignalInfo, sigaction: sigaction_t) -> DeliveryAction {
    match sigaction.sa_handler {
        SIG_DFL => match siginfo.signal {
            SIGCHLD | SIGURG | SIGWINCH => DeliveryAction::Ignore,
            sig if sig.is_real_time() => DeliveryAction::Ignore,
            SIGHUP | SIGINT | SIGKILL | SIGPIPE | SIGALRM | SIGTERM | SIGUSR1 | SIGUSR2
            | SIGPROF | SIGVTALRM | SIGSTKFLT | SIGIO | SIGPWR => DeliveryAction::Terminate,
            SIGQUIT | SIGILL | SIGABRT | SIGFPE | SIGSEGV | SIGBUS | SIGSYS | SIGTRAP | SIGXCPU
            | SIGXFSZ => DeliveryAction::CoreDump,
            SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => DeliveryAction::Stop,
            SIGCONT => DeliveryAction::Continue,
            _ => panic!("Unknown signal"),
        },
        SIG_IGN => DeliveryAction::Ignore,
        _ => DeliveryAction::CallHandler,
    }
}

/// Dequeues and handles a pending signal for `current_task`.
pub fn dequeue_signal(current_task: &mut CurrentTask) {
    let task = current_task.task_arc_clone();
    let mut task_state = task.write();

    let mask = task_state.signals.mask();
    let siginfo =
        task_state.signals.take_next_where(|sig| !mask.has_signal(sig.signal) || sig.force);
    prepare_to_restart_syscall(
        current_task,
        siginfo.as_ref().map(|siginfo| task.thread_group.signal_actions.get(siginfo.signal)),
    );

    // A syscall may have been waiting with a temporary mask which should be used to dequeue the
    // signal, but after the signal has been dequeued the old mask should be restored.
    task_state.signals.restore_mask();

    // Drop the task state before actually setting up any signal handler.
    std::mem::drop(task_state);

    if let Some(siginfo) = siginfo {
        deliver_signal(&current_task.task, siginfo, &mut current_task.registers);
    }
}

pub fn deliver_signal(task: &Task, mut siginfo: SignalInfo, registers: &mut RegisterState) {
    let mut task_state = task.write();

    loop {
        let sigaction = task.thread_group.signal_actions.get(siginfo.signal);
        let action = action_for_signal(&siginfo, sigaction);
        log_trace!(task, "handling signal {:?} with action {:?}", siginfo, action);
        match action {
            DeliveryAction::Ignore => {}
            DeliveryAction::CallHandler => {
                let signal = siginfo.signal;
                if let Err(err) = dispatch_signal_handler(
                    task,
                    registers,
                    &mut task_state.signals,
                    siginfo,
                    sigaction,
                ) {
                    log_warn!(task, "failed to deliver signal {:?}: {:?}", signal, err);

                    siginfo = SignalInfo::default(SIGSEGV);
                    // The behavior that we want is:
                    //  1. If we failed to send a SIGSEGV, or SIGSEGV is masked, or SIGSEGV is ignored,
                    //  we reset the signal disposition and unmask SIGSEGV.
                    //  2. Send a SIGSEGV to the program, with the (possibly) updated signal
                    //  disposition and mask.
                    let sigaction = task.thread_group.signal_actions.get(siginfo.signal);
                    let action = action_for_signal(&siginfo, sigaction);
                    let masked_signals = task_state.signals.mask();
                    if signal == SIGSEGV
                        || masked_signals.has_signal(SIGSEGV)
                        || action == DeliveryAction::Ignore
                    {
                        task_state
                            .signals
                            .set_mask(masked_signals.with_sigset_removed(SIGSEGV.into()));
                        task.thread_group.signal_actions.set(SIGSEGV, sigaction_t::default());
                    }

                    // Try to deliver the SIGSEGV.
                    // We already checked whether we needed to unmask or reset the signal
                    // disposition.
                    // This could not lead to an infinite loop, because if we had a SIGSEGV
                    // handler, and we failed to send a SIGSEGV, we remove the handler and resend
                    // the SIGSEGV.
                    continue;
                }
            }
            DeliveryAction::Terminate => {
                // Release the signals lock. [`ThreadGroup::exit`] sends signals to threads which
                // will include this one and cause a deadlock re-acquiring the signals lock.
                drop(task_state);
                task.thread_group.exit(ExitStatus::Kill(siginfo));
            }
            DeliveryAction::CoreDump => {
                task_state.dump_on_exit = true;
                drop(task_state);
                task.thread_group.exit(ExitStatus::CoreDump(siginfo));
            }
            DeliveryAction::Stop => {
                drop(task_state);
                task.thread_group.set_stopped(true, siginfo);
            }
            DeliveryAction::Continue => {
                // Nothing to do. Effect already happened when the signal was raised.
            }
        };
        break;
    }
}

pub fn sys_restart_syscall(current_task: &mut CurrentTask) -> Result<SyscallResult, Errno> {
    match current_task.syscall_restart_func.take() {
        Some(f) => f(current_task),
        None => {
            // This may indicate a bug where a syscall returns ERESTART_RESTARTBLOCK without
            // setting a restart func. But it can also be triggered by userspace, e.g. by directly
            // calling restart_syscall or injecting an ERESTART_RESTARTBLOCK error through ptrace.
            log_warn!(current_task, "restart_syscall called, but nothing to restart");
            error!(EINTR)
        }
    }
}
