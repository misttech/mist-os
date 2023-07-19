// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fprocess;
#[cfg(feature = "syscall_stats")]
use fuchsia_inspect::NumericProperty;
use fuchsia_runtime::{HandleInfo, HandleType};
use fuchsia_zircon::{self as zx};
use std::{convert::TryFrom, sync::Arc};

use crate::{
    fs::{
        fuchsia::{create_file_from_handle, RemoteBundle, RemoteFs, SyslogFile},
        *,
    },
    logging::log_trace,
    mm::MemoryManager,
    signals::dequeue_signal,
    syscalls::{
        decls::{Syscall, SyscallDecl},
        table::dispatch_syscall,
        SyscallResult,
    },
    task::*,
    types::*,
};

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
    pub thread_group: Arc<ThreadGroup>,

    /// The memory manager to use for the task.
    pub memory_manager: Arc<MemoryManager>,
}

/// Executes the provided `syscall` in `current_task`.
///
/// Returns an `ErrorContext` if the system call returned an error.
pub fn execute_syscall(
    current_task: &mut CurrentTask,
    syscall_decl: SyscallDecl,
) -> Option<ErrorContext> {
    #[cfg(feature = "syscall_stats")]
    SyscallDecl::stats_property(syscall_decl.number).add(1);
    trace_duration!(
        trace_category_starnix!(),
        trace_name_execute_syscall!(),
        trace_arg_name!() => syscall_decl.name
    );

    let syscall = Syscall::new(syscall_decl, current_task);

    current_task.registers.save_registers_for_restart(&syscall);

    log_trace!("{:?}", syscall);

    let result: Result<SyscallResult, Errno> =
        if current_task.seccomp_filter_state.get() != SeccompStateValue::None {
            // Inlined fast path for seccomp, so that we don't incur the cost
            // of a method call when running the filters.
            if let Some(res) = current_task.run_seccomp_filters(&syscall) {
                res
            } else {
                dispatch_syscall(current_task, &syscall)
            }
        } else {
            dispatch_syscall(current_task, &syscall)
        };

    match result {
        Ok(return_value) => {
            log_trace!("-> {:#x}", return_value.value());
            current_task.registers.set_return_register(return_value.value());
            None
        }
        Err(errno) => {
            log_trace!("!-> {:?}", errno);
            current_task.registers.set_return_register(errno.return_value());
            Some(ErrorContext { error: errno, syscall })
        }
    }
}

/// Finishes `current_task` updates after a restricted mode exit such as a syscall, exception, or kick.
///
/// Returns an `ExitStatus` if the task is meant to exit.
pub fn process_completed_restricted_exit(
    current_task: &mut CurrentTask,
    error_context: &Option<ErrorContext>,
) -> Result<Option<ExitStatus>, Errno> {
    // Checking for a signal might cause the task to exit, so check before processing exit
    if current_task.read().exit_status.is_none() {
        dequeue_signal(current_task);
    }

    if let Some(exit_status) = current_task.read().exit_status.as_ref() {
        log_trace!("exiting with status {:?}", exit_status);
        if let Some(error_context) = error_context {
            match exit_status {
                ExitStatus::Exit(value) if *value == 0 => {}
                _ => {
                    log_trace!(
                        "last failing syscall before exit: {:?}, failed with {:?}",
                        error_context.syscall,
                        error_context.error
                    );
                }
            };
        }
        return Ok(Some(exit_status.clone()));
    }

    // Block a stopped process after it's had a chance to handle signals, since a signal might
    // cause it to stop.
    block_while_stopped(current_task);

    Ok(None)
}

/// Creates a `StartupHandles` from the provided handles.
///
/// The `numbered_handles` of type `HandleType::FileDescriptor` are used to
/// create files, and the handles are required to be of type `zx::Socket`.
///
/// If there is a `numbered_handles` of type `HandleType::User0`, that is
/// interpreted as the server end of the ShellController protocol.
pub fn parse_numbered_handles(
    current_task: &CurrentTask,
    numbered_handles: Option<Vec<fprocess::HandleInfo>>,
    files: &FdTable,
) -> Result<(), Error> {
    if let Some(numbered_handles) = numbered_handles {
        for numbered_handle in numbered_handles {
            let info = HandleInfo::try_from(numbered_handle.id)?;
            if info.handle_type() == HandleType::FileDescriptor {
                files.insert(
                    current_task,
                    FdNumber::from_raw(info.arg().into()),
                    create_file_from_handle(current_task, numbered_handle.handle)?,
                )?;
            }
        }
    }

    let stdio = SyslogFile::new_file(current_task);
    // If no numbered handle is provided for each stdio handle, default to syslog.
    for i in [0, 1, 2] {
        if files.get(FdNumber::from_raw(i)).is_err() {
            files.insert(current_task, FdNumber::from_raw(i), stdio.clone())?;
        }
    }

    Ok(())
}

/// Create a filesystem to access the content of the fuchsia directory available at `fs_src` inside
/// `pkg`.
pub fn create_remotefs_filesystem(
    kernel: &Arc<Kernel>,
    root: &fio::DirectorySynchronousProxy,
    rights: fio::OpenFlags,
    fs_src: &str,
) -> Result<FileSystemHandle, Error> {
    let root = syncio::directory_open_directory_async(root, fs_src, rights)
        .map_err(|e| anyhow!("Failed to open root: {}", e))?;
    RemoteFs::new_fs(kernel, root.into_channel(), fs_src, rights).map_err(|e| e.into())
}

pub fn create_filesystem_from_spec<'a>(
    kernel: &Arc<Kernel>,
    pkg: &fio::DirectorySynchronousProxy,
    spec: &'a str,
) -> Result<(&'a [u8], FileSystemHandle), Error> {
    let mut iter = spec.splitn(4, ':');
    let mount_point =
        iter.next().ok_or_else(|| anyhow!("mount point is missing from {:?}", spec))?;
    let fs_type = iter.next().ok_or_else(|| anyhow!("fs type is missing from {:?}", spec))?;
    let fs_src = match iter.next() {
        Some(src) if !src.is_empty() => src,
        _ => ".",
    };
    let params = iter.next().unwrap_or("");

    let options = FileSystemOptions {
        source: fs_src.as_bytes().to_vec(),
        flags: MountFlags::empty(),
        params: params.as_bytes().to_vec(),
    };

    // Default rights for remotefs.
    let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;

    // The filesystem types handled in this match are the ones that can only be specified in a
    // manifest file, for whatever reason. Anything else is passed to create_filesystem, which is
    // common code that also handles the mount() system call.
    let fs = match fs_type {
        "remote_bundle" => RemoteBundle::new_fs(kernel, pkg, rights, fs_src)?,
        "remotefs" => create_remotefs_filesystem(kernel, pkg, rights, fs_src)?,
        _ => kernel.create_filesystem(fs_type.as_bytes(), options)?,
    };
    Ok((mount_point.as_bytes(), fs))
}

/// Block the execution of `current_task` as long as the task is stopped and not terminated.
pub fn block_while_stopped(current_task: &CurrentTask) {
    // Early exit test to avoid creating a port when we don't need to sleep. Testing in the loop
    // after adding the waiter to the wait queue is still important to deal with race conditions
    // where the condition becomes true between checking it and starting the wait.
    // TODO(tbodt): Find a less hacky way to do this. There might be some way to create one port
    // per task and use it every time the current task needs to sleep.
    if current_task.read().exit_status.is_some() {
        return;
    }
    if !current_task.thread_group.read().stopped {
        return;
    }

    let waiter = Waiter::new_ignoring_signals();
    loop {
        current_task.thread_group.read().stopped_waiters.wait_async(&waiter);
        if current_task.read().exit_status.is_some() {
            return;
        }
        if !current_task.thread_group.read().stopped {
            return;
        }
        // Result is not needed, as this is not in a syscall.
        let _: Result<(), Errno> = waiter.wait(current_task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{signals::*, testing::*};

    #[::fuchsia::test]
    async fn test_block_while_stopped_stop_and_continue() {
        let (_kernel, task) = create_kernel_and_task();

        // block_while_stopped must immediately returned if the task is not stopped.
        block_while_stopped(&task);

        // Stop the task.
        task.thread_group.set_stopped(true, SignalInfo::default(SIGSTOP));

        let cloned_task = task.task_arc_clone();
        let thread = std::thread::spawn(move || {
            // Wait for the task to have a waiter.
            while !cloned_task.read().signals.waiter.is_valid() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            // Continue the task.
            cloned_task.thread_group.set_stopped(false, SignalInfo::default(SIGCONT));
        });

        // Block until continued.
        block_while_stopped(&task);

        // Join the thread, which will ensure set_stopped terminated.
        thread.join().expect("joined");

        // The task should not be blocked anymore.
        block_while_stopped(&task);
    }

    #[::fuchsia::test]
    async fn test_block_while_stopped_stop_and_exit() {
        let (_kernel, task) = create_kernel_and_task();

        // block_while_stopped must immediately returned if the task is neither stopped nor exited.
        block_while_stopped(&task);

        // Stop the task.
        task.thread_group.set_stopped(true, SignalInfo::default(SIGSTOP));

        let cloned_task = task.task_arc_clone();
        let thread = std::thread::spawn(move || {
            // Wait for the task to have a waiter.
            while !cloned_task.read().signals.waiter.is_valid() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            // exit the task.
            cloned_task.thread_group.exit(ExitStatus::Exit(1));
        });

        // Block until continued.
        block_while_stopped(&task);

        // Join the task, which will ensure thread_group.exit terminated.
        thread.join().expect("joined");

        // The task should not be blocked because it is stopped.
        block_while_stopped(&task);
    }
}
