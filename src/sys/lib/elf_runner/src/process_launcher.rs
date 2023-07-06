// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::vdso_vmo::get_stable_vdso_vmo,
    anyhow::Context,
    fidl_connector::Connect,
    fidl_fuchsia_process as fproc, fuchsia_async as fasync,
    fuchsia_runtime::{HandleInfo, HandleInfoError},
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::prelude::*,
    process_builder::{
        BuiltProcess, NamespaceEntry, ProcessBuilder, ProcessBuilderError, StartupHandle,
    },
    std::{convert::TryFrom, ffi::CString, sync::Arc},
    thiserror::Error,
    tracing::{error, warn},
};

/// Internal error type for ProcessLauncher which conveniently wraps errors that might
/// result during process launching and allows for mapping them to an equivalent zx::Status, which
/// is what actually gets returned through the protocol.
#[derive(Error, Debug)]
enum LauncherError {
    #[error("Invalid arg: {}", _0)]
    InvalidArg(&'static str),
    #[error("Failed to build new process: {}", _0)]
    BuilderError(ProcessBuilderError),
    #[error("Invalid handle info: {}", _0)]
    HandleInfoError(HandleInfoError),
}

impl LauncherError {
    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            LauncherError::InvalidArg(_) => zx::Status::INVALID_ARGS,
            LauncherError::BuilderError(e) => e.as_zx_status(),
            LauncherError::HandleInfoError(_) => zx::Status::INVALID_ARGS,
        }
    }
}

impl From<ProcessBuilderError> for LauncherError {
    fn from(err: ProcessBuilderError) -> Self {
        LauncherError::BuilderError(err)
    }
}

impl From<HandleInfoError> for LauncherError {
    fn from(err: HandleInfoError) -> Self {
        LauncherError::HandleInfoError(err)
    }
}

#[derive(Default, Debug)]
struct ProcessLauncherState {
    args: Vec<Vec<u8>>,
    environ: Vec<Vec<u8>>,
    name_info: Vec<fproc::NameInfo>,
    handles: Vec<fproc::HandleInfo>,
    options: zx::ProcessOptions,
}

/// Similar to fproc::LaunchInfo, but with the job wrapped in an Arc (to enable use after the struct
/// is moved).
#[derive(Debug)]
struct LaunchInfo {
    executable: zx::Vmo,
    job: Arc<zx::Job>,
    name: String,
}

impl From<fproc::LaunchInfo> for LaunchInfo {
    fn from(info: fproc::LaunchInfo) -> Self {
        LaunchInfo { executable: info.executable, job: Arc::new(info.job), name: info.name }
    }
}

/// An implementation of the `fuchsia.process.Launcher` protocol using the `process_builder` crate.
pub struct ProcessLauncher;

impl ProcessLauncher {
    /// Serves an instance of the `fuchsia.process.Launcher` protocol given an appropriate
    /// RequestStream. Returns when the channel backing the RequestStream is closed or an
    /// unrecoverable error, like a failure to read from the stream, occurs.
    pub async fn serve(mut stream: fproc::LauncherRequestStream) -> Result<(), fidl::Error> {
        // `fuchsia.process.Launcher is stateful. The Add methods accumulate state that is
        // consumed/reset by either Launch or CreateWithoutStarting.
        let mut state = ProcessLauncherState::default();

        while let Some(req) = stream.try_next().await? {
            match req {
                fproc::LauncherRequest::Launch { info, responder } => {
                    let info = LaunchInfo::from(info);
                    let job = info.job.clone();
                    let name = info.name.clone();

                    match Self::launch_process(info, state).await {
                        Ok(process) => {
                            responder.send(zx::Status::OK.into_raw(), Some(process))?;
                        }
                        Err(err) => {
                            Self::log_launcher_error(&err, "launch", job, name);
                            responder.send(err.as_zx_status().into_raw(), None)?;
                        }
                    }

                    // Reset state to defaults.
                    state = ProcessLauncherState::default();
                }
                fproc::LauncherRequest::CreateWithoutStarting { info, responder } => {
                    let info = LaunchInfo::from(info);
                    let job = info.job.clone();
                    let name = info.name.clone();

                    match Self::create_process(info, state).await {
                        Ok(built) => {
                            let process_data = fproc::ProcessStartData {
                                process: built.process,
                                root_vmar: built.root_vmar,
                                thread: built.thread,
                                entry: built.entry as u64,
                                stack: built.stack as u64,
                                bootstrap: built.bootstrap,
                                vdso_base: built.vdso_base as u64,
                                base: built.elf_base as u64,
                            };
                            responder.send(zx::Status::OK.into_raw(), Some(process_data))?;
                        }
                        Err(err) => {
                            Self::log_launcher_error(&err, "create", job, name);
                            responder.send(err.as_zx_status().into_raw(), None)?;
                        }
                    }

                    // Reset state to defaults.
                    state = ProcessLauncherState::default();
                }
                fproc::LauncherRequest::AddArgs { mut args, control_handle: _ } => {
                    state.args.append(&mut args);
                }
                fproc::LauncherRequest::AddEnvirons { mut environ, control_handle: _ } => {
                    state.environ.append(&mut environ);
                }
                fproc::LauncherRequest::AddNames { mut names, control_handle: _ } => {
                    state.name_info.append(&mut names);
                }
                fproc::LauncherRequest::AddHandles { mut handles, control_handle: _ } => {
                    state.handles.append(&mut handles);
                }
                fproc::LauncherRequest::SetOptions { options, .. } => {
                    // SAFETY: These options are passed directly to `zx_process_create`, which
                    // will determine whether or not the options are valid.
                    state.options = unsafe { zx::ProcessOptions::from_bits_unchecked(options) };
                }
            }
        }
        Ok(())
    }

    fn log_launcher_error(err: &LauncherError, op: &str, job: Arc<zx::Job>, name: String) {
        enum LogLevel {
            Warn,
            Error,
        }
        let mut log_level = LogLevel::Warn;

        // Special case BAD_STATE errors to check the job's exited status and log more info.
        // BAD_STATE errors are expected if the job has exited for reasons outside our control, like
        // another process killing the job or the parent job exiting, while a new process is being
        // created in it.
        let mut job_message = format!("");
        match err {
            LauncherError::BuilderError(err) if err.as_zx_status() == zx::Status::BAD_STATE => {
                match job.info() {
                    Ok(job_info) => {
                        if job_info.exited {
                            job_message =
                                format!(", job has exited (retcode {})", job_info.return_code);
                        } else if job_info.return_code != 0 {
                            job_message =
                                format!(", job is exiting (retcode {})", job_info.return_code);
                        } else {
                            // If the job has not exited, then the BAD_STATE error was unexpected
                            // and indicates a bug somewhere.
                            log_level = LogLevel::Error;
                            job_message = format!(", job has not exited (retcode 0)");
                        }
                    }
                    Err(status) => {
                        log_level = LogLevel::Error;
                        job_message = format!(" (error {} getting job state)", status);
                    }
                }
            }
            _ => {}
        }

        let job_koid =
            job.get_koid().map(|j| j.raw_koid().to_string()).unwrap_or("<unknown>".to_string());

        // Repeat ourselves slightly here because tracing does not support runtime levels in macros.
        match log_level {
            LogLevel::Warn => warn!(
                %op, process_name=%name, %job_koid, %job_message, error=%err,
                "Process operation failed",
            ),
            LogLevel::Error => error!(
                %op, process_name=%name, %job_koid, %job_message, error=%err,
                "Process operation failed",
            ),
        }
    }

    async fn launch_process(
        info: LaunchInfo,
        state: ProcessLauncherState,
    ) -> Result<zx::Process, LauncherError> {
        Ok(Self::create_process(info, state).await?.start()?)
    }

    async fn create_process(
        info: LaunchInfo,
        state: ProcessLauncherState,
    ) -> Result<BuiltProcess, LauncherError> {
        Ok(Self::create_process_builder(info, state)?.build().await?)
    }

    fn create_process_builder(
        info: LaunchInfo,
        state: ProcessLauncherState,
    ) -> Result<ProcessBuilder, LauncherError> {
        let proc_name = CString::new(info.name)
            .map_err(|_| LauncherError::InvalidArg("Process name contained null byte"))?;
        let stable_vdso_vmo = get_stable_vdso_vmo().map_err(|_| {
            LauncherError::BuilderError(ProcessBuilderError::BadHandle("Failed to get stable vDSO"))
        })?;
        let mut b = ProcessBuilder::new(
            &proc_name,
            &info.job,
            state.options,
            info.executable,
            stable_vdso_vmo,
        )?;

        let arg_cstr = state
            .args
            .into_iter()
            .map(|a| CString::new(a))
            .collect::<Result<_, _>>()
            .map_err(|_| LauncherError::InvalidArg("Argument contained null byte"))?;
        b.add_arguments(arg_cstr);

        let env_cstr = state
            .environ
            .into_iter()
            .map(|e| CString::new(e))
            .collect::<Result<_, _>>()
            .map_err(|_| LauncherError::InvalidArg("Environment string contained null byte"))?;
        b.add_environment_variables(env_cstr);

        let entries = state
            .name_info
            .into_iter()
            .map(|n| Self::new_namespace_entry(n))
            .collect::<Result<_, _>>()?;
        b.add_namespace_entries(entries)?;

        // Note that clients of ``fuchsia.process.Launcher` provide the `fuchsia.ldsvc.Loader`
        // through AddHandles, with a handle type of [HandleType::LdsvcLoader].
        // [ProcessBuilder::add_handles] automatically handles that for convenience.
        let handles = state
            .handles
            .into_iter()
            .map(|h| Self::new_startup_handle(h))
            .collect::<Result<_, _>>()?;
        b.add_handles(handles)?;

        Ok(b)
    }

    // Can't impl TryFrom for these because both types are from external crates. :(
    // Could wrap in a newtype, but then have to unwrap, so this is simplest.
    fn new_namespace_entry(info: fproc::NameInfo) -> Result<NamespaceEntry, LauncherError> {
        let cstr = CString::new(info.path)
            .map_err(|_| LauncherError::InvalidArg("Namespace path contained null byte"))?;
        Ok(NamespaceEntry { path: cstr, directory: info.directory })
    }

    fn new_startup_handle(info: fproc::HandleInfo) -> Result<StartupHandle, LauncherError> {
        Ok(StartupHandle { handle: info.handle, info: HandleInfo::try_from(info.id)? })
    }
}

pub type Connector = Box<dyn Connect<Proxy = fproc::LauncherProxy> + Send + Sync>;

/// A protocol connector for `fuchsia.process.Launcher` that serves the protocol with the
/// `ProcessLauncher` implementation.
pub struct BuiltInConnector {}

impl Connect for BuiltInConnector {
    type Proxy = fproc::LauncherProxy;

    fn connect(&self) -> Result<Self::Proxy, anyhow::Error> {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fproc::LauncherMarker>()?;
        fasync::Task::spawn(async move {
            let result = ProcessLauncher::serve(stream).await;
            if let Err(error) = result {
                warn!(%error, "ProcessLauncher.serve failed");
            }
        })
        .detach();
        Ok(proxy)
    }
}

/// A protocol connector for `fuchsia.process.Launcher` that connects to the protocol from the
/// process namespace.
pub struct NamespaceConnector {}

impl Connect for NamespaceConnector {
    type Proxy = fproc::LauncherProxy;

    fn connect(&self) -> Result<Self::Proxy, anyhow::Error> {
        fuchsia_component::client::connect_to_protocol::<fproc::LauncherMarker>()
            .context("failed to connect to external launcher service")
    }
}
