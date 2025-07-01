// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::MemoryManager;
use crate::security;
use crate::signals::SignalActions;
use crate::task::{
    CurrentTask, Kernel, PidTable, ProcessGroup, RobustListHeadPtr, SeccompFilterContainer,
    SeccompState, Task, TaskBuilder, ThreadGroup, ThreadGroupParent, ThreadGroupWriteGuard,
};
use crate::vfs::{FdTable, FsContext};
use starnix_sync::{
    LockBefore, Locked, ProcessGroupState, RwLockWriteGuard, TaskRelease, Unlocked,
};
use starnix_types::arch::ArchWidth;
use starnix_types::ownership::{OwnedRef, Releasable, ReleaseGuard, Share, TempRef};
use starnix_types::release_on_error;
use starnix_uapi::auth::Credentials;
use starnix_uapi::errors::Errno;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::signals::{Signal, SIGCHLD};
use starnix_uapi::{errno, error, from_status_like_fdio, pid_t, rlimit};
use std::ffi::CString;
use std::sync::Arc;
use zx::AsHandleRef;

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
    type Context<'a> = &'a mut PidTable;

    fn release<'a>(self, pids: &'a mut PidTable) {
        self.thread_group.release(pids);
    }
}

pub fn create_zircon_process<L>(
    locked: &mut Locked<L>,
    kernel: &Arc<Kernel>,
    parent: Option<ThreadGroupWriteGuard<'_>>,
    pid: pid_t,
    exit_signal: Option<Signal>,
    process_group: Arc<ProcessGroup>,
    signal_actions: Arc<SignalActions>,
    name: &[u8],
) -> Result<ReleaseGuard<TaskInfo>, Errno>
where
    L: LockBefore<ProcessGroupState>,
{
    // Don't allow new processes to be created once the kernel has started shutting down.
    if kernel.is_shutting_down() {
        return error!(EBUSY);
    }
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
        exit_signal,
        process_group,
        signal_actions,
    );

    Ok(TaskInfo { thread: None, thread_group, memory_manager: Some(memory_manager) }.into())
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

/// Create a process that is a child of the `init` process.
///
/// The created process will be a task that is the leader of a new thread group.
///
/// Most processes are created by userspace and are descendants of the `init` process. In
/// some situations, the kernel needs to create a process itself. This function is the
/// preferred way of creating an actual userspace process because making the process a child of
/// `init` means that `init` is responsible for waiting on the process when it dies and thereby
/// cleaning up its zombie.
///
/// If you just need a kernel task, and not an entire userspace process, consider using
/// `create_system_task` instead. Even better, consider using the `kthreads` threadpool.
///
/// If `seclabel` is set, or the container specified a `default_seclabel`, then it will be
/// resolved against the `kernel`'s active security policy, and applied to the new task.
/// Otherwise the task will inherit its LSM state from the "init" task.
///
/// This function creates an underlying Zircon process to host the new task.
pub fn create_init_child_process<L>(
    locked: &mut Locked<L>,
    kernel: &Arc<Kernel>,
    initial_name: &CString,
    seclabel: Option<&CString>,
) -> Result<TaskBuilder, Errno>
where
    L: LockBefore<TaskRelease>,
{
    let weak_init = kernel.pids.read().get_task(1);
    let init_task = weak_init.upgrade().ok_or_else(|| errno!(EINVAL))?;
    let initial_name_bytes = initial_name.as_bytes().to_owned();

    let security_context = if let Some(seclabel) = seclabel {
        security::task_for_context(&init_task, seclabel.as_bytes().into())?
    } else if let Some(default_seclabel) = kernel.features.default_seclabel.as_ref() {
        security::task_for_context(&init_task, default_seclabel.as_bytes().into())?
    } else {
        // If SELinux is enabled then this call will fail with `EINVAL`.
        security::task_for_context(&init_task, b"".into()).map_err(|_| {
            errno!(EINVAL, "Container has SELinux enabled but no Security Context specified")
        })?
    };

    let task = create_task(
        locked,
        kernel,
        initial_name.clone(),
        init_task.fs().fork(),
        |locked, pid, process_group| {
            create_zircon_process(
                locked.cast_locked::<TaskRelease>(),
                kernel,
                None,
                pid,
                Some(SIGCHLD),
                process_group,
                SignalActions::default(),
                &initial_name_bytes,
            )
        },
        security_context,
    )?;
    {
        let mut init_writer = init_task.thread_group().write();
        let mut new_process_writer = task.thread_group().write();
        new_process_writer.parent = Some(ThreadGroupParent::from(init_task.thread_group()));
        init_writer.children.insert(task.tid, OwnedRef::downgrade(task.thread_group()));
    }
    // A child process created via fork(2) inherits its parent's
    // resource limits.  Resource limits are preserved across execve(2).
    let limits = init_task.thread_group().limits.lock(locked.cast_locked::<TaskRelease>()).clone();
    *task.thread_group().limits.lock(locked.cast_locked::<TaskRelease>()) = limits;
    Ok(task)
}

/// Creates the initial process for a kernel.
///
/// The created process will be a task that is the leader of a new thread group.
///
/// The init process is special because it's the root of the parent/child relationship between
/// tasks. If a task dies, the init process is ultimately responsible for waiting on that task
/// and removing it from the zombie list.
///
/// It's possible for the kernel to create tasks whose ultimate parent isn't init, but such
/// tasks cannot be created by userspace directly.
///
/// This function should only be called as part of booting a kernel instance. To create a
/// process after the kernel has already booted, consider `create_init_child_process`
/// or `create_system_task`.
///
/// The process created by this function should always have pid 1. We require the caller to
/// pass the `pid` as an argument to clarify that it's the callers responsibility to determine
/// the pid for the process.
pub fn create_init_process(
    locked: &mut Locked<Unlocked>,
    kernel: &Arc<Kernel>,
    pid: pid_t,
    initial_name: CString,
    fs: Arc<FsContext>,
    rlimits: &[(Resource, u64)],
) -> Result<TaskBuilder, Errno> {
    let initial_name_bytes = initial_name.as_bytes().to_owned();
    let pids = kernel.pids.write();
    create_task_with_pid(
        locked,
        kernel,
        pids,
        pid,
        initial_name,
        fs,
        |locked, pid, process_group| {
            create_zircon_process(
                locked,
                kernel,
                None,
                pid,
                Some(SIGCHLD),
                process_group,
                SignalActions::default(),
                &initial_name_bytes,
            )
        },
        Credentials::root(),
        rlimits,
        // If SELinux is enabled then `exec()` of the "init" executable will normally be
        // configured by policy to transition to the desired init task Security Context.
        security::task_alloc_for_kernel(),
    )
}

/// Create a task that runs inside the kernel.
///
/// There is no underlying Zircon process to host the task. Instead, the work done by this task
/// is performed by a thread in the original Starnix process, possible as part of a thread
/// pool.
///
/// This function is the preferred way to create a context for doing background work inside the
/// kernel.
///
/// Rather than calling this function directly, consider using `kthreads`, which provides both
/// a system task and a threadpool on which the task can do work.
pub fn create_system_task<L>(
    locked: &mut Locked<L>,
    kernel: &Arc<Kernel>,
    fs: Arc<FsContext>,
) -> Result<CurrentTask, Errno>
where
    L: LockBefore<TaskRelease>,
{
    let builder = create_task(
        locked,
        kernel,
        CString::new("kthreadd").unwrap(),
        fs,
        |locked, pid, process_group| {
            let process = zx::Process::from(zx::Handle::invalid());
            let thread_group = ThreadGroup::new(
                locked.cast_locked::<TaskRelease>(),
                kernel.clone(),
                process,
                None,
                pid,
                Some(SIGCHLD),
                process_group,
                SignalActions::default(),
            );
            Ok(TaskInfo { thread: None, thread_group, memory_manager: None }.into())
        },
        security::task_alloc_for_kernel(),
    )?;
    Ok(builder.into())
}

pub fn create_task<F, L>(
    locked: &mut Locked<L>,
    kernel: &Kernel,
    initial_name: CString,
    root_fs: Arc<FsContext>,
    task_info_factory: F,
    security_state: security::TaskState,
) -> Result<TaskBuilder, Errno>
where
    F: FnOnce(&mut Locked<L>, i32, Arc<ProcessGroup>) -> Result<ReleaseGuard<TaskInfo>, Errno>,
    L: LockBefore<TaskRelease>,
{
    let mut pids = kernel.pids.write();
    let pid = pids.allocate_pid();
    create_task_with_pid(
        locked,
        kernel,
        pids,
        pid,
        initial_name,
        root_fs,
        task_info_factory,
        Credentials::root(),
        &[],
        security_state,
    )
}

fn create_task_with_pid<F, L>(
    locked: &mut Locked<L>,
    kernel: &Kernel,
    mut pids: RwLockWriteGuard<'_, PidTable>,
    pid: pid_t,
    initial_name: CString,
    root_fs: Arc<FsContext>,
    task_info_factory: F,
    creds: Credentials,
    rlimits: &[(Resource, u64)],
    security_state: security::TaskState,
) -> Result<TaskBuilder, Errno>
where
    F: FnOnce(&mut Locked<L>, i32, Arc<ProcessGroup>) -> Result<ReleaseGuard<TaskInfo>, Errno>,
    L: LockBefore<TaskRelease>,
{
    debug_assert!(pids.get_task(pid).upgrade().is_none());

    let process_group = ProcessGroup::new(pid, None);
    pids.add_process_group(&process_group);

    let TaskInfo { thread, thread_group, memory_manager } =
        ReleaseGuard::take(task_info_factory(locked, pid, process_group.clone())?);

    process_group.insert(locked.cast_locked::<TaskRelease>(), &thread_group);

    // > The timer slack values of init (PID 1), the ancestor of all processes, are 50,000
    // > nanoseconds (50 microseconds).  The timer slack value is inherited by a child created
    // > via fork(2), and is preserved across execve(2).
    // https://man7.org/linux/man-pages/man2/prctl.2.html
    let default_timerslack = 50_000;
    let builder = TaskBuilder {
        task: Task::new(
            pid,
            initial_name,
            thread_group,
            thread,
            FdTable::default(),
            memory_manager,
            root_fs,
            creds,
            Arc::clone(&kernel.default_abstract_socket_namespace),
            Arc::clone(&kernel.default_abstract_vsock_namespace),
            Default::default(),
            Default::default(),
            None,
            Default::default(),
            kernel.root_uts_ns.clone(),
            false,
            SeccompState::default(),
            SeccompFilterContainer::default(),
            RobustListHeadPtr::null(&ArchWidth::Arch64),
            default_timerslack,
            security_state,
        ),
        thread_state: Default::default(),
    };
    release_on_error!(builder, locked, {
        let temp_task = TempRef::from(&builder.task);
        builder.thread_group().add(&temp_task)?;
        for (resource, limit) in rlimits {
            builder
                .thread_group()
                .limits
                .lock(locked.cast_locked::<TaskRelease>())
                .set(*resource, rlimit { rlim_cur: *limit, rlim_max: *limit });
        }

        pids.add_task(&temp_task);
        pids.add_thread_group(builder.thread_group());
        Ok(())
    });
    Ok(builder)
}

/// Create a kernel task in the same ThreadGroup as the given `system_task`.
///
/// There is no underlying Zircon thread to host the task.
pub fn create_kernel_thread<L>(
    locked: &mut Locked<L>,
    system_task: &Task,
    initial_name: CString,
) -> Result<CurrentTask, Errno>
where
    L: LockBefore<TaskRelease>,
{
    let mut pids = system_task.kernel().pids.write();
    let pid = pids.allocate_pid();

    let scheduler_state;
    let uts_ns;
    let default_timerslack_ns;
    let security_state;
    {
        let state = system_task.read();
        scheduler_state = state.scheduler_state;
        uts_ns = state.uts_ns.clone();
        default_timerslack_ns = state.default_timerslack_ns;
        security_state = security::task_alloc_for_kernel();
    }

    let current_task: CurrentTask = TaskBuilder::new(Task::new(
        pid,
        initial_name,
        OwnedRef::share(system_task.thread_group()),
        None,
        FdTable::default(),
        system_task.mm().cloned(),
        system_task.fs(),
        system_task.creds(),
        Arc::clone(&system_task.abstract_socket_namespace),
        Arc::clone(&system_task.abstract_vsock_namespace),
        Default::default(),
        Default::default(),
        None,
        scheduler_state,
        uts_ns,
        false,
        SeccompState::default(),
        SeccompFilterContainer::default(),
        RobustListHeadPtr::null(&ArchWidth::Arch64),
        default_timerslack_ns,
        security_state,
    ))
    .into();
    release_on_error!(current_task, locked, {
        let temp_task = current_task.temp_task();
        current_task.thread_group().add(&temp_task)?;
        pids.add_task(&temp_task);
        Ok(())
    });
    Ok(current_task)
}
