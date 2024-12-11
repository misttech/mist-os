// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod component;
mod component_set;
mod config;
mod crash_handler;
pub mod crash_info;
mod error;
mod memory;
pub mod process_launcher;
mod runtime_dir;
mod stdout;
pub mod vdso_vmo;

use self::component::{ElfComponent, ElfComponentInfo};
use self::config::ElfProgramConfig;
use self::error::{JobError, StartComponentError, StartInfoError};
use self::runtime_dir::RuntimeDirBuilder;
use self::stdout::bind_streams_to_syslog;
use crate::component_set::ComponentSet;
use crate::crash_info::CrashRecords;
use crate::memory::reporter::MemoryReporter;
use crate::vdso_vmo::get_next_vdso_vmo;
use ::routing::policy::ScopedPolicyChecker;
use chrono::{NaiveDateTime, TimeZone as _, Utc};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_diagnostics_types::{
    ComponentDiagnostics, ComponentTasks, Task as DiagnosticsTask,
};
use fidl_fuchsia_process_lifecycle::LifecycleMarker;
use fuchsia_async::{self as fasync, TimeoutExt};
use fuchsia_runtime::{duplicate_utc_clock_handle, job_default, HandleInfo, HandleType, UtcClock};
use futures::channel::oneshot;
use futures::TryStreamExt;
use moniker::Moniker;
use runner::component::StopInfo;
use runner::StartInfo;
use std::path::Path;
use std::sync::Arc;
use tracing::warn;
use zx::{self as zx, AsHandleRef, HandleBased};
use {
    fidl_fuchsia_component as fcomp, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_io as fio, fidl_fuchsia_memory_attribution as fattribution,
    fidl_fuchsia_process as fproc,
};

// Maximum time that the runner will wait for break_on_start eventpair to signal.
// This is set to prevent debuggers from blocking us for too long, either intentionally
// or unintentionally.
const MAX_WAIT_BREAK_ON_START: zx::MonotonicDuration = zx::MonotonicDuration::from_millis(300);

// Minimum timer slack amount and default mode. The amount should be large enough to allow for some
// coalescing of timers, but small enough to ensure applications don't miss deadlines.
//
// TODO(https://fxbug.dev/42120293): For now, set the value to 50us to avoid delaying performance-critical
// timers in Scenic and other system services.
const TIMER_SLACK_DURATION: zx::MonotonicDuration = zx::MonotonicDuration::from_micros(50);

// Rights used when duplicating the UTC clock handle.
//
// The bitwise `|` operator for `bitflags` is implemented through the `std::ops::BitOr` trait,
// which cannot be used in a const context. The workaround is to bitwise OR the raw bits.
const DUPLICATE_CLOCK_RIGHTS: zx::Rights = zx::Rights::from_bits_truncate(
    zx::Rights::READ.bits()
        | zx::Rights::WAIT.bits()
        | zx::Rights::DUPLICATE.bits()
        | zx::Rights::TRANSFER.bits(),
);

// Builds and serves the runtime directory
/// Runs components with ELF binaries.
pub struct ElfRunner {
    /// Each ELF component run by this runner will live inside a job that is a
    /// child of this job.
    job: zx::Job,

    launcher_connector: process_launcher::Connector,

    /// If `utc_clock` is populated then that Clock's handle will
    /// be passed into the newly created process. Otherwise, the UTC
    /// clock will be duplicated from current process' process table.
    /// The latter is typically the case in unit tests and nested
    /// component managers.
    utc_clock: Option<Arc<UtcClock>>,

    crash_records: CrashRecords,

    /// Tracks the ELF components that are currently running under this runner.
    components: Arc<ComponentSet>,

    /// Tracks reporting memory changes to an observer.
    memory_reporter: MemoryReporter,
}

/// The job for a component.
pub enum Job {
    Single(zx::Job),
    Multiple { parent: zx::Job, child: zx::Job },
}

impl Job {
    fn top(&self) -> &zx::Job {
        match self {
            Job::Single(job) => job,
            Job::Multiple { parent, child: _ } => parent,
        }
    }

    fn proc(&self) -> &zx::Job {
        match self {
            Job::Single(job) => job,
            Job::Multiple { parent: _, child } => child,
        }
    }
}

impl ElfRunner {
    pub fn new(
        job: zx::Job,
        launcher_connector: process_launcher::Connector,
        utc_clock: Option<Arc<UtcClock>>,
        crash_records: CrashRecords,
    ) -> ElfRunner {
        let components = ComponentSet::new();
        let memory_reporter = MemoryReporter::new(components.clone());
        ElfRunner { job, launcher_connector, utc_clock, crash_records, components, memory_reporter }
    }

    /// Returns a UTC clock handle.
    ///
    /// Duplicates `self.utc_clock` if populated, or the UTC clock assigned to the current process.
    async fn duplicate_utc_clock(&self) -> Result<UtcClock, zx::Status> {
        if let Some(utc_clock) = &self.utc_clock {
            utc_clock.duplicate_handle(DUPLICATE_CLOCK_RIGHTS)
        } else {
            duplicate_utc_clock_handle(DUPLICATE_CLOCK_RIGHTS)
        }
    }

    /// Creates a job for a component.
    fn create_job(&self, program_config: &ElfProgramConfig) -> Result<Job, JobError> {
        let job = self.job.create_child_job().map_err(JobError::CreateChild)?;

        // Set timer slack.
        //
        // Why Late and not Center or Early? Timers firing a little later than requested is not
        // uncommon in non-realtime systems. Programs are generally tolerant of some
        // delays. However, timers firing before their deadline can be unexpected and lead to bugs.
        job.set_policy(zx::JobPolicy::TimerSlack(
            TIMER_SLACK_DURATION,
            zx::JobDefaultTimerMode::Late,
        ))
        .map_err(JobError::SetPolicy)?;

        // Prevent direct creation of processes.
        //
        // The kernel-level mechanisms for creating processes are very low-level. We require that
        // all processes be created via fuchsia.process.Launcher in order for the platform to
        // maintain change-control over how processes are created.
        if !program_config.create_raw_processes {
            job.set_policy(zx::JobPolicy::Basic(
                zx::JobPolicyOption::Absolute,
                vec![(zx::JobCondition::NewProcess, zx::JobAction::Deny)],
            ))
            .map_err(JobError::SetPolicy)?;
        }

        // Default deny the job policy which allows ambiently marking VMOs executable, i.e. calling
        // vmo_replace_as_executable without an appropriate resource handle.
        if !program_config.ambient_mark_vmo_exec {
            job.set_policy(zx::JobPolicy::Basic(
                zx::JobPolicyOption::Absolute,
                vec![(zx::JobCondition::AmbientMarkVmoExec, zx::JobAction::Deny)],
            ))
            .map_err(JobError::SetPolicy)?;
        }

        Ok(if program_config.job_with_available_exception_channel {
            // Create a new job to hold the process because the component wants
            // the process to be a direct child of a job that has its exception
            // channel available for taking. Note that we (the ELF runner) uses
            // a job's exception channel for crash recording so we create a new
            // job underneath the original job to hold the process.
            let child = job.create_child_job().map_err(JobError::CreateChild)?;
            Job::Multiple { parent: job, child }
        } else {
            Job::Single(job)
        })
    }

    fn create_handle_infos(
        outgoing_dir: Option<zx::Channel>,
        lifecycle_server: Option<zx::Channel>,
        utc_clock: UtcClock,
        next_vdso: Option<zx::Vmo>,
        config_vmo: Option<zx::Vmo>,
    ) -> Vec<fproc::HandleInfo> {
        let mut handle_infos = vec![];

        if let Some(outgoing_dir) = outgoing_dir {
            handle_infos.push(fproc::HandleInfo {
                handle: outgoing_dir.into_handle(),
                id: HandleInfo::new(HandleType::DirectoryRequest, 0).as_raw(),
            });
        }

        if let Some(lifecycle_chan) = lifecycle_server {
            handle_infos.push(fproc::HandleInfo {
                handle: lifecycle_chan.into_handle(),
                id: HandleInfo::new(HandleType::Lifecycle, 0).as_raw(),
            })
        };

        handle_infos.push(fproc::HandleInfo {
            handle: utc_clock.into_handle(),
            id: HandleInfo::new(HandleType::ClockUtc, 0).as_raw(),
        });

        if let Some(next_vdso) = next_vdso {
            handle_infos.push(fproc::HandleInfo {
                handle: next_vdso.into_handle(),
                id: HandleInfo::new(HandleType::VdsoVmo, 0).as_raw(),
            });
        }

        if let Some(config_vmo) = config_vmo {
            handle_infos.push(fproc::HandleInfo {
                handle: config_vmo.into_handle(),
                id: HandleInfo::new(HandleType::ComponentConfigVmo, 0).as_raw(),
            });
        }

        handle_infos
    }

    pub async fn start_component(
        &self,
        start_info: fcrunner::ComponentStartInfo,
        checker: &ScopedPolicyChecker,
    ) -> Result<ElfComponent, StartComponentError> {
        let start_info: StartInfo =
            start_info.try_into().map_err(StartInfoError::StartInfoError)?;

        let resolved_url = start_info.resolved_url.clone();

        // This also checks relevant security policy for config that it wraps using the provided
        // PolicyChecker.
        let program_config = ElfProgramConfig::parse_and_check(&start_info.program, &checker)
            .map_err(|err| {
                StartComponentError::StartInfoError(StartInfoError::ProgramError(err))
            })?;

        let main_process_critical = program_config.main_process_critical;
        let res: Result<ElfComponent, StartComponentError> =
            self.start_component_helper(start_info, checker.scope.clone(), program_config).await;
        match res {
            Err(e) if main_process_critical => {
                panic!(
                    "failed to launch component with a critical process ({:?}): {:?}",
                    &resolved_url, e
                )
            }
            x => x,
        }
    }

    async fn start_component_helper(
        &self,
        mut start_info: StartInfo,
        moniker: Moniker,
        program_config: ElfProgramConfig,
    ) -> Result<ElfComponent, StartComponentError> {
        let resolved_url = &start_info.resolved_url;

        // Fail early if there are clock issues.
        let boot_clock = zx::Clock::<zx::MonotonicTimeline, zx::BootTimeline>::create(
            zx::ClockOpts::CONTINUOUS,
            /*backstop=*/ None,
        )
        .map_err(StartComponentError::BootClockCreateFailed)?;

        // Connect to `fuchsia.process.Launcher`.
        let launcher = self
            .launcher_connector
            .connect()
            .map_err(|err| StartComponentError::ProcessLauncherConnectError(err.into()))?;

        // Create a job for this component that will contain its process.
        let job = self.create_job(&program_config)?;

        crash_handler::run_exceptions_server(
            job.top(),
            moniker.clone(),
            resolved_url.clone(),
            self.crash_records.clone(),
        )
        .map_err(StartComponentError::ExceptionRegistrationFailed)?;

        // Convert the directories into proxies, so we can find "/pkg" and open "lib" and bin_path
        let ns = namespace::Namespace::try_from(start_info.namespace)
            .map_err(StartComponentError::NamespaceError)?;

        let config_vmo =
            start_info.encoded_config.take().map(runner::get_config_vmo).transpose()?;

        let next_vdso = program_config.use_next_vdso.then(get_next_vdso_vmo).transpose()?;

        let (lifecycle_client, lifecycle_server) = if program_config.notify_lifecycle_stop {
            // Creating a channel is not expected to fail.
            let (client, server) = fidl::endpoints::create_proxy::<LifecycleMarker>();
            (Some(client), Some(server.into_channel()))
        } else {
            (None, None)
        };

        // Take the UTC clock handle out of `start_info.numbered_handles`, if available.
        let utc_handle = start_info
            .numbered_handles
            .iter()
            .position(|handles| handles.id == HandleInfo::new(HandleType::ClockUtc, 0).as_raw())
            .map(|position| start_info.numbered_handles.swap_remove(position).handle);

        let utc_clock = if let Some(handle) = utc_handle {
            zx::Clock::from(handle)
        } else {
            self.duplicate_utc_clock()
                .await
                .map_err(StartComponentError::UtcClockDuplicateFailed)?
        };

        // Duplicate the clock handle again, used later to wait for the clock to start, while
        // the original handle is passed to the process.
        let utc_clock_dup = utc_clock
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(StartComponentError::UtcClockDuplicateFailed)?;

        // Create and serve the runtime dir.
        let runtime_dir_server_end = start_info
            .runtime_dir
            .ok_or(StartComponentError::StartInfoError(StartInfoError::MissingRuntimeDir))?;
        let job_koid =
            job.proc().get_koid().map_err(StartComponentError::JobGetKoidFailed)?.raw_koid();

        let runtime_dir = RuntimeDirBuilder::new(runtime_dir_server_end)
            .args(program_config.args.clone())
            .job_id(job_koid)
            .serve();

        // If the component supports memory attribution, clone its outgoing directory connection
        // so that we may later connect to it.
        let outgoing_directory = if program_config.memory_attribution {
            let Some(outgoing_dir) = start_info.outgoing_dir else {
                return Err(StartComponentError::StartInfoError(
                    StartInfoError::MissingOutgoingDir,
                ));
            };
            let (outgoing_dir_client, outgoing_dir_server) = fidl::endpoints::create_endpoints();
            start_info.outgoing_dir = Some(outgoing_dir_server);
            fdio::open_at(
                outgoing_dir_client.channel(),
                ".",
                fio::Flags::PROTOCOL_DIRECTORY,
                outgoing_dir.into_channel(),
            )
            .unwrap();
            Some(outgoing_dir_client)
        } else {
            None
        };

        // Create procarg handles.
        let mut handle_infos = ElfRunner::create_handle_infos(
            start_info.outgoing_dir.map(|dir| dir.into_channel()),
            lifecycle_server,
            utc_clock,
            next_vdso,
            config_vmo,
        );

        // Add stdout and stderr handles that forward to syslog.
        let (stdout_and_stderr_tasks, stdout_and_stderr_handles) =
            bind_streams_to_syslog(&ns, program_config.stdout_sink, program_config.stderr_sink);
        handle_infos.extend(stdout_and_stderr_handles);

        // Add any external numbered handles.
        handle_infos.extend(start_info.numbered_handles);

        // If the program escrowed a dictionary, give it back via `numbered_handles`.
        if let Some(escrowed_dictionary) = start_info.escrowed_dictionary {
            handle_infos.push(fproc::HandleInfo {
                handle: escrowed_dictionary.token.into_handle().into(),
                id: HandleInfo::new(HandleType::EscrowedDictionary, 0).as_raw(),
            });
        }

        // Configure the process launcher.
        let proc_job_dup = job
            .proc()
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(StartComponentError::JobDuplicateFailed)?;

        let name = Path::new(resolved_url)
            .file_name()
            .and_then(|filename| filename.to_str())
            .ok_or_else(|| {
                StartComponentError::StartInfoError(StartInfoError::BadResolvedUrl(
                    resolved_url.clone(),
                ))
            })?;

        let launch_info =
            runner::component::configure_launcher(runner::component::LauncherConfigArgs {
                bin_path: &program_config.binary,
                name,
                options: program_config.process_options(),
                args: Some(program_config.args.clone()),
                ns,
                job: Some(proc_job_dup),
                handle_infos: Some(handle_infos),
                name_infos: None,
                environs: program_config.environ.clone(),
                launcher: &launcher,
                loader_proxy_chan: None,
                executable_vmo: None,
            })
            .await?;

        // Wait on break_on_start with a timeout and don't fail.
        if let Some(break_on_start) = start_info.break_on_start {
            fasync::OnSignals::new(&break_on_start, zx::Signals::OBJECT_PEER_CLOSED)
                .on_timeout(MAX_WAIT_BREAK_ON_START, || Err(zx::Status::TIMED_OUT))
                .await
                .err()
                .map(|error| warn!(%moniker, %error, "Failed to wait break_on_start"));
        }

        // Launch the process.
        let (status, process) = launcher
            .launch(launch_info)
            .await
            .map_err(StartComponentError::ProcessLauncherFidlError)?;
        zx::Status::ok(status).map_err(StartComponentError::CreateProcessFailed)?;
        let process = process.unwrap(); // Process is present iff status is OK.
        if program_config.main_process_critical {
            job_default()
                .set_critical(zx::JobCriticalOptions::RETCODE_NONZERO, &process)
                .map_err(StartComponentError::ProcessMarkCriticalFailed)
                .expect("failed to set process as critical");
        }

        let pid = process.get_koid().map_err(StartComponentError::ProcessGetKoidFailed)?.raw_koid();

        // Add process ID to the runtime dir.
        runtime_dir.add_process_id(pid);

        fuchsia_trace::instant!(
            c"component:start",
            c"elf",
            fuchsia_trace::Scope::Thread,
            "moniker" => format!("{}", moniker).as_str(),
            "url" => resolved_url.as_str(),
            "pid" => pid
        );

        // Add process start time to the runtime dir.
        let process_start_mono_ns =
            process.info().map_err(StartComponentError::ProcessInfoFailed)?.start_time;
        runtime_dir.add_process_start_time(process_start_mono_ns);

        // Add UTC estimate of the process start time to the runtime dir.
        let utc_clock_started = fasync::OnSignals::new(&utc_clock_dup, zx::Signals::CLOCK_STARTED)
            .on_timeout(zx::MonotonicInstant::after(zx::MonotonicDuration::default()), || {
                Err(zx::Status::TIMED_OUT)
            })
            .await
            .is_ok();

        // The clock transformations needed to map a timestamp on a monotonic timeline
        // to a timestamp on the UTC timeline.
        let mono_to_clock_transformation =
            boot_clock.get_details().map(|details| details.reference_to_synthetic).ok();
        let boot_to_utc_transformation = utc_clock_started
            .then(|| utc_clock_dup.get_details().map(|details| details.reference_to_synthetic).ok())
            .flatten();

        if let Some(clock_transformation) = boot_to_utc_transformation {
            // This composes two transformations, to get from a timestamp expressed in
            // nanoseconds on the monotonic timeline, to our best estimate of the
            // corresponding UTC date-time.
            //
            // The clock transformations are computed before they are applied. If
            // a suspend intervenes exactly between the computation and application,
            // the timelines will drift away during sleep, causing a wrong date-time
            // to be exposed in `runtime_dir`.
            //
            // This should not be a huge issue in practice, as the chances of that
            // happening are vanishingly small.
            let process_start_instant_mono =
                zx::MonotonicInstant::from_nanos(process_start_mono_ns);
            let maybe_time_utc = mono_to_clock_transformation
                .map(|t| t.apply(process_start_instant_mono))
                .map(|time_boot| clock_transformation.apply(time_boot));

            if let Some(utc_timestamp) = maybe_time_utc {
                let utc_time_ns = utc_timestamp.into_nanos();
                let seconds = (utc_time_ns / 1_000_000_000) as i64;
                let nanos = (utc_time_ns % 1_000_000_000) as u32;
                let dt = Utc
                    .from_utc_datetime(&NaiveDateTime::from_timestamp_opt(seconds, nanos).unwrap());

                // If any of the above values are unavailable (unlikely), then this
                // does not happen.
                runtime_dir.add_process_start_time_utc_estimate(dt.to_string())
            }
        };

        Ok(ElfComponent::new(
            runtime_dir,
            moniker,
            job,
            process,
            lifecycle_client,
            program_config.main_process_critical,
            stdout_and_stderr_tasks,
            resolved_url.clone(),
            outgoing_directory,
            program_config,
            start_info.component_instance.ok_or(StartComponentError::StartInfoError(
                StartInfoError::MissingComponentInstanceToken,
            ))?,
        ))
    }

    pub fn get_scoped_runner(
        self: Arc<Self>,
        checker: ScopedPolicyChecker,
    ) -> Arc<ScopedElfRunner> {
        Arc::new(ScopedElfRunner { runner: self, checker })
    }

    pub fn serve_memory_reporter(&self, stream: fattribution::ProviderRequestStream) {
        self.memory_reporter.serve(stream);
    }
}

pub struct ScopedElfRunner {
    runner: Arc<ElfRunner>,
    checker: ScopedPolicyChecker,
}

impl ScopedElfRunner {
    pub fn serve(&self, mut stream: fcrunner::ComponentRunnerRequestStream) {
        let runner = self.runner.clone();
        let checker = self.checker.clone();
        fasync::Task::spawn(async move {
            while let Ok(Some(request)) = stream.try_next().await {
                match request {
                    fcrunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                        start(&runner, checker.clone(), start_info, controller).await;
                    }
                    fcrunner::ComponentRunnerRequest::_UnknownMethod { ordinal, .. } => {
                        warn!(%ordinal, "Unknown ComponentRunner request");
                    }
                }
            }
        })
        .detach();
    }

    pub async fn start(
        &self,
        start_info: fcrunner::ComponentStartInfo,
        server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
    ) {
        start(&self.runner, self.checker.clone(), start_info, server_end).await
    }
}

/// Starts a component by creating a new Job and Process for the component.
async fn start(
    runner: &ElfRunner,
    checker: ScopedPolicyChecker,
    start_info: fcrunner::ComponentStartInfo,
    server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
) {
    let resolved_url = start_info.resolved_url.clone().unwrap_or_else(|| "<unknown>".to_string());

    let elf_component = match runner.start_component(start_info, &checker).await {
        Ok(elf_component) => elf_component,
        Err(err) => {
            runner::component::report_start_error(
                err.as_zx_status(),
                format!("{}", err),
                &resolved_url,
                server_end,
            );
            return;
        }
    };

    let (termination_tx, termination_rx) = oneshot::channel::<StopInfo>();
    // This function waits for something from the channel and
    // returns it or Error::Internal if the channel is closed
    let termination_fn = Box::pin(async move {
        termination_rx
            .await
            .unwrap_or_else(|_| {
                warn!("epitaph oneshot channel closed unexpectedly");
                StopInfo::from_error(fcomp::Error::Internal, None)
            })
            .into()
    });

    let Some(proc_copy) = elf_component.copy_process() else {
        runner::component::report_start_error(
            zx::Status::from_raw(
                i32::try_from(fcomp::Error::InstanceCannotStart.into_primitive()).unwrap(),
            ),
            "Component unexpectedly had no process".to_string(),
            &resolved_url,
            server_end,
        );
        return;
    };

    let component_diagnostics = elf_component
        .info()
        .copy_job_for_diagnostics()
        .map(|job| ComponentDiagnostics {
            tasks: Some(ComponentTasks {
                component_task: Some(DiagnosticsTask::Job(job.into())),
                ..Default::default()
            }),
            ..Default::default()
        })
        .map_err(|error| {
            warn!(%error, "Failed to copy job for diagnostics");
            ()
        })
        .ok();

    let (server_stream, control) = server_end.into_stream_and_control_handle();

    // Spawn a future that watches for the process to exit
    fasync::Task::spawn({
        let resolved_url = resolved_url.clone();
        async move {
            fasync::OnSignals::new(&proc_copy.as_handle_ref(), zx::Signals::PROCESS_TERMINATED)
                .await
                .map(|_: fidl::Signals| ()) // Discard.
                .unwrap_or_else(|error| warn!(%error, "error creating signal handler"));
            // Process exit code '0' is considered a clean return.
            // TODO(https://fxbug.dev/42134825) If we create an epitaph that indicates
            // intentional, non-zero exit, use that for all non-0 exit
            // codes.
            let stop_info = match proc_copy.info() {
                Ok(zx::ProcessInfo { return_code, .. }) => {
                    match return_code {
                        0 => StopInfo::from_ok(Some(return_code)),
                        // Don't log SYSCALL_KILL codes because they are expected in the course
                        // of normal operation. When elf_runner process a `Kill` method call for
                        // a component it makes a zx_task_kill syscall which sets this return code.
                        zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL => StopInfo::from_error(
                            fcomp::Error::InstanceDied.into(),
                            Some(return_code),
                        ),
                        _ => {
                            warn!(url=%resolved_url, %return_code,
                                "process terminated with abnormal return code");
                            StopInfo::from_error(fcomp::Error::InstanceDied, Some(return_code))
                        }
                    }
                }
                Err(error) => {
                    warn!(%error, "Unable to query process info");
                    StopInfo::from_error(fcomp::Error::Internal, None)
                }
            };
            termination_tx.send(stop_info).unwrap_or_else(|_| warn!("error sending done signal"));
        }
    })
    .detach();

    let mut elf_component = elf_component;
    runner.components.clone().add(&mut elf_component);

    // Create a future which owns and serves the controller
    // channel. The `epitaph_fn` future completes when the
    // component's main process exits. The controller then sets the
    // epitaph on the controller channel, closes it, and stops
    // serving the protocol.
    fasync::Task::spawn(async move {
        if let Some(component_diagnostics) = component_diagnostics {
            control.send_on_publish_diagnostics(component_diagnostics).unwrap_or_else(
                |error| warn!(url=%resolved_url, %error, "sending diagnostics failed"),
            );
        }
        runner::component::Controller::new(elf_component, server_stream, control)
            .serve(termination_fn)
            .await;
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::runtime_dir::RuntimeDirectory;
    use super::*;
    use anyhow::{Context, Error};
    use assert_matches::assert_matches;
    use cm_config::{AllowlistEntryBuilder, JobPolicyAllowlists, SecurityPolicy};
    use fidl::endpoints::{
        create_endpoints, create_proxy, spawn_stream_handler, ClientEnd,
        DiscoverableProtocolMarker, Proxy,
    };
    use fidl_connector::Connect;
    use fidl_fuchsia_diagnostics_types::Task as DiagnosticsTask;
    use fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequest, LogSinkRequestStream};
    use fidl_fuchsia_process_lifecycle::LifecycleProxy;
    use fuchsia_component::server::{ServiceFs, ServiceObjLocal};
    use futures::channel::mpsc;
    use futures::lock::Mutex;
    use futures::{join, StreamExt};
    use runner::component::Controllable;
    use std::task::Poll;
    use zx::{self as zx, Task};
    use {
        fidl_fuchsia_component as fcomp, fidl_fuchsia_component_runner as fcrunner,
        fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    };

    pub enum MockServiceRequest {
        LogSink(LogSinkRequestStream),
    }

    pub type MockServiceFs<'a> = ServiceFs<ServiceObjLocal<'a, MockServiceRequest>>;

    /// Create a new local fs and install a mock LogSink service into.
    /// Returns the created directory and corresponding namespace entries.
    pub fn create_fs_with_mock_logsink(
    ) -> Result<(MockServiceFs<'static>, Vec<fcrunner::ComponentNamespaceEntry>), Error> {
        let (dir_client, dir_server) = create_endpoints::<fio::DirectoryMarker>();

        let mut dir = ServiceFs::new_local();
        dir.add_fidl_service_at(LogSinkMarker::PROTOCOL_NAME, MockServiceRequest::LogSink);
        dir.serve_connection(dir_server).context("Failed to add serving channel.")?;

        let namespace = vec![fcrunner::ComponentNamespaceEntry {
            path: Some("/svc".to_string()),
            directory: Some(dir_client),
            ..Default::default()
        }];

        Ok((dir, namespace))
    }

    pub fn new_elf_runner_for_test() -> Arc<ElfRunner> {
        Arc::new(ElfRunner::new(
            job_default().duplicate(zx::Rights::SAME_RIGHTS).unwrap(),
            Box::new(process_launcher::BuiltInConnector {}),
            None,
            CrashRecords::new(),
        ))
    }

    fn namespace_entry(path: &str, flags: fio::Flags) -> fcrunner::ComponentNamespaceEntry {
        // Get a handle to /pkg
        let ns_path = path.to_string();
        let ns_dir = fuchsia_fs::directory::open_in_namespace(path, flags).unwrap();
        // TODO(https://fxbug.dev/42060182): Use Proxy::into_client_end when available.
        let client_end = ClientEnd::new(
            ns_dir.into_channel().expect("could not convert proxy to channel").into_zx_channel(),
        );
        fcrunner::ComponentNamespaceEntry {
            path: Some(ns_path),
            directory: Some(client_end),
            ..Default::default()
        }
    }

    fn pkg_dir_namespace_entry() -> fcrunner::ComponentNamespaceEntry {
        namespace_entry("/pkg", fio::PERM_READABLE | fio::PERM_EXECUTABLE)
    }

    fn svc_dir_namespace_entry() -> fcrunner::ComponentNamespaceEntry {
        namespace_entry("/svc", fio::PERM_READABLE)
    }

    fn hello_world_startinfo(
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> fcrunner::ComponentStartInfo {
        let ns = vec![pkg_dir_namespace_entry()];

        fcrunner::ComponentStartInfo {
            resolved_url: Some(
                "fuchsia-pkg://fuchsia.com/elf_runner_tests#meta/hello-world-rust.cm".to_string(),
            ),
            program: Some(fdata::Dictionary {
                entries: Some(vec![
                    fdata::DictionaryEntry {
                        key: "args".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::StrVec(vec![
                            "foo".to_string(),
                            "bar".to_string(),
                        ]))),
                    },
                    fdata::DictionaryEntry {
                        key: "binary".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str(
                            "bin/hello_world_rust".to_string(),
                        ))),
                    },
                ]),
                ..Default::default()
            }),
            ns: Some(ns),
            outgoing_dir: None,
            runtime_dir: Some(runtime_dir),
            component_instance: Some(zx::Event::create()),
            ..Default::default()
        }
    }

    /// ComponentStartInfo that points to a non-existent binary.
    fn invalid_binary_startinfo(
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> fcrunner::ComponentStartInfo {
        let ns = vec![pkg_dir_namespace_entry()];

        fcrunner::ComponentStartInfo {
            resolved_url: Some(
                "fuchsia-pkg://fuchsia.com/elf_runner_tests#meta/does-not-exist.cm".to_string(),
            ),
            program: Some(fdata::Dictionary {
                entries: Some(vec![fdata::DictionaryEntry {
                    key: "binary".to_string(),
                    value: Some(Box::new(fdata::DictionaryValue::Str(
                        "bin/does_not_exist".to_string(),
                    ))),
                }]),
                ..Default::default()
            }),
            ns: Some(ns),
            outgoing_dir: None,
            runtime_dir: Some(runtime_dir),
            component_instance: Some(zx::Event::create()),
            ..Default::default()
        }
    }

    /// Creates start info for a component which runs until told to exit. The
    /// ComponentController protocol can be used to stop the component when the
    /// test is done inspecting the launched component.
    pub fn lifecycle_startinfo(
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> fcrunner::ComponentStartInfo {
        let ns = vec![pkg_dir_namespace_entry()];

        fcrunner::ComponentStartInfo {
            resolved_url: Some(
                "fuchsia-pkg://fuchsia.com/lifecycle-example#meta/lifecycle.cm".to_string(),
            ),
            program: Some(fdata::Dictionary {
                entries: Some(vec![
                    fdata::DictionaryEntry {
                        key: "args".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::StrVec(vec![
                            "foo".to_string(),
                            "bar".to_string(),
                        ]))),
                    },
                    fdata::DictionaryEntry {
                        key: "binary".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str(
                            "bin/lifecycle_placeholder".to_string(),
                        ))),
                    },
                    fdata::DictionaryEntry {
                        key: "lifecycle.stop_event".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str("notify".to_string()))),
                    },
                ]),
                ..Default::default()
            }),
            ns: Some(ns),
            outgoing_dir: None,
            runtime_dir: Some(runtime_dir),
            component_instance: Some(zx::Event::create()),
            ..Default::default()
        }
    }

    fn create_child_process(job: &zx::Job, name: &str) -> zx::Process {
        let (process, _vmar) = job
            .create_child_process(zx::ProcessOptions::empty(), name.as_bytes())
            .expect("could not create process");
        process
    }

    fn make_default_elf_component(
        lifecycle_client: Option<LifecycleProxy>,
        critical: bool,
    ) -> (scoped_task::Scoped<zx::Job>, ElfComponent) {
        let job = scoped_task::create_child_job().expect("failed to make child job");
        let process = create_child_process(&job, "test_process");
        let job_copy =
            job.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("job handle duplication failed");
        let component = ElfComponent::new(
            RuntimeDirectory::empty(),
            Moniker::default(),
            Job::Single(job_copy),
            process,
            lifecycle_client,
            critical,
            Vec::new(),
            "".to_string(),
            None,
            Default::default(),
            zx::Event::create(),
        );
        (job, component)
    }

    // TODO(https://fxbug.dev/42073224): A variation of this is used in a couple of places. We should consider
    // refactoring this into a test util file.
    async fn read_file<'a>(root_proxy: &'a fio::DirectoryProxy, path: &'a str) -> String {
        let file_proxy =
            fuchsia_fs::directory::open_file_async(&root_proxy, path, fuchsia_fs::PERM_READABLE)
                .expect("Failed to open file.");
        let res = fuchsia_fs::file::read_to_string(&file_proxy).await;
        res.expect("Unable to read file.")
    }

    #[fuchsia::test]
    async fn test_runtime_dir_entries() -> Result<(), Error> {
        let (runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>();
        let start_info = lifecycle_startinfo(runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();

        runner.start(start_info, server_controller).await;

        // Verify that args are added to the runtime directory.
        assert_eq!("foo", read_file(&runtime_dir, "args/0").await);
        assert_eq!("bar", read_file(&runtime_dir, "args/1").await);

        // Process Id, Process Start Time, Job Id will vary with every run of this test. Here we
        // verify that they exist in the runtime directory, they can be parsed as integers,
        // they're greater than zero and they are not the same value. Those are about the only
        // invariants we can verify across test runs.
        let process_id = read_file(&runtime_dir, "elf/process_id").await.parse::<u64>()?;
        let process_start_time =
            read_file(&runtime_dir, "elf/process_start_time").await.parse::<i64>()?;
        let process_start_time_utc_estimate =
            read_file(&runtime_dir, "elf/process_start_time_utc_estimate").await;
        let job_id = read_file(&runtime_dir, "elf/job_id").await.parse::<u64>()?;
        assert!(process_id > 0);
        assert!(process_start_time > 0);
        assert!(process_start_time_utc_estimate.contains("UTC"));
        assert!(job_id > 0);
        assert_ne!(process_id, job_id);

        controller.stop().expect("Stop request failed");
        // Wait for the process to exit so the test doesn't pagefault due to an invalid stdout
        // handle.
        controller.on_closed().await.expect("failed waiting for channel to close");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_kill_component() -> Result<(), Error> {
        let (job, mut component) = make_default_elf_component(None, false);

        let job_info = job.info()?;
        assert!(!job_info.exited);

        component.kill().await;

        let h = job.as_handle_ref();
        fasync::OnSignals::new(&h, zx::Signals::TASK_TERMINATED)
            .await
            .expect("failed waiting for termination signal");

        let job_info = job.info()?;
        assert!(job_info.exited);
        Ok(())
    }

    #[fuchsia::test]
    fn test_stop_critical_component() -> Result<(), Error> {
        let mut exec = fasync::TestExecutor::new();
        // Presence of the Lifecycle channel isn't used by ElfComponent to sense
        // component exit, but it does modify the stop behavior and this is
        // what we want to test.
        let (lifecycle_client, _lifecycle_server) = create_proxy::<LifecycleMarker>();
        let (job, mut component) = make_default_elf_component(Some(lifecycle_client), true);
        let process = component.copy_process().unwrap();
        let job_info = job.info()?;
        assert!(!job_info.exited);

        // Ask the runner to stop the component, it returns a future which
        // completes when the component closes its side of the lifecycle
        // channel
        let mut completes_when_stopped = component.stop();

        // The returned future shouldn't complete because we're holding the
        // lifecycle channel open.
        match exec.run_until_stalled(&mut completes_when_stopped) {
            Poll::Ready(_) => {
                panic!("runner should still be waiting for lifecycle channel to stop");
            }
            _ => {}
        }
        assert_eq!(process.kill(), Ok(()));

        exec.run_singlethreaded(&mut completes_when_stopped);

        // Check that the runner killed the job hosting the exited component.
        let h = job.as_handle_ref();
        let termination_fut = async move {
            fasync::OnSignals::new(&h, zx::Signals::TASK_TERMINATED)
                .await
                .expect("failed waiting for termination signal");
        };
        exec.run_singlethreaded(termination_fut);

        let job_info = job.info()?;
        assert!(job_info.exited);
        Ok(())
    }

    #[fuchsia::test]
    fn test_stop_noncritical_component() -> Result<(), Error> {
        let mut exec = fasync::TestExecutor::new();
        // Presence of the Lifecycle channel isn't used by ElfComponent to sense
        // component exit, but it does modify the stop behavior and this is
        // what we want to test.
        let (lifecycle_client, lifecycle_server) = create_proxy::<LifecycleMarker>();
        let (job, mut component) = make_default_elf_component(Some(lifecycle_client), false);

        let job_info = job.info()?;
        assert!(!job_info.exited);

        // Ask the runner to stop the component, it returns a future which
        // completes when the component closes its side of the lifecycle
        // channel
        let mut completes_when_stopped = component.stop();

        // The returned future shouldn't complete because we're holding the
        // lifecycle channel open.
        match exec.run_until_stalled(&mut completes_when_stopped) {
            Poll::Ready(_) => {
                panic!("runner should still be waiting for lifecycle channel to stop");
            }
            _ => {}
        }
        drop(lifecycle_server);

        match exec.run_until_stalled(&mut completes_when_stopped) {
            Poll::Ready(_) => {}
            _ => {
                panic!("runner future should have completed, lifecycle channel is closed.");
            }
        }
        // Check that the runner killed the job hosting the exited component.
        let h = job.as_handle_ref();
        let termination_fut = async move {
            fasync::OnSignals::new(&h, zx::Signals::TASK_TERMINATED)
                .await
                .expect("failed waiting for termination signal");
        };
        exec.run_singlethreaded(termination_fut);

        let job_info = job.info()?;
        assert!(job_info.exited);
        Ok(())
    }

    /// Stopping a component which doesn't have a lifecycle channel should be
    /// equivalent to killing a component directly.
    #[fuchsia::test]
    async fn test_stop_component_without_lifecycle() -> Result<(), Error> {
        let (job, mut component) = make_default_elf_component(None, false);

        let job_info = job.info()?;
        assert!(!job_info.exited);

        component.stop().await;

        let h = job.as_handle_ref();
        fasync::OnSignals::new(&h, zx::Signals::TASK_TERMINATED)
            .await
            .expect("failed waiting for termination signal");

        let job_info = job.info()?;
        assert!(job_info.exited);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_stop_critical_component_with_closed_lifecycle() -> Result<(), Error> {
        let (lifecycle_client, lifecycle_server) = create_proxy::<LifecycleMarker>();
        let (job, mut component) = make_default_elf_component(Some(lifecycle_client), true);
        let process = component.copy_process().unwrap();
        let job_info = job.info()?;
        assert!(!job_info.exited);

        // Close the lifecycle channel
        drop(lifecycle_server);
        // Kill the process because this is what ElfComponent monitors to
        // determine if the component exited.
        process.kill()?;
        component.stop().await;

        let h = job.as_handle_ref();
        fasync::OnSignals::new(&h, zx::Signals::TASK_TERMINATED)
            .await
            .expect("failed waiting for termination signal");

        let job_info = job.info()?;
        assert!(job_info.exited);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_stop_noncritical_component_with_closed_lifecycle() -> Result<(), Error> {
        let (lifecycle_client, lifecycle_server) = create_proxy::<LifecycleMarker>();
        let (job, mut component) = make_default_elf_component(Some(lifecycle_client), false);

        let job_info = job.info()?;
        assert!(!job_info.exited);

        // Close the lifecycle channel
        drop(lifecycle_server);
        // Kill the process because this is what ElfComponent monitors to
        // determine if the component exited.
        component.stop().await;

        let h = job.as_handle_ref();
        fasync::OnSignals::new(&h, zx::Signals::TASK_TERMINATED)
            .await
            .expect("failed waiting for termination signal");

        let job_info = job.info()?;
        assert!(job_info.exited);
        Ok(())
    }

    /// Dropping the component should kill the job hosting it.
    #[fuchsia::test]
    async fn test_drop() -> Result<(), Error> {
        let (job, component) = make_default_elf_component(None, false);

        let job_info = job.info()?;
        assert!(!job_info.exited);

        drop(component);

        let h = job.as_handle_ref();
        fasync::OnSignals::new(&h, zx::Signals::TASK_TERMINATED)
            .await
            .expect("failed waiting for termination signal");

        let job_info = job.info()?;
        assert!(job_info.exited);
        Ok(())
    }

    fn with_mark_vmo_exec(
        mut start_info: fcrunner::ComponentStartInfo,
    ) -> fcrunner::ComponentStartInfo {
        start_info.program.as_mut().map(|dict| {
            dict.entries.as_mut().map(|entry| {
                entry.push(fdata::DictionaryEntry {
                    key: "job_policy_ambient_mark_vmo_exec".to_string(),
                    value: Some(Box::new(fdata::DictionaryValue::Str("true".to_string()))),
                });
                entry
            })
        });
        start_info
    }

    fn with_main_process_critical(
        mut start_info: fcrunner::ComponentStartInfo,
    ) -> fcrunner::ComponentStartInfo {
        start_info.program.as_mut().map(|dict| {
            dict.entries.as_mut().map(|entry| {
                entry.push(fdata::DictionaryEntry {
                    key: "main_process_critical".to_string(),
                    value: Some(Box::new(fdata::DictionaryValue::Str("true".to_string()))),
                });
                entry
            })
        });
        start_info
    }

    #[fuchsia::test]
    async fn vmex_security_policy_denied() -> Result<(), Error> {
        let (_runtime_dir, runtime_dir_server) = create_endpoints::<fio::DirectoryMarker>();
        let start_info = with_mark_vmo_exec(lifecycle_startinfo(runtime_dir_server));

        // Config does not allowlist any monikers to have access to the job policy.
        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();

        // Attempting to start the component should fail, which we detect by looking for an
        // ACCESS_DENIED epitaph on the ComponentController's event stream.
        runner.start(start_info, server_controller).await;
        assert_matches!(
            controller.take_event_stream().try_next().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::ACCESS_DENIED, .. })
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn vmex_security_policy_allowed() -> Result<(), Error> {
        let (runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>();
        let start_info = with_mark_vmo_exec(lifecycle_startinfo(runtime_dir_server));

        let policy = SecurityPolicy {
            job_policy: JobPolicyAllowlists {
                ambient_mark_vmo_exec: vec![AllowlistEntryBuilder::new().exact("foo").build()],
                ..Default::default()
            },
            ..Default::default()
        };
        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(policy),
            Moniker::try_from(vec!["foo"]).unwrap(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();
        runner.start(start_info, server_controller).await;

        // Runtime dir won't exist if the component failed to start.
        let process_id = read_file(&runtime_dir, "elf/process_id").await.parse::<u64>()?;
        assert!(process_id > 0);
        // Component controller should get shutdown normally; no ACCESS_DENIED epitaph.
        controller.kill().expect("kill failed");

        // We expect the event stream to have closed, which is reported as an
        // error and the value of the error should match the epitaph for a
        // process that was killed.
        let mut event_stream = controller.take_event_stream();
        expect_diagnostics_event(&mut event_stream).await;

        let s = zx::Status::from_raw(
            i32::try_from(fcomp::Error::InstanceDied.into_primitive()).unwrap(),
        );
        expect_on_stop(&mut event_stream, s, Some(zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL)).await;
        expect_channel_closed(&mut event_stream).await;
        Ok(())
    }

    #[fuchsia::test]
    async fn critical_security_policy_denied() -> Result<(), Error> {
        let (_runtime_dir, runtime_dir_server) = create_endpoints::<fio::DirectoryMarker>();
        let start_info = with_main_process_critical(hello_world_startinfo(runtime_dir_server));

        // Default policy does not allowlist any monikers to be marked as critical
        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();

        // Attempting to start the component should fail, which we detect by looking for an
        // ACCESS_DENIED epitaph on the ComponentController's event stream.
        runner.start(start_info, server_controller).await;
        assert_matches!(
            controller.take_event_stream().try_next().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::ACCESS_DENIED, .. })
        );

        Ok(())
    }

    #[fuchsia::test]
    #[should_panic]
    async fn fail_to_launch_critical_component() {
        let (_runtime_dir, runtime_dir_server) = create_endpoints::<fio::DirectoryMarker>();

        // ElfRunner should fail to start the component because this start_info points
        // to a binary that does not exist in the test package.
        let start_info = with_main_process_critical(invalid_binary_startinfo(runtime_dir_server));

        // Policy does not allowlist any monikers to be marked as critical without being
        // allowlisted, so make sure we permit this one.
        let policy = SecurityPolicy {
            job_policy: JobPolicyAllowlists {
                main_process_critical: vec![AllowlistEntryBuilder::new().build()],
                ..Default::default()
            },
            ..Default::default()
        };
        let runner = new_elf_runner_for_test();
        let runner =
            runner.get_scoped_runner(ScopedPolicyChecker::new(Arc::new(policy), Moniker::root()));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();

        runner.start(start_info, server_controller).await;

        controller
            .take_event_stream()
            .try_next()
            .await
            .map(|_: Option<fcrunner::ComponentControllerEvent>| ()) // Discard.
            .unwrap_or_else(|error| warn!(%error, "error reading from event stream"));
    }

    fn hello_world_startinfo_forward_stdout_to_log(
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
        mut ns: Vec<fcrunner::ComponentNamespaceEntry>,
    ) -> fcrunner::ComponentStartInfo {
        ns.push(pkg_dir_namespace_entry());

        fcrunner::ComponentStartInfo {
            resolved_url: Some(
                "fuchsia-pkg://fuchsia.com/hello-world-rust#meta/hello-world-rust.cm".to_string(),
            ),
            program: Some(fdata::Dictionary {
                entries: Some(vec![
                    fdata::DictionaryEntry {
                        key: "binary".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str(
                            "bin/hello_world_rust".to_string(),
                        ))),
                    },
                    fdata::DictionaryEntry {
                        key: "forward_stdout_to".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str("log".to_string()))),
                    },
                    fdata::DictionaryEntry {
                        key: "forward_stderr_to".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str("log".to_string()))),
                    },
                ]),
                ..Default::default()
            }),
            ns: Some(ns),
            outgoing_dir: None,
            runtime_dir: Some(runtime_dir),
            component_instance: Some(zx::Event::create()),
            ..Default::default()
        }
    }

    // TODO(https://fxbug.dev/42148789): Following function shares a lot of code with
    // //src/sys/component_manager/src/model/namespace.rs tests. Shared
    // functionality should be refactored into a common test util lib.
    #[fuchsia::test]
    async fn enable_stdout_and_stderr_logging() -> Result<(), Error> {
        let (dir, ns) = create_fs_with_mock_logsink()?;

        let run_component_fut = async move {
            let (_runtime_dir, runtime_dir_server) = create_endpoints::<fio::DirectoryMarker>();
            let start_info = hello_world_startinfo_forward_stdout_to_log(runtime_dir_server, ns);

            let runner = new_elf_runner_for_test();
            let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
                Arc::new(SecurityPolicy::default()),
                Moniker::root(),
            ));
            let (client_controller, server_controller) =
                create_proxy::<fcrunner::ComponentControllerMarker>();

            runner.start(start_info, server_controller).await;
            let mut event_stream = client_controller.take_event_stream();
            expect_diagnostics_event(&mut event_stream).await;
            expect_on_stop(&mut event_stream, zx::Status::OK, Some(0)).await;
            expect_channel_closed(&mut event_stream).await;
        };

        // Just check for connection count, other integration tests cover decoding the actual logs.
        let connection_count = 1u8;
        let request_count = Arc::new(Mutex::new(0u8));
        let request_count_copy = request_count.clone();

        let service_fs_listener_fut = async move {
            dir.for_each_concurrent(None, move |request: MockServiceRequest| match request {
                MockServiceRequest::LogSink(mut r) => {
                    let req_count = request_count_copy.clone();
                    async move {
                        while let Some(Ok(req)) = r.next().await {
                            match req {
                                LogSinkRequest::Connect { .. } => {
                                    panic!("Unexpected call to `Connect`");
                                }
                                LogSinkRequest::ConnectStructured { .. } => {
                                    let mut count = req_count.lock().await;
                                    *count += 1;
                                }
                                LogSinkRequest::WaitForInterestChange { .. } => {
                                    // this is expected but asserting it was received is flakey because
                                    // it's sent at some point after the scoped logger is created
                                }
                            }
                        }
                    }
                }
            })
            .await;
        };

        join!(run_component_fut, service_fs_listener_fut);

        assert_eq!(*request_count.lock().await, connection_count);
        Ok(())
    }

    #[fuchsia::test]
    async fn on_publish_diagnostics_contains_job_handle() -> Result<(), Error> {
        let (runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>();
        let start_info = lifecycle_startinfo(runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();

        runner.start(start_info, server_controller).await;

        let job_id = read_file(&runtime_dir, "elf/job_id").await.parse::<u64>().unwrap();
        let mut event_stream = controller.take_event_stream();
        match event_stream.try_next().await {
            Ok(Some(fcrunner::ComponentControllerEvent::OnPublishDiagnostics {
                payload:
                    ComponentDiagnostics {
                        tasks:
                            Some(ComponentTasks {
                                component_task: Some(DiagnosticsTask::Job(job)), ..
                            }),
                        ..
                    },
            })) => {
                assert_eq!(job_id, job.get_koid().unwrap().raw_koid());
            }
            other => panic!("unexpected event result: {:?}", other),
        }

        controller.stop().expect("Stop request failed");
        // Wait for the process to exit so the test doesn't pagefault due to an invalid stdout
        // handle.
        controller.on_closed().await.expect("failed waiting for channel to close");

        Ok(())
    }

    async fn expect_diagnostics_event(event_stream: &mut fcrunner::ComponentControllerEventStream) {
        let event = event_stream.try_next().await;
        assert_matches!(
            event,
            Ok(Some(fcrunner::ComponentControllerEvent::OnPublishDiagnostics {
                payload: ComponentDiagnostics {
                    tasks: Some(ComponentTasks {
                        component_task: Some(DiagnosticsTask::Job(_)),
                        ..
                    }),
                    ..
                },
            }))
        );
    }

    async fn expect_on_stop(
        event_stream: &mut fcrunner::ComponentControllerEventStream,
        expected_status: zx::Status,
        expected_exit_code: Option<i64>,
    ) {
        let event = event_stream.try_next().await;
        assert_matches!(
            event,
            Ok(Some(fcrunner::ComponentControllerEvent::OnStop {
                payload: fcrunner::ComponentStopInfo { termination_status: Some(s), exit_code, .. },
            }))
            if s == expected_status.into_raw() &&
                exit_code == expected_exit_code
        );
    }

    async fn expect_channel_closed(event_stream: &mut fcrunner::ComponentControllerEventStream) {
        let event = event_stream.try_next().await;
        match event {
            Ok(None) => {}
            other => panic!("Expected channel closed error, got {:?}", other),
        }
    }

    /// An implementation of launcher that sends a complete launch request payload back to
    /// a test through an mpsc channel.
    struct LauncherConnectorForTest {
        sender: mpsc::UnboundedSender<LaunchPayload>,
    }

    /// Contains all the information passed to fuchsia.process.Launcher before and up to calling
    /// Launch/CreateWithoutStarting.
    #[derive(Default)]
    struct LaunchPayload {
        launch_info: Option<fproc::LaunchInfo>,
        args: Vec<Vec<u8>>,
        environ: Vec<Vec<u8>>,
        name_info: Vec<fproc::NameInfo>,
        handles: Vec<fproc::HandleInfo>,
        options: u32,
    }

    impl Connect for LauncherConnectorForTest {
        type Proxy = fproc::LauncherProxy;

        fn connect(&self) -> Result<Self::Proxy, anyhow::Error> {
            let sender = self.sender.clone();
            let payload = Arc::new(Mutex::new(LaunchPayload::default()));

            Ok(spawn_stream_handler(move |launcher_request| {
                let sender = sender.clone();
                let payload = payload.clone();
                async move {
                    let mut payload = payload.lock().await;
                    match launcher_request {
                        fproc::LauncherRequest::Launch { info, responder } => {
                            let process = create_child_process(&info.job, "test_process");
                            responder.send(zx::Status::OK.into_raw(), Some(process)).unwrap();

                            let mut payload =
                                std::mem::replace(&mut *payload, LaunchPayload::default());
                            payload.launch_info = Some(info);
                            sender.unbounded_send(payload).unwrap();
                        }
                        fproc::LauncherRequest::CreateWithoutStarting { info: _, responder: _ } => {
                            unimplemented!()
                        }
                        fproc::LauncherRequest::AddArgs { mut args, control_handle: _ } => {
                            payload.args.append(&mut args);
                        }
                        fproc::LauncherRequest::AddEnvirons { mut environ, control_handle: _ } => {
                            payload.environ.append(&mut environ);
                        }
                        fproc::LauncherRequest::AddNames { mut names, control_handle: _ } => {
                            payload.name_info.append(&mut names);
                        }
                        fproc::LauncherRequest::AddHandles { mut handles, control_handle: _ } => {
                            payload.handles.append(&mut handles);
                        }
                        fproc::LauncherRequest::SetOptions { options, .. } => {
                            payload.options = options;
                        }
                    }
                }
            }))
        }
    }

    #[fuchsia::test]
    async fn process_created_with_utc_clock_from_numbered_handles() -> Result<(), Error> {
        let (payload_tx, mut payload_rx) = mpsc::unbounded();

        let connector = LauncherConnectorForTest { sender: payload_tx };
        let runner = ElfRunner::new(
            job_default().duplicate(zx::Rights::SAME_RIGHTS).unwrap(),
            Box::new(connector),
            None,
            CrashRecords::new(),
        );
        let policy_checker = ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::try_from(vec!["foo"]).unwrap(),
        );

        // Create a clock and pass it to the component as the UTC clock through numbered_handles.
        let clock =
            zx::SyntheticClock::create(zx::ClockOpts::AUTO_START | zx::ClockOpts::MONOTONIC, None)?;
        let clock_koid = clock.get_koid().unwrap();

        let (_runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>();
        let mut start_info = hello_world_startinfo(runtime_dir_server);
        start_info.numbered_handles = Some(vec![fproc::HandleInfo {
            handle: clock.into_handle(),
            id: HandleInfo::new(HandleType::ClockUtc, 0).as_raw(),
        }]);

        // Start the component.
        let _ = runner
            .start_component(start_info, &policy_checker)
            .await
            .context("failed to start component")?;

        let payload = payload_rx.next().await.unwrap();
        assert!(payload
            .handles
            .iter()
            .any(|handle_info| handle_info.handle.get_koid().unwrap() == clock_koid));

        Ok(())
    }

    /// Test visiting running components using [`ComponentSet`].
    #[fuchsia::test]
    async fn test_enumerate_components() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (_runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>();
        let start_info = lifecycle_startinfo(runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let components = runner.components.clone();

        // Initially there are zero components.
        let count = Arc::new(AtomicUsize::new(0));
        components.clone().visit(|_, _| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 0);

        // Run a component.
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();
        runner.start(start_info, server_controller).await;

        // There should now be one component in the set.
        let count = Arc::new(AtomicUsize::new(0));
        components.clone().visit(|elf_component: &ElfComponentInfo, _| {
            assert_eq!(
                elf_component.get_url().as_str(),
                "fuchsia-pkg://fuchsia.com/lifecycle-example#meta/lifecycle.cm"
            );
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // Stop the component.
        controller.stop().unwrap();
        controller.on_closed().await.unwrap();

        // There should now be zero components in the set.
        // Keep retrying until the component is asynchronously deregistered.
        loop {
            let count = Arc::new(AtomicUsize::new(0));
            components.clone().visit(|_, _| {
                count.fetch_add(1, Ordering::SeqCst);
            });
            let count = count.load(Ordering::SeqCst);
            assert!(count == 0 || count == 1);
            if count == 0 {
                break;
            }
            // Yield to the executor once so that we are not starving the
            // asynchronous deregistration task from running.
            yield_to_executor().await;
        }
    }

    async fn yield_to_executor() {
        let mut done = false;
        futures::future::poll_fn(|cx| {
            if done {
                Poll::Ready(())
            } else {
                done = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    /// Creates start info for a component which runs immediately escrows its
    /// outgoing directory and then exits.
    pub fn immediate_escrow_startinfo(
        outgoing_dir: ServerEnd<fio::DirectoryMarker>,
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> fcrunner::ComponentStartInfo {
        let ns = vec![
            pkg_dir_namespace_entry(),
            // Give the test component LogSink.
            svc_dir_namespace_entry(),
        ];

        fcrunner::ComponentStartInfo {
            resolved_url: Some("#meta/immediate_escrow_component.cm".to_string()),
            program: Some(fdata::Dictionary {
                entries: Some(vec![
                    fdata::DictionaryEntry {
                        key: "binary".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str(
                            "bin/immediate_escrow".to_string(),
                        ))),
                    },
                    fdata::DictionaryEntry {
                        key: "lifecycle.stop_event".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str("notify".to_string()))),
                    },
                ]),
                ..Default::default()
            }),
            ns: Some(ns),
            outgoing_dir: Some(outgoing_dir),
            runtime_dir: Some(runtime_dir),
            component_instance: Some(zx::Event::create()),
            ..Default::default()
        }
    }

    /// Test that an ELF component can send an `OnEscrow` event on its lifecycle
    /// channel and this event is forwarded to the `ComponentController`.
    #[fuchsia::test]
    async fn test_lifecycle_on_escrow() {
        let (outgoing_dir_client, outgoing_dir_server) =
            fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let (_, runtime_dir_server) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let start_info = immediate_escrow_startinfo(outgoing_dir_server, runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();

        runner.start(start_info, server_controller).await;

        let mut event_stream = controller.take_event_stream();

        expect_diagnostics_event(&mut event_stream).await;

        match event_stream.try_next().await {
            Ok(Some(fcrunner::ComponentControllerEvent::OnEscrow {
                payload: fcrunner::ComponentControllerOnEscrowRequest { outgoing_dir, .. },
            })) => {
                let outgoing_dir_server = outgoing_dir.unwrap();

                assert_eq!(
                    outgoing_dir_client.basic_info().unwrap().koid,
                    outgoing_dir_server.basic_info().unwrap().related_koid
                );
            }
            other => panic!("unexpected event result: {:?}", other),
        }

        expect_on_stop(&mut event_stream, zx::Status::OK, Some(0)).await;
        expect_channel_closed(&mut event_stream).await;
    }

    fn exit_with_code_startinfo(exit_code: i64) -> fcrunner::ComponentStartInfo {
        let (_runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>();
        let ns = vec![pkg_dir_namespace_entry()];

        fcrunner::ComponentStartInfo {
            resolved_url: Some(
                "fuchsia-pkg://fuchsia.com/elf_runner_tests#meta/exit-with-code.cm".to_string(),
            ),
            program: Some(fdata::Dictionary {
                entries: Some(vec![
                    fdata::DictionaryEntry {
                        key: "args".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::StrVec(vec![format!(
                            "{}",
                            exit_code
                        )]))),
                    },
                    fdata::DictionaryEntry {
                        key: "binary".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str(
                            "bin/exit_with_code".to_string(),
                        ))),
                    },
                ]),
                ..Default::default()
            }),
            ns: Some(ns),
            outgoing_dir: None,
            runtime_dir: Some(runtime_dir_server),
            component_instance: Some(zx::Event::create()),
            ..Default::default()
        }
    }

    #[fuchsia::test]
    async fn test_return_code_success() {
        let start_info = exit_with_code_startinfo(0);

        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();
        runner.start(start_info, server_controller).await;

        let mut event_stream = controller.take_event_stream();
        expect_diagnostics_event(&mut event_stream).await;
        expect_on_stop(&mut event_stream, zx::Status::OK, Some(0)).await;
        expect_channel_closed(&mut event_stream).await;
    }

    #[fuchsia::test]
    async fn test_return_code_failure() {
        let start_info = exit_with_code_startinfo(123);

        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>();
        runner.start(start_info, server_controller).await;

        let mut event_stream = controller.take_event_stream();
        expect_diagnostics_event(&mut event_stream).await;
        let s = zx::Status::from_raw(
            i32::try_from(fcomp::Error::InstanceDied.into_primitive()).unwrap(),
        );
        expect_on_stop(&mut event_stream, s, Some(123)).await;
        expect_channel_closed(&mut event_stream).await;
    }
}
