// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fprocess;
#[cfg(feature = "syscall_stats")]
use fuchsia_inspect::NumericProperty;
use fuchsia_runtime::{HandleInfo, HandleType};
use fuchsia_zircon::{self as zx, AsHandleRef};
use process_builder::elf_parse;
use std::convert::TryFrom;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::arch::execution::generate_interrupt_instructions;
use crate::fs::fuchsia::{create_file_from_handle, RemoteBundle, RemoteFs, SyslogFile};
use crate::fs::*;
use crate::logging::log_trace;
use crate::mm::{DesiredAddress, MappingOptions, PAGE_SIZE};
use crate::mm::{MappingName, MemoryAccessorExt, MemoryManager, ProtectionFlags};
use crate::signals::dequeue_signal;
use crate::syscalls::{
    decls::{Syscall, SyscallDecl},
    table::dispatch_syscall,
};
use crate::task::*;
use crate::types::*;
use crate::vmex_resource::VMEX_RESOURCE;

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

    // Inlined fast path for seccomp, so that we don't incur the cost
    // of a method call when running the filters.
    if current_task.has_seccomp_filters.load(Ordering::Acquire) != SeccompFilterState::None as u8 {
        if let Some(errno) = current_task.run_seccomp_filters(&syscall) {
            current_task.registers.set_return_register(errno.return_value());
            return Some(ErrorContext { error: errno, syscall });
        }
    }

    log_trace!("{:?}", syscall);
    match dispatch_syscall(current_task, &syscall) {
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

/// Finishes `current_task` updates after system call dispatch.
///
/// Returns an `ExitStatus` if the task is meant to exit.
pub fn process_completed_syscall(
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

    // Handle the debug address after the thread is set up to continue, because
    // `notify_debugger_of_module_list` expects the register state to be in a post-syscall state
    // (most importantly the instruction pointer needs to be "correct").
    notify_debugger_of_module_list(current_task)?;

    Ok(None)
}

/// Notifies the debugger, if one is attached, that the initial module list has been loaded.
///
/// Sets the process property if a valid address is found in the `DT_DEBUG` entry. If the existing
/// value of ZX_PROP_BREAK_ON_LOAD is non-zero, the task will be set up to trigger a software
/// interrupt (for the debugger to catch) before resuming execution at the current instruction
/// pointer.
///
/// If the property is set on the process (i.e., nothing fails and the values are valid),
/// `current_task.debug_address` will be cleared.
///
/// TODO(fxbug.dev/117484): Starnix should issue a debug breakpoint on each dlopen/dlclose that
/// causes the module list to change.
///
/// For more information about the debugger protocol, see:
/// https://cs.opensource.google/fuchsia/fuchsia/+/master:src/developer/debug/debug_agent/process_handle.h;l=31
///
/// # Parameters:
/// - `current_task`: The task to set the property for. The register's of this task, the instruction
///                   pointer specifically, needs to be set to the value with which the task is
///                   expected to resume.
pub fn notify_debugger_of_module_list(current_task: &mut CurrentTask) -> Result<(), Errno> {
    let dt_debug_address = match current_task.dt_debug_address {
        Some(dt_debug_address) => dt_debug_address,
        // The DT_DEBUG entry does not exist, or has already been read and set on the process.
        None => return Ok(()),
    };

    // The debug_addres is the pointer located at DT_DEBUG.
    let debug_address: elf_parse::Elf64Dyn =
        current_task.mm.read_object(UserRef::new(dt_debug_address))?;
    if debug_address.value == 0 {
        // The DT_DEBUG entry is present, but has not yet been set by the linker, check next time.
        return Ok(());
    }

    let break_on_load = current_task
        .thread_group
        .process
        .get_break_on_load()
        .map_err(|err| from_status_like_fdio!(err))?;

    // If break on load is 0, there is no debugger attached, so return before issuing the software
    // breakpoint.
    if break_on_load == 0 {
        // Still set the debug address, and clear the debug address from `current_task` to avoid
        // entering this function again.
        match current_task.thread_group.process.set_debug_addr(&debug_address.value) {
            Err(zx::Status::ACCESS_DENIED) => {}
            status => status.map_err(|err| from_status_like_fdio!(err))?,
        };
        current_task.dt_debug_address = None;
        return Ok(());
    }

    // An executable VMO is mapped into the process, which does two things:
    //   1. Issues a software interrupt caught by the debugger.
    //   2. Jumps back to the current instruction pointer of the thread executed by an indirect
    //      jump to the 64-bit address immediately following the jump instruction.
    let instructions = generate_interrupt_instructions(current_task);
    let vmo = Arc::new(
        zx::Vmo::create(*PAGE_SIZE)
            .and_then(|vmo| vmo.replace_as_executable(&VMEX_RESOURCE))
            .map_err(|err| from_status_like_fdio!(err))?,
    );
    vmo.write(&instructions, 0).map_err(|e| from_status_like_fdio!(e))?;

    let prot_flags = ProtectionFlags::EXEC | ProtectionFlags::READ;
    let instruction_pointer = current_task.mm.map(
        DesiredAddress::Hint(UserAddress::default()),
        vmo,
        0,
        instructions.len(),
        prot_flags,
        prot_flags.to_vmar_flags(),
        MappingOptions::empty(),
        MappingName::None,
    )?;

    // Set the break on load value to point to the software breakpoint. This is how the debugger
    // distinguishes the breakpoint from other software breakpoints.
    current_task
        .thread_group
        .process
        .set_break_on_load(&(instruction_pointer.ptr() as u64))
        .map_err(|err| from_status_like_fdio!(err))?;

    current_task.registers.set_instruction_pointer_register(instruction_pointer.ptr() as u64);
    current_task
        .thread_group
        .process
        .set_debug_addr(&debug_address.value)
        .map_err(|err| from_status_like_fdio!(err))?;
    current_task.dt_debug_address = None;

    Ok(())
}

pub fn copy_process_debug_addr(
    source_process: &zx::Process,
    target_process: &zx::Process,
) -> Result<(), Errno> {
    let source_debug_addr =
        source_process.get_debug_addr().map_err(|err| from_status_like_fdio!(err))?;

    match target_process.set_debug_addr(&source_debug_addr) {
        Err(zx::Status::ACCESS_DENIED) => {}
        status => status.map_err(|err| from_status_like_fdio!(err))?,
    };
    Ok(())
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
    kernel: &Kernel,
    root: &fio::DirectorySynchronousProxy,
    rights: fio::OpenFlags,
    fs_src: &str,
) -> Result<FileSystemHandle, Error> {
    let root = syncio::directory_open_directory_async(root, fs_src, rights)
        .map_err(|e| anyhow!("Failed to open root: {}", e))?;
    RemoteFs::new_fs(kernel, root.into_channel(), rights).map_err(|e| e.into())
}

pub fn create_filesystem_from_spec<'a>(
    task: &CurrentTask,
    pkg: &fio::DirectorySynchronousProxy,
    spec: &'a str,
) -> Result<(&'a [u8], WhatToMount), Error> {
    use WhatToMount::*;
    let mut iter = spec.splitn(3, ':');
    let mount_point =
        iter.next().ok_or_else(|| anyhow!("mount point is missing from {:?}", spec))?;
    let fs_type = iter.next().ok_or_else(|| anyhow!("fs type is missing from {:?}", spec))?;
    let fs_src = iter.next().unwrap_or(".");

    // Default rights for remotefs.
    let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;

    // The filesystem types handled in this match are the ones that can only be specified in a
    // manifest file, for whatever reason. Anything else is passed to create_filesystem, which is
    // common code that also handles the mount() system call.
    let fs = match fs_type {
        "bind" => Bind(task.lookup_path_from_root(fs_src.as_bytes())?),
        "remote_bundle" => Fs(RemoteBundle::new_fs(task.kernel(), pkg, rights, fs_src)?),
        "remotefs" => Fs(create_remotefs_filesystem(task.kernel(), pkg, rights, fs_src)?),
        _ => create_filesystem(task, fs_src.as_bytes(), fs_type.as_bytes(), b"")?,
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
    use crate::signals::*;
    use crate::testing::*;

    #[::fuchsia::test]
    fn test_block_while_stopped_stop_and_continue() {
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
    fn test_block_while_stopped_stop_and_exit() {
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

/// Reads from `chan` into `buf`.
///
/// If the initial read returns `SHOULD_WAIT`, the function waits for the channel to be readable and
/// tries again (once).
pub fn read_channel_sync(chan: &zx::Channel, buf: &mut zx::MessageBuf) -> Result<(), zx::Status> {
    let res = chan.read(buf);
    if let Err(zx::Status::SHOULD_WAIT) = res {
        chan.wait_handle(
            zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
            zx::Time::INFINITE,
        )?;
        chan.read(buf)
    } else {
        res
    }
}

/// Converts a `zx::MessageBuf` into an exception info by transmuting a copy of the bytes.
// TODO: Should we move this code into fuchsia_zircon? It seems like part of a better abstraction
// for exception channels.
pub fn as_exception_info(buffer: &zx::MessageBuf) -> zx::sys::zx_exception_info_t {
    let mut tmp = [0; std::mem::size_of::<zx::sys::zx_exception_info_t>()];
    tmp.clone_from_slice(buffer.bytes());
    unsafe { std::mem::transmute(tmp) }
}
