// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]

use super::shared::{execute_syscall, process_completed_restricted_exit, TaskInfo};
use crate::{
    arch::execution::{generate_cfi_directives, restore_cfi_directives},
    mm::MemoryManager,
    signals::{deliver_signal, SignalActions, SignalInfo},
    task::{
        ptrace_attach_from_state, CurrentTask, ExceptionResult, ExitStatus, Kernel, ProcessGroup,
        PtraceCoreState, StopState, Task, TaskBuilder, ThreadGroup, ThreadGroupWriteGuard,
    },
};
use anyhow::{format_err, Error};
use fuchsia_inspect_contrib::{profile_duration, ProfileDuration};
use fuchsia_zircon::{
    AsHandleRef, {self as zx},
};
use starnix_logging::{
    firehose_trace_duration, firehose_trace_duration_begin, firehose_trace_duration_end,
    firehose_trace_instant, log_error, log_warn, set_zx_name, CoreDumpInfo, ARG_NAME,
    CATEGORY_STARNIX, MAX_ARGV_LENGTH, NAME_EXECUTE_SYSCALL, NAME_HANDLE_EXCEPTION,
    NAME_READ_RESTRICTED_STATE, NAME_RESTRICTED_KICK, NAME_RUN_TASK, NAME_WRITE_RESTRICTED_STATE,
};
use starnix_sync::{LockBefore, Locked, ProcessGroupState, Unlocked};
use starnix_syscalls::decls::SyscallDecl;
use starnix_uapi::{
    errno, errors::Errno, from_status_like_fdio, ownership::WeakRef, pid_t, signals::SIGKILL,
};
use std::{
    os::unix::thread::JoinHandleExt,
    sync::{mpsc::sync_channel, Arc},
};

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

    // Gets the thread handle underlying a specific thread.
    // In C the 'thread' parameter is thrd_t which on Fuchsia is the same as pthread_t.
    fn thrd_get_zx_handle(thread: u64) -> zx::sys::zx_handle_t;

    /// breakpoint_for_module_changes is a single breakpoint instruction that is used to notify
    /// the debugger about the module changes.
    fn breakpoint_for_module_changes();
}

/// `RestrictedState` manages accesses into the restricted state VMO.
///
/// See `zx_restricted_bind_state`.
pub struct RestrictedState {
    state_size: usize,
    bound_state: &'static mut [u8],
}

impl RestrictedState {
    pub fn from_vmo(state_vmo: zx::Vmo) -> Result<Self, Error> {
        // Map the restricted state VMO and arrange for it to be unmapped later.
        let state_size = state_vmo.get_size()? as usize;
        let state_address = fuchsia_runtime::vmar_root_self()
            .map(0, &state_vmo, 0, state_size, zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE)
            .unwrap();
        let bound_state =
            unsafe { std::slice::from_raw_parts_mut(state_address as *mut u8, state_size) };
        Ok(Self { state_size, bound_state })
    }

    pub fn write_state(&mut self, state: &zx::sys::zx_restricted_state_t) {
        firehose_trace_duration!(CATEGORY_STARNIX, NAME_WRITE_RESTRICTED_STATE);
        debug_assert!(self.state_size >= std::mem::size_of::<zx::sys::zx_restricted_state_t>());
        self.bound_state[0..std::mem::size_of::<zx::sys::zx_restricted_state_t>()]
            .copy_from_slice(Self::restricted_state_as_bytes(state));
    }

    pub fn read_state(&self, state: &mut zx::sys::zx_restricted_state_t) {
        firehose_trace_duration!(CATEGORY_STARNIX, NAME_READ_RESTRICTED_STATE);
        debug_assert!(self.state_size >= std::mem::size_of::<zx::sys::zx_restricted_state_t>());
        Self::restricted_state_as_bytes_mut(state).copy_from_slice(
            &self.bound_state[0..std::mem::size_of::<zx::sys::zx_restricted_state_t>()],
        );
    }

    pub fn read_exception(&self) -> zx::sys::zx_restricted_exception_t {
        // Safety: We use MaybeUninit because we are going to copy the exception details from
        // the restricted state VMO. We are fully populating the zx_restricted_exception_t
        // structure so there will be no uninitialized data visible outside of the unsafe block.
        unsafe {
            let mut report: std::mem::MaybeUninit<zx::sys::zx_restricted_exception_t> =
                std::mem::MaybeUninit::uninit();
            let bytes = std::slice::from_raw_parts_mut(
                report.as_mut_ptr() as *mut u8,
                std::mem::size_of::<zx::sys::zx_restricted_exception_t>(),
            );
            debug_assert!(
                self.state_size >= std::mem::size_of::<zx::sys::zx_restricted_exception_t>()
            );
            bytes.copy_from_slice(
                &self.bound_state[0..std::mem::size_of::<zx::sys::zx_restricted_exception_t>()],
            );
            report.assume_init()
        }
    }

    /// Returns a mutable reference to `state` as bytes. Used to read and write restricted state from
    /// the kernel.
    fn restricted_state_as_bytes_mut(state: &mut zx::sys::zx_restricted_state_t) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(
                (state as *mut zx::sys::zx_restricted_state_t) as *mut u8,
                std::mem::size_of::<zx::sys::zx_restricted_state_t>(),
            )
        }
    }
    fn restricted_state_as_bytes(state: &zx::sys::zx_restricted_state_t) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                (state as *const zx::sys::zx_restricted_state_t) as *const u8,
                std::mem::size_of::<zx::sys::zx_restricted_state_t>(),
            )
        }
    }
}

impl std::ops::Drop for RestrictedState {
    fn drop(&mut self) {
        let mapping_addr = self.bound_state.as_ptr() as usize;
        let mapping_size = self.bound_state.len();
        // Safety: We are un-mapping the state VMO. This is safe because we route all access
        // into this memory region though this struct so it is safe to unmap on Drop.
        unsafe {
            fuchsia_runtime::vmar_root_self()
                .unmap(mapping_addr, mapping_size)
                .expect("Failed to unmap");
        }
    }
}

const RESTRICTED_ENTER_OPTIONS: u32 = 0;

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
/// TODO(https://fxbug.dev/42068497): Note, cross-process shared resources allocated in this function
/// that aren't freed by the Zircon kernel upon thread and/or process termination (like mappings in
/// the shared region) should be freed in `Task::destroy_do_not_use_outside_of_drop_if_possible()`.
fn run_task(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
) -> Result<ExitStatus, Error> {
    let mut profiling_guard = ProfileDuration::enter("TaskLoopSetup");

    set_zx_name(&fuchsia_runtime::thread_self(), current_task.command().as_bytes());
    let span = current_task.logging_span();
    let _span_guard = span.enter();

    firehose_trace_duration!(CATEGORY_STARNIX, NAME_RUN_TASK);

    // This is the pointer that is passed to `restricted_enter`.
    let restricted_return_ptr = restricted_return as *const ();

    // This tracks the last failing system call for debugging purposes.
    let mut error_context = None;

    // Allocate a VMO and bind it to this thread.
    let mut out_vmo_handle = 0;
    let status = zx::Status::from_raw(unsafe { zx_restricted_bind_state(0, &mut out_vmo_handle) });
    match { status } {
        zx::Status::OK => {
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
    let mut restricted_state = RestrictedState::from_vmo(state_vmo)?;

    // We need to check for exit once, before the task starts executing, in case
    // the task has already been sent a signal that will cause it to exit.
    let state = zx::sys::zx_restricted_state_t::from(&*current_task.thread_state.registers);
    if let Some(exit_status) =
        process_completed_restricted_exit(current_task, &error_context, &state)?
    {
        return Ok(exit_status);
    }
    loop {
        let mut state = zx::sys::zx_restricted_state_t::from(&*current_task.thread_state.registers);
        // Copy the register state into the mapped VMO.
        restricted_state.write_state(&state);

        // We're about to hand control back to userspace, start measuring time in user code.
        profiling_guard.pivot("RestrictedMode");

        let mut reason_code: zx::sys::zx_restricted_reason_t = u64::MAX;
        let status = zx::Status::from_raw(unsafe {
            // The closure provided to run_with_saved_state must be minimal to avoid using
            // floating point or vector state. In particular, the zx::Status conversion compiles
            // to a vector register operation by default and must happen outside this closure.
            current_task.thread_state.extended_pstate.run_with_saved_state(|| {
                restricted_enter(
                    RESTRICTED_ENTER_OPTIONS,
                    restricted_return_ptr as usize,
                    &mut reason_code,
                )
            })
        });
        match { status } {
            zx::Status::OK => {
                // Successfully entered and exited restricted mode. At this point the task has
                // trapped back out of restricted mode, so the restricted state contains the
                // information about which system call to dispatch.
            }
            _ => return Err(format_err!("failed to restricted_enter: {:?} {:?}", state, status)),
        }

        // We just received control back from r-space, start measuring time in normal mode.
        profiling_guard.pivot("NormalMode");

        // Copy the register state out of the VMO.
        restricted_state.read_state(&mut state);

        match reason_code {
            zx::sys::ZX_RESTRICTED_REASON_SYSCALL => {
                profile_duration!("ExecuteSyscall");
                firehose_trace_duration_begin!(CATEGORY_STARNIX, NAME_EXECUTE_SYSCALL);

                // Store the new register state in the current task before dispatching the system call.
                current_task.thread_state.registers =
                    zx::sys::zx_thread_state_general_regs_t::from(&state).into();

                let syscall_decl = SyscallDecl::from_number(
                    current_task.thread_state.registers.syscall_register(),
                );

                // Generate CFI directives so the unwinder will be redirected to unwind the restricted
                // stack.
                generate_cfi_directives!(state);

                if let Some(new_error_context) = execute_syscall(locked, current_task, syscall_decl)
                {
                    error_context = Some(new_error_context);
                }

                // Restore the CFI directives before continuing.
                restore_cfi_directives!();

                firehose_trace_duration_end!(
                    CATEGORY_STARNIX,
                    NAME_EXECUTE_SYSCALL,
                    ARG_NAME => syscall_decl.name
                );
            }
            zx::sys::ZX_RESTRICTED_REASON_EXCEPTION => {
                firehose_trace_duration!(CATEGORY_STARNIX, NAME_HANDLE_EXCEPTION);
                profile_duration!("HandleException");
                let restricted_exception = restricted_state.read_exception();

                current_task.thread_state.registers =
                    zx::sys::zx_thread_state_general_regs_t::from(&restricted_exception.state)
                        .into();
                let exception_result =
                    current_task.process_exception(&restricted_exception.exception);
                process_completed_exception(current_task, exception_result);
            }
            zx::sys::ZX_RESTRICTED_REASON_KICK => {
                firehose_trace_instant!(
                    CATEGORY_STARNIX,
                    NAME_RESTRICTED_KICK,
                    fuchsia_trace::Scope::Thread
                );
                profile_duration!("RestrictedKick");

                // Update the task's register state.
                current_task.thread_state.registers =
                    zx::sys::zx_thread_state_general_regs_t::from(&state).into();

                // Fall through to the post-syscall / post-exception handling logic. We were likely kicked because a
                // signal is pending deliver or the task has exited. Spurious kicks are also possible.
            }
            _ => {
                return Err(format_err!(
                    "Received unexpected restricted reason code: {}",
                    reason_code
                ));
            }
        }
        if let Some(exit_status) =
            process_completed_restricted_exit(current_task, &error_context, &state)?
        {
            return Ok(exit_status);
        }
    }
}

pub fn get_core_dump_info(task: &Task) -> CoreDumpInfo {
    let process_koid = task
        .thread_group
        .process
        .get_koid()
        .expect("handles for processes with crashing threads are still valid");
    let thread_koid = task
        .thread
        .read()
        .as_ref()
        .expect("coredumps occur in tasks with associated threads")
        .get_koid()
        .expect("handles for crashing threads are still valid");
    let pid = task.thread_group.leader as i64;
    let mut argv = task
        .read_argv()
        .unwrap_or_else(|_| vec!["<unknown>".into()])
        .into_iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(" ");

    let original_len = argv.len();
    argv.truncate(MAX_ARGV_LENGTH - 3);
    if argv.len() < original_len {
        argv.push_str("...");
    }
    CoreDumpInfo { process_koid, thread_koid, pid, argv }
}

pub fn create_zircon_process<L>(
    locked: &mut Locked<'_, L>,
    kernel: &Arc<Kernel>,
    parent: Option<ThreadGroupWriteGuard<'_>>,
    pid: pid_t,
    process_group: Arc<ProcessGroup>,
    signal_actions: Arc<SignalActions>,
    name: &[u8],
) -> Result<TaskInfo, Errno>
where
    L: LockBefore<ProcessGroupState>,
{
    let (process, root_vmar) =
        create_shared(&kernel.kthreads.starnix_process, zx::ProcessOptions::empty(), name)
            .map_err(|status| from_status_like_fdio!(status))?;

    let memory_manager =
        Arc::new(MemoryManager::new(root_vmar).map_err(|status| from_status_like_fdio!(status))?);

    let thread_group = ThreadGroup::new(
        locked,
        kernel.clone(),
        process,
        parent,
        pid,
        process_group,
        signal_actions,
    );

    Ok(TaskInfo { thread: None, thread_group, memory_manager })
}

pub fn execute_task_with_prerun_result<F, R, G>(
    task_builder: TaskBuilder,
    pre_run: F,
    task_complete: G,
    ptrace_state: Option<PtraceCoreState>,
) -> Result<R, Errno>
where
    F: FnOnce(&mut Locked<'_, Unlocked>, &mut CurrentTask) -> Result<R, Errno>
        + Send
        + Sync
        + 'static,
    R: Send + Sync + 'static,
    G: FnOnce(Result<ExitStatus, Error>) + Send + Sync + 'static,
{
    let (sender, receiver) = sync_channel::<Result<R, Errno>>(1);
    execute_task(
        task_builder,
        move |current_task, locked| match pre_run(current_task, locked) {
            Err(errno) => {
                let _ = sender.send(Err(errno.clone()));
                Err(errno)
            }
            Ok(value) => sender.send(Ok(value)).map_err(|error| {
                log_error!("Unable to send `pre_run` result: {error:?}");
                errno!(EINVAL)
            }),
        },
        task_complete,
        ptrace_state,
    );
    receiver.recv().map_err(|e| {
        log_error!("Unable to retrieve result from `pre_run`: {e:?}");
        errno!(EINVAL)
    })?
}

pub fn execute_task<F, G>(
    task_builder: TaskBuilder,
    pre_run: F,
    task_complete: G,
    ptrace_state: Option<PtraceCoreState>,
) where
    F: FnOnce(&mut Locked<'_, Unlocked>, &mut CurrentTask) -> Result<(), Errno>
        + Send
        + Sync
        + 'static,
    G: FnOnce(Result<ExitStatus, Error>) + Send + Sync + 'static,
{
    // Set the process handle to the new task's process, so the new thread is spawned in that
    // process.
    let process_handle = task_builder.task.thread_group.process.raw_handle();
    let old_process_handle = unsafe { thrd_set_zx_process(process_handle) };

    let weak_task = WeakRef::from(&task_builder.task);
    let ref_task = weak_task.upgrade().unwrap();
    if let Some(ptrace_state) = ptrace_state {
        let _ = ptrace_attach_from_state(&task_builder.task, ptrace_state);
    }

    // Hold a lock on the task's thread slot until we have a chance to initialize it.
    let mut task_thread_guard = ref_task.thread.write();

    // Spawn the process' thread. Note, this closure ends up executing in the process referred to by
    // `process_handle`.
    let join_handle = std::thread::Builder::new()
        .name("user-thread".to_string())
        .spawn(move || {
            let mut locked = Unlocked::new();
            let mut current_task: CurrentTask = task_builder.into();
            let pre_run_result = pre_run(&mut locked, &mut current_task);
            if pre_run_result.is_err() {
                log_error!("Pre run failed from {pre_run_result:?}. The task will not be run.");
            } else {
                let run_result = match run_task(&mut locked, &mut current_task) {
                    Err(error) => {
                        log_warn!("Died unexpectedly from {error:?}! treating as SIGKILL");
                        let exit_status = ExitStatus::Kill(SignalInfo::default(SIGKILL));

                        current_task.write().set_exit_status(exit_status.clone());
                        Ok(exit_status)
                    }
                    ok => ok,
                };

                task_complete(run_result);
            }

            // `release` must be called as the absolute last action on this thread to ensure that
            // any deferred release are done before it.
            current_task.release(&mut locked);
        })
        .expect("able to spawn threads");

    // Set the task's thread handle
    let pthread = join_handle.as_pthread_t();
    let raw_thread_handle =
        unsafe { zx::Unowned::<'_, zx::Thread>::from_raw_handle(thrd_get_zx_handle(pthread)) };
    *task_thread_guard = Some(raw_thread_handle.duplicate(zx::Rights::SAME_RIGHTS).unwrap());

    // Reset the process handle used to create threads.
    unsafe {
        thrd_set_zx_process(old_process_handle);
    };

    // Now that the task has a thread handle, update the thread's role using the policy configured.
    drop(task_thread_guard);
    if let Err(err) = ref_task.sync_scheduler_policy_to_role() {
        log_warn!(?err, "Couldn't update freshly spawned thread's profile.");
    }
}

fn process_completed_exception(current_task: &mut CurrentTask, exception_result: ExceptionResult) {
    match exception_result {
        ExceptionResult::Handled => {}
        ExceptionResult::Signal(signal) => {
            // TODO: Verify that the rip is actually in restricted code.
            let mut registers = current_task.thread_state.registers;
            registers.reset_flags();
            {
                let mut task_state = current_task.task.write();
                if task_state.ptrace_on_signal_consume() {
                    task_state.set_stopped(
                        StopState::SignalDeliveryStopping,
                        Some(signal.clone()),
                        Some(&current_task),
                        None,
                    );
                    return;
                }

                if let Some(status) = deliver_signal(
                    current_task,
                    task_state,
                    signal,
                    &mut registers,
                    &current_task.thread_state.extended_pstate,
                ) {
                    current_task.thread_group_exit(status);
                }
            }
            current_task.thread_state.registers = registers;
        }
    }
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

/// Notifies the debugger, if one is attached, that the module list might have been changed.
///
/// For more information about the debugger protocol, see:
/// https://cs.opensource.google/fuchsia/fuchsia/+/master:src/developer/debug/debug_agent/process_handle.h;l=31
///
/// # Parameters:
/// - `current_task`: The task to set the property for. The register's of this task, the instruction
///                   pointer specifically, needs to be set to the value with which the task is
///                   expected to resume.
pub fn notify_debugger_of_module_list(current_task: &mut CurrentTask) -> Result<(), Errno> {
    let break_on_load = current_task
        .thread_group
        .process
        .get_break_on_load()
        .map_err(|err| from_status_like_fdio!(err))?;

    // If break on load is 0, there is no debugger attached, so return before issuing the software
    // breakpoint.
    if break_on_load == 0 {
        return Ok(());
    }

    // For restricted executor, we only need to trigger the debug break on the current thread.
    let breakpoint_addr = breakpoint_for_module_changes as usize as u64;

    if breakpoint_addr != break_on_load {
        current_task
            .thread_group
            .process
            .set_break_on_load(&breakpoint_addr)
            .map_err(|err| from_status_like_fdio!(err))?;
    }

    unsafe {
        breakpoint_for_module_changes();
    }

    Ok(())
}

pub fn interrupt_thread(thread: &zx::Thread) {
    let status = unsafe { zx::sys::zx_restricted_kick(thread.raw_handle(), 0) };
    if status != zx::sys::ZX_OK {
        // zx_restricted_kick() could return ZX_ERR_BAD_STATE if the target thread is already in the
        // DYING or DEAD states. That's fine since it means that the task is in the process of
        // tearing down, so allow it.
        assert_eq!(status, zx::sys::ZX_ERR_BAD_STATE);
    }
}
