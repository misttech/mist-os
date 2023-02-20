// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]
#![cfg(feature = "restricted_mode")]

use anyhow::{format_err, Error};
use fuchsia_zircon as zx;
use fuchsia_zircon::{AsHandleRef, Task as zxTask};
use std::sync::Arc;

use super::shared::{
    as_exception_info, execute_syscall, process_completed_syscall, read_channel_sync, TaskInfo,
};
use crate::logging::{log_error, log_trace, log_warn, set_zx_name};
use crate::mm::MemoryManager;
use crate::signals::{deliver_signal, SignalActions, SignalInfo};
use crate::syscalls::decls::SyscallDecl;
use crate::task::{
    CurrentTask, ExceptionResult, ExitStatus, Kernel, ProcessGroup, RegisterState, Task,
    ThreadGroup, ThreadGroupWriteGuard,
};
use crate::types::*;

extern "C" {
    /// The function which enters restricted mode. This function never technically returns, instead
    /// it saves some register state and calls `zx_restricted_enter`. When the thread traps in
    /// restricted mode, the kernel returns control to `restricted_return`, `restricted_return` then
    /// restores the register state and returns back out to where `restricted_enter` was called
    /// from.
    fn restricted_enter(options: u32, restricted_return: usize) -> zx::sys::zx_status_t;

    /// The function that is used to "return" from restricted mode. See `restricted_enter` for more
    /// information.
    fn restricted_return();

    /// `zx_restricted_write_state` system call.
    fn zx_restricted_write_state(buffer: *const u8, buffer_size: usize) -> zx::sys::zx_status_t;

    /// `zx_restricted_read_state` system call.
    fn zx_restricted_read_state(buffer: *mut u8, buffer_size: usize) -> zx::sys::zx_status_t;

    /// Sets the process handle used to create new threads, for the current thread.
    fn thrd_set_zx_process(handle: zx::sys::zx_handle_t) -> zx::sys::zx_handle_t;
}

/// Runs the `current_task` to completion.
///
/// The high-level flow of this function looks as follows:
///
///   1. Write the restricted state for the current thread to set it up to enter into the restricted
///      (Linux) part of the address space.
///   2. Enter restricted mode.
///   3. Return from restricted mode, reading out the new state of the restricted mode execution.
///      This state contains the thread's restricted register state, which is used to determine
///      which system call to dispatch.
///   4. Dispatch the system call.
///   5. Handle pending signals.
///   6. Goto 1.
fn run_task(current_task: &mut CurrentTask) -> Result<ExitStatus, Error> {
    set_zx_name(&fuchsia_runtime::thread_self(), current_task.command().as_bytes());

    // The task does not yet have a thread associated with it, so associate it with this thread.
    let mut thread = current_task.thread.write();
    *thread = Some(fuchsia_runtime::thread_self().duplicate(zx::Rights::SAME_RIGHTS).unwrap());

    // Create an exception channel to monitor for page faults in restricted code.
    let exception_channel = thread.as_ref().unwrap().create_exception_channel()?;
    std::mem::drop(thread);

    let task = current_task.task.clone();
    handle_exceptions(task, exception_channel);

    // This is the pointer that is passed to `restricted_enter`.
    let restricted_return_ptr = restricted_return as *const ();

    // This tracks the last failing system call for debugging purposes.
    let mut error_context = None;

    let mut syscall_decl = SyscallDecl::from_number(u64::MAX);
    loop {
        let mut state = zx::sys::zx_restricted_state_t::from(&*current_task.registers);
        match unsafe {
            zx_restricted_write_state(
                restricted_state_as_bytes(&mut state).as_ptr(),
                std::mem::size_of::<zx::sys::zx_restricted_state_t>(),
            )
        } {
            zx::sys::ZX_OK => {
                // Wrote restricted state successfully.
            }
            _ => {
                fuchsia_trace::duration_end!(
                    trace_category_starnix!(),
                    trace_name_run_task_loop!(),
                    trace_arg_name!() => syscall_decl.name
                );
                return Err(format_err!("failed to zx_restricted_write_state: {:?}", state));
            }
        }
        fuchsia_trace::duration_end!(
            trace_category_starnix!(),
            trace_name_run_task_loop!(),
            trace_arg_name!() => syscall_decl.name
        );

        fuchsia_trace::duration_begin!(trace_category_starnix!(), trace_name_user_space!());
        let status = unsafe { restricted_enter(0, restricted_return_ptr as usize) };
        fuchsia_trace::duration_end!(trace_category_starnix!(), trace_name_user_space!());
        match { status } {
            zx::sys::ZX_OK => {
                // Successfully entered and exited restricted mode. At this point the task has
                // trapped back out of restricted mode, so the restricted state contains the
                // information about which system call to dispatch.
            }
            _ => return Err(format_err!("failed to restricted_enter: {:?}", state)),
        }

        fuchsia_trace::duration_begin!(
            trace_category_starnix!(),
            trace_name_run_task_loop!(),
            trace_arg_name!() => syscall_decl.name
        );
        match unsafe {
            zx_restricted_read_state(
                restricted_state_as_bytes(&mut state).as_mut_ptr(),
                std::mem::size_of::<zx::sys::zx_restricted_state_t>(),
            )
        } {
            zx::sys::ZX_OK => {
                // Read restricted state successfully.
            }
            _ => {
                fuchsia_trace::duration_end!(
                    trace_category_starnix!(),
                    trace_name_run_task_loop!(),
                    trace_arg_name!() => syscall_decl.name
                );
                return Err(format_err!("failed to zx_restricted_read_state: {:?}", state));
            }
        }

        // Store the new register state in the current task before dispatching the system call.
        current_task.registers = zx::sys::zx_thread_state_general_regs_t::from(&state).into();
        syscall_decl = SyscallDecl::from_number(state.rax);

        if let Some(new_error_context) = execute_syscall(current_task, syscall_decl) {
            error_context = Some(new_error_context);
        }

        if let Some(exit_status) = process_completed_syscall(current_task, &error_context)? {
            fuchsia_trace::duration_end!(
                trace_category_starnix!(),
                trace_name_run_task_loop!(),
                trace_arg_name!() => syscall_decl.name
            );
            return Ok(exit_status);
        }
    }
}

/// Returns a mutable reference to `state` as bytes. Used to read and write restricted state from
/// the kernel.
fn restricted_state_as_bytes(state: &mut zx::sys::zx_restricted_state_t) -> &mut [u8] {
    unsafe {
        std::slice::from_raw_parts_mut(
            (state as *mut zx::sys::zx_restricted_state_t) as *mut u8,
            std::mem::size_of::<zx::sys::zx_restricted_state_t>(),
        )
    }
}

/// Note: This does not actually create a Zircon thread. It creates a thread group and memory
/// manager. The exception executor does use this to create an actual thread, but once that executor
/// is removed this function can be renamed/reworked.
pub fn create_zircon_thread(parent: &Task) -> Result<TaskInfo, Errno> {
    let thread_group = parent.thread_group.clone();
    let memory_manager = parent.mm.clone();
    Ok(TaskInfo { thread: None, thread_group, memory_manager })
}

pub fn create_zircon_process(
    kernel: &Arc<Kernel>,
    parent: Option<ThreadGroupWriteGuard<'_>>,
    pid: pid_t,
    process_group: Arc<ProcessGroup>,
    signal_actions: Arc<SignalActions>,
    name: &[u8],
) -> Result<TaskInfo, Errno> {
    let (process, root_vmar) =
        create_shared(&kernel.starnix_process, zx::ProcessOptions::empty(), name)
            .map_err(|status| from_status_like_fdio!(status))?;

    let memory_manager =
        Arc::new(MemoryManager::new(root_vmar).map_err(|status| from_status_like_fdio!(status))?);

    let thread_group =
        ThreadGroup::new(kernel.clone(), process, parent, pid, process_group, signal_actions);

    Ok(TaskInfo { thread: None, thread_group, memory_manager })
}

pub fn execute_task<F>(mut current_task: CurrentTask, task_complete: F)
where
    F: FnOnce(Result<ExitStatus, Error>) + Send + Sync + 'static,
{
    // Set the process handle to the new task's process, so the new thread is spawned in that
    // process.
    let process_handle = current_task.thread_group.process.raw_handle();
    let old_process_handle = unsafe { thrd_set_zx_process(process_handle) };

    // Spawn the process' thread. Note, this closure ends up executing in the process referred to by
    // `process_handle`.
    std::thread::spawn(move || {
        let run_result = match run_task(&mut current_task) {
            Err(error) => {
                log_warn!(current_task, "Died unexpectedly from {:?}! treating as SIGKILL", error);
                let exit_status = ExitStatus::Kill(SignalInfo::default(SIGKILL));

                current_task.write().exit_status = Some(exit_status.clone());
                Ok(exit_status)
            }
            ok => ok,
        };

        current_task.signal_exit();
        task_complete(run_result);
    });

    // Reset the process handle used to create threads.
    unsafe {
        thrd_set_zx_process(old_process_handle);
    };
}

/// Spawn a thread to handle page faults from restricted code.
///
/// This will be handled by the kernel in the future, where the thread will pop out of restricted
/// mode just like it does on a syscall, but for now this needs to be handled by a separate thread.
fn handle_exceptions(task: Arc<Task>, exception_channel: zx::Channel) {
    std::thread::spawn(move || {
        let mut buffer = zx::MessageBuf::new();
        loop {
            match read_channel_sync(&exception_channel, &mut buffer) {
                Ok(_) => {}
                Err(_) => return,
            };
            let info = as_exception_info(&buffer);
            assert!(buffer.n_handles() == 1);
            let exception = zx::Exception::from(buffer.take_handle(0).unwrap());
            let thread = exception.get_thread().unwrap();
            let report = thread.get_exception_report().unwrap();

            match task.process_exception(&info, &exception, &report) {
                ExceptionResult::Handled => {}
                ExceptionResult::Signal(signal) => {
                    log_warn!("delivering signal {signal:?}");
                    // TODO: Verify that the rip is actually in restricted code.

                    let mut registers: RegisterState =
                        thread.read_state_general_regs().unwrap().into();

                    // TODO: Should this be 0, does it matter?
                    registers.rflags = 0;

                    deliver_signal(&task, signal, &mut registers);

                    if task.read().exit_status.is_some() {
                        log_trace!(
                            task,
                            "exiting with status {:?}",
                            task.read().exit_status.as_ref()
                        );
                        // Subtle logic / terrible hack ahead!
                        // At this point we want the task to exit. The restricted mode thread is blocked until
                        // |exception| is handled with its PC pointed into a restricted mode address. We do not
                        // currently have a way to tell Zircon to kick this thread out of restricted mode, so we do not
                        // have a direct mechanism for executing the normal exit from restricted mode logic. Instead, we
                        // let the Zircon exception propagate so that Zircon terminates the thread and thus the process.
                        // Since we won't unwind the stack of the Starnix portion of the faulting thread, which would
                        // usually involve invoking the Drop handler of CurrentTask, we have to destroy it manually
                        // here.  Be aware that since we're destroying the process in a shared address space without
                        // unwinding the stack and dropping objects as would usually happen the state in the shared
                        // space may be surprising.
                        // TODO(https://fxbug.dev/117302): Remove this mechanism once the execution model is evolved
                        // such that we can handle exceptions in the thread that entered restricted mode.
                        task.destroy_do_not_use_outside_of_drop_if_possible();
                        let exception_state = if task.read().dump_on_exit {
                            // Let crashsvc or the debugger handle the exception.
                            &zx::sys::ZX_EXCEPTION_STATE_TRY_NEXT
                        } else {
                            &zx::sys::ZX_EXCEPTION_STATE_THREAD_EXIT
                        };
                        exception.set_exception_state(exception_state).unwrap();

                        unsafe {
                            // The task reference that lives in the syscall dispatch loop will never
                            // be dropped, so `task`'s reference count is decremented twice here.
                            // Once for this reference, and once for the one in the syscall dispatch
                            // loop.
                            // TODO(https://fxbug.dev/117302): Remove this hack.
                            let t = Arc::into_raw(task);
                            Arc::decrement_strong_count(t);
                            Arc::decrement_strong_count(t);
                        }

                        return;
                    } else {
                        thread.write_state_general_regs(registers.into()).unwrap();
                        exception
                            .set_exception_state(&zx::sys::ZX_EXCEPTION_STATE_HANDLED)
                            .unwrap();
                    }
                }
                ExceptionResult::Unhandled => {
                    log_error!(task, "Unhandled exception {:?}", info);
                    exception
                        .set_exception_state(&zx::sys::ZX_EXCEPTION_STATE_THREAD_EXIT)
                        .unwrap();
                    return;
                }
            }
        }
    });
}

/// Creates a process that shares half its address space with this process.
///
/// The created process will also share its handle table and futex context with `self`.
///
/// Returns the created process and a handle to the created process' restricted address space.
///
/// Wraps the
/// [zx_process_create_shared](https://fuchsia.dev/fuchsia-src/reference/syscalls/process_create_shared.md)
/// syscall.
pub fn create_shared(
    process: &zx::Process,
    options: zx::ProcessOptions,
    name: &[u8],
) -> Result<(zx::Process, zx::Vmar), zx::Status> {
    let self_raw = process.raw_handle();
    let name_ptr = name.as_ptr();
    let name_len = name.len();
    let mut process_out = 0;
    let mut restricted_vmar_out = 0;
    let status = unsafe {
        zx::sys::zx_process_create_shared(
            self_raw,
            options.bits(),
            name_ptr,
            name_len,
            &mut process_out,
            &mut restricted_vmar_out,
        )
    };
    zx::ok(status)?;
    unsafe {
        Ok((
            zx::Process::from(zx::Handle::from_raw(process_out)),
            zx::Vmar::from(zx::Handle::from_raw(restricted_vmar_out)),
        ))
    }
}
