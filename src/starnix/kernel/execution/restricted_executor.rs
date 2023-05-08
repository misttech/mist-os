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
use crate::arch::{
    execution::{generate_cfi_directives, restore_cfi_directives},
    registers::RegisterState,
};
use crate::logging::{log_error, log_trace, log_warn, set_current_task_info, set_zx_name};
use crate::mm::MemoryManager;
use crate::signals::{deliver_signal, SignalActions, SignalInfo};
use crate::syscalls::decls::SyscallDecl;
use crate::task::{
    CurrentTask, ExceptionResult, ExitStatus, Kernel, ProcessGroup, Task, ThreadGroup,
    ThreadGroupWriteGuard,
};
use crate::types::*;

extern "C" {
    /// The function which enters restricted mode. This function never technically returns, instead
    /// it saves some register state and calls `zx_restricted_enter`. When the thread traps in
    /// restricted mode, the kernel returns control to `restricted_return`, `restricted_return` then
    /// restores the register state and returns back out to where `restricted_enter` was called
    /// from.
    ///
    /// `exit_reason` is populated with reason code that identifies why the thread has returned to
    /// normal mode. This is only written if this function returns ZX_OK and is otherwise not
    /// touched.
    fn restricted_enter(
        options: u32,
        restricted_return: usize,
        exit_reason: *mut zx::sys::zx_restricted_reason_t,
    ) -> zx::sys::zx_status_t;

    /// The function that is used to "return" from restricted mode. See `restricted_enter` for more
    /// information.
    fn restricted_return();

    /// `zx_restricted_bind_state` system call.
    fn zx_restricted_bind_state(
        options: u32,
        out_vmo_handle: *mut zx::sys::zx_handle_t,
    ) -> zx::sys::zx_status_t;

    /// `zx_restricted_unbind_state` system call.
    fn zx_restricted_unbind_state(options: u32) -> zx::sys::zx_status_t;

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
///
/// TODO(https://fxbug.dev/117302): Note, cross-process shared resources allocated in this function
/// that aren't freed by the Zircon kernel upon thread and/or process termination (like mappings in
/// the shared region) should be freed in `Task::destroy_do_not_use_outside_of_drop_if_possible()`.
fn run_task(current_task: &mut CurrentTask) -> Result<ExitStatus, Error> {
    set_zx_name(&fuchsia_runtime::thread_self(), current_task.command().as_bytes());
    set_current_task_info(current_task);

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

    // Allocate a VMO and bind it to this thread.
    let mut out_vmo_handle = 0;
    let status = unsafe { zx_restricted_bind_state(0, &mut out_vmo_handle) };
    match { status } {
        zx::sys::ZX_OK => {
            // We've successfully attached the VMO to the current thread. This VMO will be mapped
            // and used for the kernel to store restricted mode register state as it enters and
            // exits restricted mode.
        }
        _ => return Err(format_err!("failed restricted_bind_state: {:?}", status)),
    }
    let state_vmo = unsafe { zx::Vmo::from(zx::Handle::from_raw(out_vmo_handle)) };

    // Unbind when we leave this scope to avoid unnecessarily retaining the VMO via this thread's
    // binding.  Of course, we'll still have to remove any mappings and close any handles that refer
    // to the VMO to ensure it will be destroyed.  See note about preventing resource leaks in this
    // function's documentation.
    scopeguard::defer! {
        unsafe { zx_restricted_unbind_state(0); }
    }

    // Map the restricted state VMO and arrange for it to be unmapped later.
    let state_vmo_size = state_vmo.get_size()? as usize;
    let state_address = fuchsia_runtime::vmar_root_self()
        .map(0, &state_vmo, 0, state_vmo_size, zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE)
        .unwrap();
    // TODO(https://fxbug.dev/117302): Normally, we'd use scopeguard::defer! to unmap as we leave
    // this function, but instead we'll stash the `state_address` and `state_vmo_size` so that
    // `Task::destroy_do_not_use_outside_of_drop_if_possible()` can remove this mapping later.
    {
        let mut task_state = current_task.write();
        task_state.restricted_state_addr_and_size = Some((state_address, state_vmo_size));
    }

    let bound_state = unsafe {
        std::slice::from_raw_parts_mut(
            state_address as *mut u8,
            std::mem::size_of::<zx::sys::zx_restricted_state_t>(),
        )
    };

    let mut syscall_decl = SyscallDecl::from_number(u64::MAX);
    loop {
        let mut state = zx::sys::zx_restricted_state_t::from(&*current_task.registers);

        // Copy the register state into the mapped VMO.
        bound_state.copy_from_slice(restricted_state_as_bytes(&mut state));

        let mut reason_code: zx::sys::zx_restricted_reason_t = u64::MAX;
        trace_duration_begin!(trace_category_starnix!(), trace_name_user_space!());
        let status = unsafe {
            restricted_enter(
                zx::sys::ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL,
                restricted_return_ptr as usize,
                &mut reason_code,
            )
        };
        trace_duration_end!(trace_category_starnix!(), trace_name_user_space!());
        match { status } {
            zx::sys::ZX_OK => {
                // Successfully entered and exited restricted mode. At this point the task has
                // trapped back out of restricted mode, so the restricted state contains the
                // information about which system call to dispatch.
            }
            _ => return Err(format_err!("failed to restricted_enter: {:?} {:?}", state, status)),
        }

        if reason_code != zx::sys::ZX_RESTRICTED_REASON_SYSCALL {
            return Err(format_err!("Received unexpected restricted reason code: {}", reason_code));
        }

        trace_duration!(
            trace_category_starnix!(),
            trace_name_run_task_loop!(),
            trace_arg_name!() => syscall_decl.name
        );

        // Copy the register state out of the VMO.
        restricted_state_as_bytes(&mut state).copy_from_slice(bound_state);

        // Store the new register state in the current task before dispatching the system call.
        current_task.registers = zx::sys::zx_thread_state_general_regs_t::from(&state).into();
        syscall_decl = SyscallDecl::from_number(current_task.registers.syscall_register());

        // Generate CFI directives so the unwinder will be redirected to unwind the restricted
        // stack.
        generate_cfi_directives!(state);

        if let Some(new_error_context) = execute_syscall(current_task, syscall_decl) {
            error_context = Some(new_error_context);
        }

        // Restore the CFI directives before continuing.
        restore_cfi_directives!();

        if let Some(exit_status) = process_completed_syscall(current_task, &error_context)? {
            let dump_on_exit = current_task.read().dump_on_exit;
            if dump_on_exit {
                log_trace!("requesting backtrace");
                // Disable exception processing on the exception handling thread since we're about
                // to generate a SW breakpoint exception and we want the system to handle it. This
                // is a temporary workaround. Once Zircon gains the ability to return exceptions
                // generated from restricted mode through zx_restricted_enter's restricted_return
                // call we can just generate an exception here in normal mode and it will not be
                // confused with exceptions from restricted mode.
                current_task.ignore_exceptions.store(true, std::sync::atomic::Ordering::Release);
                // (Re)-generate CFI directives so that stack unwinders will trace into the Linux state.
                generate_cfi_directives!(state);
                backtrace_request::backtrace_request_current_thread();
                restore_cfi_directives!();
            }
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
                log_warn!("Died unexpectedly from {:?}! treating as SIGKILL", error);
                let exit_status = ExitStatus::Kill(SignalInfo::default(SIGKILL));

                current_task.write().exit_status = Some(exit_status.clone());
                Ok(exit_status)
            }
            ok => ok,
        };

        current_task.signal_vfork();
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
            if task.ignore_exceptions.load(std::sync::atomic::Ordering::Acquire) {
                log_warn!("ignoring exception");
                continue;
            }
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
                    registers.set_flags_register(0);

                    deliver_signal(&task, signal, &mut registers);

                    if task.read().exit_status.is_some() {
                        log_trace!("exiting with status {:?}", task.read().exit_status.as_ref());
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
                    log_error!("Unhandled exception {:?}", info);
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
