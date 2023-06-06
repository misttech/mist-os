// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    arch::{
        registers::RegisterState,
        signal_handling::{
            align_stack_pointer, restore_registers, update_register_state_for_restart,
            SignalStackFrame, RED_ZONE_SIZE, SIG_STACK_SIZE, SYSCALL_INSTRUCTION_SIZE_BYTES,
        },
    },
    logging::{log_trace, log_warn},
    mm::MemoryAccessor,
    signals::*,
    syscalls::SyscallResult,
    task::*,
    types::*,
};

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
    let handler = if siginfo.force && sigaction.sa_handler == SIG_IGN {
        SIG_DFL
    } else {
        sigaction.sa_handler
    };
    match handler {
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
        log_trace!("handling signal {:?} with action {:?}", siginfo, action);
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
                    log_warn!("failed to deliver signal {:?}: {:?}", signal, err);

                    siginfo = SignalInfo::default(SIGSEGV);
                    // The behavior that we want is:
                    //  1. If we failed to send a SIGSEGV, or SIGSEGV is masked, or SIGSEGV is
                    //  ignored, we reset the signal disposition and unmask SIGSEGV.
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

/// Prepares `current` state to execute the signal handler stored in `action`.
///
/// This function stores the state required to restore after the signal handler on the stack.
pub fn dispatch_signal_handler(
    task: &Task,
    registers: &mut RegisterState,
    signal_state: &mut SignalState,
    siginfo: SignalInfo,
    action: sigaction_t,
) -> Result<(), Errno> {
    let signal_stack_frame = SignalStackFrame::new(task, registers, signal_state, &siginfo, action);

    let main_stack = registers.stack_pointer_register().checked_sub(RED_ZONE_SIZE);
    let stack_bottom = if (action.sa_flags & SA_ONSTACK as u64) != 0 {
        match signal_state.alt_stack {
            Some(sigaltstack) => {
                match main_stack {
                    // Only install the sigaltstack if the stack pointer is not already in it.
                    Some(sp) if sigaltstack.contains_pointer(sp) => main_stack,
                    _ => {
                        // Since the stack grows down, the size is added to the ss_sp when
                        // calculating the "bottom" of the stack.
                        // Use the main stack if sigaltstack overflows.
                        sigaltstack
                            .ss_sp
                            .ptr()
                            .checked_add(sigaltstack.ss_size)
                            .map(|sp| sp as u64)
                            .or(main_stack)
                    }
                }
            }
            None => main_stack,
        }
    } else {
        main_stack
    }
    .ok_or(errno!(EINVAL))?;

    let stack_pointer =
        align_stack_pointer(stack_bottom.checked_sub(SIG_STACK_SIZE as u64).ok_or(errno!(EINVAL))?);

    if let Some(sigaltstack) = signal_state.alt_stack {
        if sigaltstack.contains_pointer(stack_pointer) ^ sigaltstack.contains_pointer(stack_bottom)
        {
            return error!(EINVAL);
        }
    }

    // Write the signal stack frame at the updated stack pointer.
    task.mm.write_memory(UserAddress::from(stack_pointer), signal_stack_frame.as_bytes())?;
    signal_state.set_mask(action.sa_mask);

    registers.set_stack_pointer_register(stack_pointer);
    registers.set_arg0_register(siginfo.signal.number() as u64);
    if (action.sa_flags & SA_SIGINFO as u64) != 0 {
        registers.set_arg1_register(
            stack_pointer + memoffset::offset_of!(SignalStackFrame, siginfo_bytes) as u64,
        );
        registers.set_arg2_register(
            stack_pointer + memoffset::offset_of!(SignalStackFrame, context) as u64,
        );
    }
    registers.set_instruction_pointer_register(action.sa_handler.ptr() as u64);

    Ok(())
}

pub fn restore_from_signal_handler(current_task: &mut CurrentTask) -> Result<(), Errno> {
    // Read the signal stack frame from memory.
    let signal_frame_address = align_stack_pointer(current_task.registers.stack_pointer_register());
    let mut signal_stack_bytes = [0; SIG_STACK_SIZE];
    current_task
        .mm
        .read_memory(UserAddress::from(signal_frame_address), &mut signal_stack_bytes)?;

    // Grab the registers state from the stack frame.
    let signal_stack_frame = SignalStackFrame::from_bytes(signal_stack_bytes);
    restore_registers(current_task, &signal_stack_frame);

    // Restore the stored signal mask.
    current_task.write().signals.set_mask(SigSet::from(signal_stack_frame.context.uc_sigmask));

    Ok(())
}

/// Maybe adjust a task's registers to restart a syscall once the task switches back to userspace,
/// based on whether the return value is one of the restartable error codes such as ERESTARTSYS.
pub fn prepare_to_restart_syscall(current_task: &mut CurrentTask, sigaction: Option<sigaction_t>) {
    let err = ErrnoCode::from_return_value(current_task.registers.return_register());
    // If sigaction is None, the syscall must be restarted if it is restartable. The default
    // sigaction will not have a sighandler, which will guarantee a restart.
    let sigaction = sigaction.unwrap_or_default();

    let should_restart = match err {
        ERESTARTSYS => sigaction.sa_flags & SA_RESTART as u64 != 0,
        ERESTARTNOINTR => true,
        ERESTARTNOHAND | ERESTART_RESTARTBLOCK => false,

        // The syscall did not request to be restarted.
        _ => return,
    };
    // Always restart if the signal did not call a handler (i.e. SIGSTOP).
    let should_restart = should_restart || sigaction.sa_handler.is_null();

    if !should_restart {
        current_task.registers.set_return_register(EINTR.return_value());
        return;
    }

    update_register_state_for_restart(&mut current_task.registers, err);

    current_task.registers.set_instruction_pointer_register(
        current_task.registers.instruction_pointer_register() - SYSCALL_INSTRUCTION_SIZE_BYTES,
    );
}

pub fn sys_restart_syscall(current_task: &mut CurrentTask) -> Result<SyscallResult, Errno> {
    match current_task.syscall_restart_func.take() {
        Some(f) => f(current_task),
        None => {
            // This may indicate a bug where a syscall returns ERESTART_RESTARTBLOCK without
            // setting a restart func. But it can also be triggered by userspace, e.g. by directly
            // calling restart_syscall or injecting an ERESTART_RESTARTBLOCK error through ptrace.
            log_warn!("restart_syscall called, but nothing to restart");
            error!(EINTR)
        }
    }
}
