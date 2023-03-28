// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{Lifecycle, Program, Start, Stop},
    async_trait::async_trait,
    fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, Task},
    process_builder::ProcessBuilderError,
    std::ffi::CString,
};

/// Parameters for creating a `zx::Process`.
pub struct CreateParams {
    /// The name to assign to the created process.
    pub name: CString,

    /// The job in which to create the process.
    pub job: zx::Job,

    /// Additional flags.
    ///
    /// See <https://fuchsia.dev/fuchsia-src/reference/syscalls/process_create> for details.
    pub options: zx::ProcessOptions,
}

/// Waits for the process to terminate and returns its return code.
pub async fn wait_for_process_terminated(process: &zx::Process) -> Result<i64, zx::Status> {
    fasync::OnSignals::new(process, zx::Signals::PROCESS_TERMINATED)
        .await
        .map(|_: fidl::Signals| ())?;
    let zx::ProcessInfo { return_code, .. } = process.info()?;
    Ok(return_code)
}

/// A container for a created, but not yet started process.
///
/// This implements the `Start` trait for [process_builder::BuiltProcess].
pub struct BuiltProcess(pub process_builder::BuiltProcess);

#[async_trait]
impl Start for BuiltProcess {
    type Error = zx::Status;
    type Stop = Process;

    /// Starts the `BuiltProcess`.
    ///
    /// Returns a `exec::Process` that represents the running process.
    ///
    /// # Errors
    ///
    /// Returns a `zx::Status` value if the process fails to start. See [zx_process_start]
    /// for possible status values and their meaning.
    ///
    /// [zx_process_start]: https://fuchsia.dev/fuchsia-src/reference/syscalls/process_start
    async fn start(self) -> Result<Self::Stop, Self::Error> {
        let process = self.0.start().map_err(|err| match err {
            ProcessBuilderError::ProcessStart(status) => status,
            _ => zx::Status::INTERNAL,
        })?;
        Ok(Process(process))
    }
}

/// A wrapper for a running Zircon process.
///
/// This implements the `Stop` and `Program` traits for processes.
pub struct Process(pub zx::Process);

#[async_trait]
impl Stop for Process {
    type Error = zx::Status;

    /// Stops the `Process`.
    ///
    /// This kills the Zircon task.
    ///
    /// # Errors
    ///
    /// Returns a `zx::Status` value if the process cannot be killed. See [zx_task_kill]
    /// for possible status values and their meaning.
    ///
    /// [zx_task_kill]: https://fuchsia.dev/fuchsia-src/reference/syscalls/task_kill
    async fn stop(&mut self) -> Result<(), Self::Error> {
        self.0.kill()
    }
}

#[async_trait]
impl Lifecycle for Process {
    type Exit = i64;

    /// Returns the exit code once the process is terminated.
    ///
    /// This returns once the underlying Zircon process is terminated. This can happen either
    /// if the process exits normally, the process is stopped via `Stop`, or the process
    /// is otherwise killed by some other means.
    ///
    /// If the process is killed, the return code is
    /// [fuchsia_zircon_types::ZX_TASK_RETCODE_SYSCALL_KILL].
    ///
    /// # Errors
    ///
    /// Returns a `zx::Status` value if waiting on the `PROCESS_TERMINATED` signal or retrieving
    /// the process info fails. See [zx_object_wait_async] and [zx_object_get_info] for possible
    /// status values and their meaning.
    ///
    /// [zx_object_wait_async]: https://fuchsia.dev/fuchsia-src/reference/syscalls/object_wait_async
    /// [zx_object_get_info]: https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info
    async fn on_exit(&self) -> Result<i64, zx::Status> {
        wait_for_process_terminated(&self.0).await
    }
}

impl Program for Process {}
