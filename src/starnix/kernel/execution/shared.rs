// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::arch::execution::new_syscall;
use crate::mm::MemoryManager;
use crate::signals::{dequeue_signal, prepare_to_restart_syscall};
use crate::syscalls::table::dispatch_syscall;
use crate::task::{
    ptrace_syscall_enter, ptrace_syscall_exit, CurrentTask, ExitStatus, SeccompStateValue,
    TaskFlags, ThreadGroup,
};
use crate::vfs::FileSystemCreator;
#[cfg(feature = "syscall_stats")]
use fuchsia_inspect::NumericProperty;
use fuchsia_zircon::{self as zx};
use starnix_logging::log_trace;
use starnix_sync::{Locked, Unlocked};
use starnix_syscalls::decls::{Syscall, SyscallDecl};
use starnix_syscalls::SyscallResult;
use starnix_uapi::errors::Errno;
use starnix_uapi::ownership::{OwnedRef, Releasable};
use std::sync::Arc;

/// Contains context to track the most recently failing system call.
///
/// When a task exits with a non-zero exit code, this context is logged to help debugging which
/// system call may have triggered the failure.
pub struct ErrorContext {
    /// The system call that failed.
    pub syscall: Syscall,

    /// The error that was returned for the system call.
    pub error: Errno,
}

/// Result returned when creating new Zircon threads and processes for tasks.
pub struct TaskInfo {
    /// The thread that was created for the task.
    pub thread: Option<zx::Thread>,

    /// The thread group that the task should be added to.
    pub thread_group: OwnedRef<ThreadGroup>,

    /// The memory manager to use for the task.
    pub memory_manager: Arc<MemoryManager>,
}

impl Releasable for TaskInfo {
    type Context<'a> = ();

    fn release<'a>(self, context: Self::Context<'a>) {
        self.thread_group.release(context);
    }
}

/// Executes the provided `syscall` in `current_task`.
///
/// Returns an `ErrorContext` if the system call returned an error.
#[inline(never)] // Inlining this function breaks the CFI directives used to unwind into user code.
pub fn execute_syscall(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    syscall_decl: SyscallDecl,
) -> Option<ErrorContext> {
    #[cfg(feature = "syscall_stats")]
    crate::syscalls::syscall_stats::syscall_stats_property(syscall_decl.number).add(1);

    let syscall = new_syscall(syscall_decl, current_task);

    current_task.thread_state.registers.save_registers_for_restart(syscall.decl.number);

    if current_task.trace_syscalls.load(std::sync::atomic::Ordering::Relaxed) {
        ptrace_syscall_enter(current_task);
    }

    log_trace!("{:?}", syscall);

    let result: Result<SyscallResult, Errno> =
        if current_task.seccomp_filter_state.get() != SeccompStateValue::None {
            // Inlined fast path for seccomp, so that we don't incur the cost
            // of a method call when running the filters.
            if let Some(res) = current_task.run_seccomp_filters(&syscall) {
                res
            } else {
                dispatch_syscall(locked, current_task, &syscall)
            }
        } else {
            dispatch_syscall(locked, current_task, &syscall)
        };

    current_task.trigger_delayed_releaser();

    let return_value = match result {
        Ok(return_value) => {
            log_trace!("-> {:#x}", return_value.value());
            current_task.thread_state.registers.set_return_register(return_value.value());
            None
        }
        Err(errno) => {
            log_trace!("!-> {:?}", errno);
            current_task.thread_state.registers.set_return_register(errno.return_value());
            Some(ErrorContext { error: errno, syscall })
        }
    };

    if current_task.trace_syscalls.load(std::sync::atomic::Ordering::Relaxed) {
        ptrace_syscall_exit(current_task, return_value.is_some());
    }

    return_value
}

/// Finishes `current_task` updates after a restricted mode exit such as a syscall, exception, or kick.
///
/// Returns an `ExitStatus` if the task is meant to exit.
pub fn process_completed_restricted_exit(
    current_task: &mut CurrentTask,
    error_context: &Option<ErrorContext>,
) -> Result<Option<ExitStatus>, Errno> {
    let result;
    loop {
        // Checking for a signal might cause the task to exit, so check before processing exit
        {
            {
                if !current_task.is_exitted() {
                    dequeue_signal(current_task);
                }
                // The syscall may need to restart for a non-signal-related
                // reason. This call does nothing if we aren't restarting.
                prepare_to_restart_syscall(&mut current_task.thread_state.registers, None);
            }
        }

        let exit_status = current_task.exit_status();
        if let Some(exit_status) = exit_status {
            log_trace!("exiting with status {:?}", exit_status);
            if let Some(error_context) = error_context {
                match exit_status {
                    ExitStatus::Exit(value) if value == 0 => {}
                    _ => {
                        log_trace!(
                            "last failing syscall before exit: {:?}, failed with {:?}",
                            error_context.syscall,
                            error_context.error
                        );
                    }
                };
            }

            result = Some(exit_status);
            break;
        } else {
            // Block a stopped process after it's had a chance to handle signals, since a signal might
            // cause it to stop.
            current_task.block_while_stopped();
            // If ptrace_cont has sent a signal, process it immediately.  This
            // seems to match Linux behavior.

            let task_state = current_task.read();
            if task_state
                .ptrace
                .as_ref()
                .is_some_and(|ptrace| ptrace.stop_status == crate::task::PtraceStatus::Continuing)
                && task_state.is_any_signal_pending()
                && !current_task.is_exitted()
            {
                continue;
            }
            result = None;
            break;
        }
    }

    if let Some(exit_status) = &result {
        if current_task.flags().contains(TaskFlags::DUMP_ON_EXIT) {
            // Request a backtrace before reporting the crash to increase chance of a backtrace in
            // logs.
            // TODO(https://fxbug.dev/356732164) collect a backtrace ourselves
            debug::backtrace_request_current_thread();
            current_task.kernel().crash_reporter.handle_core_dump(&current_task, exit_status);
        }
    }
    return Ok(result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::SignalInfo;
    use crate::task::StopState;
    use crate::testing::*;
    use starnix_uapi::signals::{SIGCONT, SIGSTOP};

    #[::fuchsia::test]
    async fn test_block_while_stopped_stop_and_continue() {
        let (_kernel, mut task) = create_kernel_and_task();

        // block_while_stopped must immediately returned if the task is not stopped.
        task.block_while_stopped();

        // Stop the task.
        task.thread_group.set_stopped(
            StopState::GroupStopping,
            Some(SignalInfo::default(SIGSTOP)),
            false,
        );

        let thread = std::thread::spawn({
            let task = task.weak_task();
            move || {
                let task = task.upgrade().expect("task must be alive");
                // Wait for the task to have a waiter.
                while !task.read().is_blocked() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }

                // Continue the task.
                task.thread_group.set_stopped(
                    StopState::Waking,
                    Some(SignalInfo::default(SIGCONT)),
                    false,
                );
            }
        });

        // Block until continued.
        task.block_while_stopped();

        // Join the thread, which will ensure set_stopped terminated.
        thread.join().expect("joined");

        // The task should not be blocked anymore.
        task.block_while_stopped();
    }

    #[::fuchsia::test]
    async fn test_block_while_stopped_stop_and_exit() {
        let (_kernel, mut task) = create_kernel_and_task();

        // block_while_stopped must immediately returned if the task is neither stopped nor exited.
        task.block_while_stopped();

        // Stop the task.
        task.thread_group.set_stopped(
            StopState::GroupStopping,
            Some(SignalInfo::default(SIGSTOP)),
            false,
        );

        let thread = std::thread::spawn({
            let task = task.weak_task();
            move || {
                let task = task.upgrade().expect("task must be alive");
                // Wait for the task to have a waiter.
                while !task.read().is_blocked() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }

                // exit the task.
                task.thread_group.exit(ExitStatus::Exit(1), None);
            }
        });

        // Block until continued.
        task.block_while_stopped();

        // Join the task, which will ensure thread_group.exit terminated.
        thread.join().expect("joined");

        // The task should not be blocked because it is stopped.
        task.block_while_stopped();
    }
}
