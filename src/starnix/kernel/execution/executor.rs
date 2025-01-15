// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]

use crate::arch::execution::new_syscall;
use crate::mm::MemoryManager;
use crate::signals::{
    deliver_signal, dequeue_signal, prepare_to_restart_syscall, SignalActions, SignalInfo,
};
use crate::syscalls::table::dispatch_syscall;
use crate::task::{
    ptrace_attach_from_state, ptrace_syscall_enter, ptrace_syscall_exit, CurrentTask,
    ExceptionResult, ExitStatus, Kernel, ProcessGroup, PtraceCoreState, SeccompStateValue,
    StopState, TaskBuilder, TaskFlags, ThreadGroup, ThreadGroupWriteGuard,
};
use crate::vfs::DelayedReleaser;
use anyhow::{format_err, Error};
#[cfg(feature = "syscall_stats")]
use fuchsia_inspect::NumericProperty;
use fuchsia_inspect_contrib::{profile_duration, ProfileDuration};
use starnix_logging::{
    firehose_trace_duration, firehose_trace_duration_begin, firehose_trace_duration_end,
    firehose_trace_instant, log_error, log_trace, log_warn, set_current_task_info, ARG_NAME,
    CATEGORY_STARNIX, NAME_EXECUTE_SYSCALL, NAME_HANDLE_EXCEPTION, NAME_READ_RESTRICTED_STATE,
    NAME_RESTRICTED_KICK, NAME_RUN_TASK, NAME_WRITE_RESTRICTED_STATE,
};
use starnix_sync::{LockBefore, Locked, ProcessGroupState, TaskRelease, Unlocked};
use starnix_syscalls::decls::{Syscall, SyscallDecl};
use starnix_syscalls::SyscallResult;
use starnix_types::ownership::{OwnedRef, Releasable, ReleaseGuard, WeakRef};
use starnix_uapi::errors::Errno;
use starnix_uapi::signals::SIGKILL;
use starnix_uapi::{errno, from_status_like_fdio, pid_t};
use std::os::unix::thread::JoinHandleExt;
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use zx::{
    AsHandleRef, {self as zx},
};

extern "C" {
    fn restricted_enter_loop(
        options: u32,
        restricted_return: usize,
        restricted_exit_callback: usize,
        restricted_exit_callback_context: usize,
        restricted_state_addr: usize,
        extended_pstate_addr: usize,
    ) -> zx::sys::zx_status_t;

    fn restricted_return_loop();

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
    pub fn from_vmo(state_vmo: zx::Vmo) -> Result<Self, zx::Status> {
        // Map the restricted state VMO and arrange for it to be unmapped later.
        let state_size = state_vmo.get_size()? as usize;
        let state_address = fuchsia_runtime::vmar_root_self().map(
            0,
            &state_vmo,
            0,
            state_size,
            zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )?;
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

struct RestrictedEnterContext<'a> {
    current_task: &'a mut CurrentTask,
    restricted_state: RestrictedState,
    state: zx::sys::zx_restricted_state_t,
    profiling_guard: ProfileDuration,
    error_context: Option<ErrorContext>,
    exit_status: Result<ExitStatus, Error>,
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
fn run_task(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    mut restricted_state: RestrictedState,
) -> Result<ExitStatus, Error> {
    let mut profiling_guard = ProfileDuration::enter("TaskLoopSetup");

    set_current_task_info(
        &current_task.task.command(),
        current_task.task.thread_group.leader,
        current_task.id,
    );

    firehose_trace_duration!(CATEGORY_STARNIX, NAME_RUN_TASK);

    // This is the pointer that is passed to `restricted_enter`.
    let restricted_return_ptr = restricted_return_loop as *const ();

    // This tracks the last failing system call for debugging purposes.
    let error_context = None;

    // We need to check for exit once, before the task starts executing, in case
    // the task has already been sent a signal that will cause it to exit.
    if let Some(exit_status) =
        process_completed_restricted_exit(locked, current_task, &error_context)?
    {
        return Ok(exit_status);
    }

    let state = zx::sys::zx_restricted_state_t::from(&*current_task.thread_state.registers);
    // Copy the initial register state into the mapped VMO.
    restricted_state.write_state(&state);

    let restricted_state_addr = restricted_state.bound_state.as_ptr() as usize;
    let extended_pstate_addr = &current_task.thread_state.extended_pstate as *const _ as usize;

    // We're about to hand control to userspace, start measuring time in user code.
    profiling_guard.pivot("RestrictedMode");

    let restricted_enter_context = RestrictedEnterContext {
        current_task,
        restricted_state,
        state,
        profiling_guard,
        error_context,
        exit_status: Err(errno!(ENOEXEC).into()),
    };

    unsafe {
        restricted_enter_loop(
            RESTRICTED_ENTER_OPTIONS,
            restricted_return_ptr as usize,
            restricted_exit_callback_c as usize,
            &restricted_enter_context as *const _ as usize,
            restricted_state_addr,
            extended_pstate_addr,
        );
    }
    restricted_enter_context.exit_status
}

extern "C" fn restricted_exit_callback_c(
    context: usize,
    reason_code: zx::sys::zx_restricted_reason_t,
) -> bool {
    let restricted_context = unsafe { &mut *(context as *mut RestrictedEnterContext<'_>) };
    restricted_exit_callback(
        reason_code,
        restricted_context.current_task,
        &mut restricted_context.restricted_state,
        &mut restricted_context.state,
        &mut restricted_context.profiling_guard,
        &mut restricted_context.error_context,
        &mut restricted_context.exit_status,
    )
}

fn restricted_exit_callback(
    reason_code: zx::sys::zx_restricted_reason_t,
    current_task: &mut CurrentTask,
    restricted_state: &mut RestrictedState,
    state: &mut zx::sys::zx_restricted_state_t,
    profiling_guard: &mut ProfileDuration,
    error_context: &mut Option<ErrorContext>,
    exit_status: &mut Result<ExitStatus, Error>,
) -> bool {
    debug_assert_eq!(
        current_task.thread_state.restart_code, None,
        "restart_code should only ever be Some() in normal mode",
    );

    let ret = match process_restricted_exit(
        reason_code,
        current_task,
        restricted_state,
        state,
        profiling_guard,
        error_context,
    ) {
        Ok(None) => {
            // Keep going!
            true
        }
        Ok(Some(completed_exit_status)) => {
            *exit_status = Ok(completed_exit_status);
            false
        }
        Err(error) => {
            *exit_status = Err(error);
            false
        }
    };

    debug_assert_eq!(
        current_task.thread_state.restart_code, None,
        "restart_code should only ever be Some() in normal mode",
    );

    ret
}

fn process_restricted_exit(
    reason_code: zx::sys::zx_restricted_reason_t,
    current_task: &mut CurrentTask,
    restricted_state: &mut RestrictedState,
    state: &mut zx::sys::zx_restricted_state_t,
    profiling_guard: &mut ProfileDuration,
    error_context: &mut Option<ErrorContext>,
) -> Result<Option<ExitStatus>, Error> {
    // We just received control back from r-space, start measuring time in normal mode.
    profiling_guard.pivot("NormalMode");

    // We can't hold any locks entering restricted mode so we can't be holding any locks on exit.
    let mut locked = unsafe { Unlocked::new() };

    // Copy the register state out of the VMO.
    restricted_state.read_state(state);

    match reason_code {
        zx::sys::ZX_RESTRICTED_REASON_SYSCALL => {
            profile_duration!("ExecuteSyscall");
            firehose_trace_duration_begin!(CATEGORY_STARNIX, NAME_EXECUTE_SYSCALL);

            // Store the new register state in the current task before dispatching the system call.
            current_task.thread_state.registers =
                zx::sys::zx_thread_state_general_regs_t::from(&*state).into();

            let syscall_decl = SyscallDecl::from_number(
                current_task.thread_state.registers.syscall_register(),
                current_task.thread_state.arch_width,
            );

            if let Some(new_error_context) =
                execute_syscall(&mut locked, current_task, syscall_decl)
            {
                *error_context = Some(new_error_context);
            }

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
                zx::sys::zx_thread_state_general_regs_t::from(&restricted_exception.state).into();
            let exception_result = current_task.process_exception(&restricted_exception.exception);
            process_completed_exception(&mut locked, current_task, exception_result);
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
                zx::sys::zx_thread_state_general_regs_t::from(&*state).into();

            // Fall through to the post-syscall / post-exception handling logic. We were likely kicked because a
            // signal is pending deliver or the task has exited. Spurious kicks are also possible.
        }
        _ => {
            return Err(format_err!("Received unexpected restricted reason code: {}", reason_code));
        }
    }
    if let Some(exit_status) =
        process_completed_restricted_exit(&mut locked, current_task, &error_context)?
    {
        return Ok(Some(exit_status));
    }

    // Copy the updated register state into the mapped VMO.
    let state = zx::sys::zx_restricted_state_t::from(&*current_task.thread_state.registers);
    restricted_state.write_state(&state);

    Ok(None)
}

pub fn create_zircon_process<L>(
    locked: &mut Locked<'_, L>,
    kernel: &Arc<Kernel>,
    parent: Option<ThreadGroupWriteGuard<'_>>,
    pid: pid_t,
    process_group: Arc<ProcessGroup>,
    signal_actions: Arc<SignalActions>,
    name: &[u8],
) -> Result<ReleaseGuard<TaskInfo>, Errno>
where
    L: LockBefore<ProcessGroupState>,
{
    let (process, root_vmar) =
        create_shared(&kernel.kthreads.starnix_process, zx::ProcessOptions::empty(), name)
            .map_err(|status| from_status_like_fdio!(status))?;

    // Make sure that if this process panics in normal mode that the whole kernel's job is killed.
    fuchsia_runtime::job_default()
        .set_critical(zx::JobCriticalOptions::RETCODE_NONZERO, &process)
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

    Ok(TaskInfo { thread: None, thread_group, memory_manager: Some(memory_manager) }.into())
}

pub fn execute_task_with_prerun_result<L, F, R, G>(
    locked: &mut Locked<'_, L>,
    task_builder: TaskBuilder,
    pre_run: F,
    task_complete: G,
    ptrace_state: Option<PtraceCoreState>,
) -> Result<R, Errno>
where
    L: LockBefore<TaskRelease>,
    F: FnOnce(&mut Locked<'_, Unlocked>, &mut CurrentTask) -> Result<R, Errno>
        + Send
        + Sync
        + 'static,
    R: Send + Sync + 'static,
    G: FnOnce(Result<ExitStatus, Error>) + Send + Sync + 'static,
{
    let (sender, receiver) = sync_channel::<Result<R, Errno>>(1);
    execute_task(
        locked,
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
    )?;
    receiver.recv().map_err(|e| {
        log_error!("Unable to retrieve result from `pre_run`: {e:?}");
        errno!(EINVAL)
    })?
}

pub fn execute_task<L, F, G>(
    locked: &mut Locked<'_, L>,
    task_builder: TaskBuilder,
    pre_run: F,
    task_complete: G,
    ptrace_state: Option<PtraceCoreState>,
) -> Result<(), Errno>
where
    L: LockBefore<TaskRelease>,
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
    let (sender, receiver) = sync_channel::<TaskBuilder>(1);
    let result = std::thread::Builder::new().name("user-thread".to_string()).spawn(move || {
        // It's safe to create a new lock context since we are on a new thread.
        let mut locked = unsafe { Unlocked::new() };

        // Note, cross-process shared resources allocated in this function that aren't freed by the
        // Zircon kernel upon thread and/or process termination (like mappings in the shared region)
        // should be freed using the delayed finalizer mechanism and Task drop.
        let mut current_task: CurrentTask = receiver
            .recv()
            .expect("caller should always send task builder before disconnecting")
            .into();

        // We don't need the receiver anymore. If we don't drop the receiver now, we'll keep it
        // allocated for the lifetime of the thread.
        std::mem::drop(receiver);

        let pre_run_result = { pre_run(&mut locked, &mut current_task) };
        if pre_run_result.is_err() {
            log_error!("Pre run failed from {pre_run_result:?}. The task will not be run.");

            // Drop the task_complete callback to ensure that the closure isn't holding any
            // releasables.
            std::mem::drop(task_complete);
        } else {
            // Allocate a VMO and bind it to this thread.
            let mut out_vmo_handle = 0;
            let status =
                zx::Status::from_raw(unsafe { zx_restricted_bind_state(0, &mut out_vmo_handle) });
            match { status } {
                zx::Status::OK => {
                    // We've successfully attached the VMO to the current thread. This VMO will be
                    // mapped and used for the kernel to store restricted mode register state as it
                    // enters and exits restricted mode.
                }
                _ => panic!("zx_restricted_bind_state failed with {status}!"),
            }
            let state_vmo = unsafe { zx::Vmo::from(zx::Handle::from_raw(out_vmo_handle)) };

            // Unbind when we leave this scope to avoid unnecessarily retaining the VMO via this
            // thread's binding.  Of course, we'll still have to remove any mappings and close any
            // handles that refer to the VMO to ensure it will be destroyed.  See note about
            // preventing resource leaks in this function's documentation.
            scopeguard::defer! {
                unsafe { zx_restricted_unbind_state(0); }
            }

            // Map the restricted state VMO and arrange for it to be unmapped later.
            let exit_status = match RestrictedState::from_vmo(state_vmo) {
                Ok(restricted_state) => {
                    match run_task(&mut locked, &mut current_task, restricted_state) {
                        Ok(ok) => ok,
                        Err(error) => {
                            log_warn!("Died unexpectedly from {error:?}! treating as SIGKILL");
                            ExitStatus::Kill(SignalInfo::default(SIGKILL))
                        }
                    }
                }
                Err(error) => {
                    log_error!("failed to map mode state vmo, {error:?}! treating as SIGKILL");
                    ExitStatus::Kill(SignalInfo::default(SIGKILL))
                }
            };

            current_task.write().set_exit_status(exit_status.clone());
            task_complete(Ok(exit_status));
        }

        // `release` must be called as the absolute last action on this thread to ensure that
        // any deferred release are done before it.
        current_task.release(&mut locked);

        // Ensure that no releasables are registered after this point as we unwind the stack.
        DelayedReleaser::finalize();
    });
    let join_handle = match result {
        Ok(handle) => handle,
        Err(e) => {
            task_builder.release(locked);
            match e.kind() {
                std::io::ErrorKind::WouldBlock => return Err(errno!(EAGAIN)),
                other => panic!("unexpected error on thread spawn: {other}"),
            }
        }
    };
    // Wait to send the `TaskBuilder` to the spawned thread until we know that it
    // spawned successfully, as we need to ensure the builder is always explicitly
    // released.
    sender
        .send(task_builder)
        .expect("receiver should not be disconnected because thread spawned successfully");

    // We're done with the sender now. We drop the sender explicitly to free the memory earlier.
    std::mem::drop(sender);

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
        log_warn!(err:?; "Couldn't update freshly spawned thread's profile.");
    }

    Ok(())
}

fn process_completed_exception(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    exception_result: ExceptionResult,
) {
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
                        Some(signal),
                        Some(&current_task),
                        None,
                    );
                    return;
                }

                if let Some(status) = deliver_signal(
                    current_task,
                    task_state,
                    signal.into(),
                    &mut registers,
                    &current_task.thread_state.extended_pstate,
                ) {
                    current_task.thread_group_exit(locked, status);
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
fn create_shared(
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

/// Contains context to track the most recently failing system call.
///
/// When a task exits with a non-zero exit code, this context is logged to help debugging which
/// system call may have triggered the failure.
#[derive(Debug)]
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
    pub memory_manager: Option<Arc<MemoryManager>>,
}

impl Releasable for TaskInfo {
    type Context<'a: 'b, 'b> = ();

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
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
        ptrace_syscall_enter(locked, current_task);
    }

    log_trace!("{:?}", syscall);

    let result: Result<SyscallResult, Errno> =
        if current_task.seccomp_filter_state.get() != SeccompStateValue::None {
            // Inlined fast path for seccomp, so that we don't incur the cost
            // of a method call when running the filters.
            if let Some(res) = current_task.run_seccomp_filters(locked, &syscall) {
                res
            } else {
                dispatch_syscall(locked, current_task, &syscall)
            }
        } else {
            dispatch_syscall(locked, current_task, &syscall)
        };

    current_task.trigger_delayed_releaser(locked);

    let return_value = match result {
        Ok(return_value) => {
            log_trace!("-> {:#x}", return_value.value());
            current_task.thread_state.registers.set_return_register(return_value.value());
            None
        }
        Err(errno) => {
            log_trace!("!-> {:?}", errno);
            if errno.is_restartable() {
                current_task.thread_state.restart_code = Some(errno.code);
            }
            current_task.thread_state.registers.set_return_register(errno.return_value());
            Some(ErrorContext { error: errno, syscall })
        }
    };

    if current_task.trace_syscalls.load(std::sync::atomic::Ordering::Relaxed) {
        ptrace_syscall_exit(locked, current_task, return_value.is_some());
    }

    return_value
}

/// Finishes `current_task` updates after a restricted mode exit such as a syscall, exception, or kick.
///
/// Returns an `ExitStatus` if the task is meant to exit.
pub fn process_completed_restricted_exit(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    error_context: &Option<ErrorContext>,
) -> Result<Option<ExitStatus>, Errno> {
    let result;
    loop {
        // Checking for a signal might cause the task to exit, so check before processing exit
        {
            {
                if !current_task.is_exitted() {
                    dequeue_signal(locked, current_task);
                }
                // The syscall may need to restart for a non-signal-related
                // reason. This call does nothing if we aren't restarting.
                prepare_to_restart_syscall(&mut current_task.thread_state, None);
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
            current_task.block_while_stopped(locked);
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
        let (_kernel, mut task, mut locked) = create_kernel_task_and_unlocked();

        // block_while_stopped must immediately returned if the task is not stopped.
        task.block_while_stopped(&mut locked);

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
        task.block_while_stopped(&mut locked);

        // Join the thread, which will ensure set_stopped terminated.
        thread.join().expect("joined");

        // The task should not be blocked anymore.
        task.block_while_stopped(&mut locked);
    }

    #[::fuchsia::test]
    async fn test_block_while_stopped_stop_and_exit() {
        let (_kernel, mut task, mut locked) = create_kernel_task_and_unlocked();

        // block_while_stopped must immediately returned if the task is neither stopped nor exited.
        task.block_while_stopped(&mut locked);

        // Stop the task.
        task.thread_group.set_stopped(
            StopState::GroupStopping,
            Some(SignalInfo::default(SIGSTOP)),
            false,
        );

        let thread = std::thread::spawn({
            let task = task.weak_task();
            move || {
                let mut locked = unsafe { Unlocked::new() };
                let task = task.upgrade().expect("task must be alive");
                // Wait for the task to have a waiter.
                while !task.read().is_blocked() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }

                // exit the task.
                task.thread_group.exit(&mut locked, ExitStatus::Exit(1), None);
            }
        });

        // Block until continued.
        task.block_while_stopped(&mut locked);

        // Join the task, which will ensure thread_group.exit terminated.
        thread.join().expect("joined");

        // The task should not be blocked because it is stopped.
        task.block_while_stopped(&mut locked);
    }
}
