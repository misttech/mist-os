// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod component;
mod config;
mod crash_handler;
pub mod crash_info;
mod error;
pub mod process_launcher;
mod runtime_dir;
mod stdout;
pub mod vdso_vmo;

use {
    self::{
        component::ElfComponent,
        config::ElfProgramConfig,
        error::{ConfigDataError, JobError, StartComponentError, StartInfoError},
        runtime_dir::RuntimeDirBuilder,
        stdout::bind_streams_to_syslog,
    },
    crate::{crash_info::CrashRecords, vdso_vmo::get_next_vdso_vmo},
    ::routing::policy::ScopedPolicyChecker,
    async_trait::async_trait,
    chrono::{DateTime, NaiveDateTime, Utc},
    cm_runner::Runner,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_component as fcomp, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_diagnostics_types::{
        ComponentDiagnostics, ComponentTasks, Task as DiagnosticsTask,
    },
    fidl_fuchsia_mem as fmem, fidl_fuchsia_process as fproc,
    fidl_fuchsia_process_lifecycle::LifecycleMarker,
    fuchsia_async::{self as fasync, TimeoutExt},
    fuchsia_runtime::{duplicate_utc_clock_handle, job_default, HandleInfo, HandleType},
    fuchsia_zircon::{self as zx, AsHandleRef, HandleBased},
    futures::channel::oneshot,
    moniker::Moniker,
    runner::component::ChannelEpitaph,
    std::convert::TryFrom,
    std::{convert::TryInto, path::Path, sync::Arc},
    tracing::warn,
};

// Maximum time that the runner will wait for break_on_start eventpair to signal.
// This is set to prevent debuggers from blocking us for too long, either intentionally
// or unintentionally.
const MAX_WAIT_BREAK_ON_START: zx::Duration = zx::Duration::from_millis(300);

// Minimum timer slack amount and default mode. The amount should be large enough to allow for some
// coalescing of timers, but small enough to ensure applications don't miss deadlines.
//
// TODO(fxbug.dev/43934): For now, set the value to 50us to avoid delaying performance-critical
// timers in Scenic and other system services.
const TIMER_SLACK_DURATION: zx::Duration = zx::Duration::from_micros(50);

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
    launcher_connector: process_launcher::Connector,

    /// If `utc_clock` is populated then that Clock's handle will
    /// be passed into the newly created process. Otherwise, the UTC
    /// clock will be duplicated from current process' process table.
    /// The latter is typically the case in unit tests and nested
    /// component managers.
    utc_clock: Option<Arc<zx::Clock>>,

    crash_records: CrashRecords,
}

impl ElfRunner {
    pub fn new(
        launcher_connector: process_launcher::Connector,
        utc_clock: Option<Arc<zx::Clock>>,
        crash_records: CrashRecords,
    ) -> ElfRunner {
        ElfRunner { launcher_connector, utc_clock, crash_records }
    }

    /// Returns a UTC clock handle.
    ///
    /// Duplicates `self.utc_clock` if populated, or the UTC clock assigned to the current process.
    async fn duplicate_utc_clock(&self) -> Result<zx::Clock, zx::Status> {
        if let Some(utc_clock) = &self.utc_clock {
            utc_clock.duplicate_handle(DUPLICATE_CLOCK_RIGHTS)
        } else {
            duplicate_utc_clock_handle(DUPLICATE_CLOCK_RIGHTS)
        }
    }

    /// Creates a job for a component.
    fn create_job(program_config: &ElfProgramConfig) -> Result<zx::Job, JobError> {
        let job = job_default().create_child_job().map_err(JobError::CreateChild)?;

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

        Ok(job)
    }

    fn encoded_config_into_vmo(encoded_config: fmem::Data) -> Result<zx::Vmo, ConfigDataError> {
        match encoded_config {
            fmem::Data::Buffer(fmem::Buffer {
                vmo,
                size: _, // we get this vmo from component manager which sets the content size
            }) => Ok(vmo),
            fmem::Data::Bytes(bytes) => {
                let size = bytes.len() as u64;
                let vmo = zx::Vmo::create(size).map_err(ConfigDataError::VmoCreate)?;
                vmo.write(&bytes, 0).map_err(ConfigDataError::VmoWrite)?;
                Ok(vmo)
            }
            _ => Err(ConfigDataError::UnrecognizedDataVariant.into()),
        }
    }

    fn create_handle_infos(
        outgoing_dir: Option<zx::Channel>,
        lifecycle_server: Option<zx::Channel>,
        utc_clock: zx::Clock,
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
        let resolved_url = start_info
            .resolved_url
            .clone()
            .ok_or(StartComponentError::StartInfoError(StartInfoError::MissingResolvedUrl))?;

        // This also checks relevant security policy for config that it wraps using the provided
        // PolicyChecker.
        let program_config = start_info
            .program
            .as_ref()
            .map(|program| ElfProgramConfig::parse_and_check(&program, &checker))
            .ok_or(StartComponentError::StartInfoError(StartInfoError::MissingProgram))?
            .map_err(|err| {
                StartComponentError::StartInfoError(StartInfoError::ProgramError(err))
            })?;

        let url = resolved_url.clone();
        let main_process_critical = program_config.main_process_critical;
        let res: Result<ElfComponent, StartComponentError> = self
            .start_component_helper(start_info, checker.scope.clone(), resolved_url, program_config)
            .await;
        match res {
            Err(e) if main_process_critical => {
                panic!("failed to launch component with a critical process ({:?}): {:?}", url, e)
            }
            x => x,
        }
    }

    async fn start_component_helper(
        &self,
        mut start_info: fcrunner::ComponentStartInfo,
        moniker: Moniker,
        resolved_url: String,
        program_config: ElfProgramConfig,
    ) -> Result<ElfComponent, StartComponentError> {
        // Connect to `fuchsia.process.Launcher`.
        let launcher = self
            .launcher_connector
            .connect()
            .map_err(|err| StartComponentError::ProcessLauncherConnectError(err.into()))?;

        // Create a job for this component that will contain its process.
        let job = ElfRunner::create_job(&program_config).map_err(StartComponentError::JobError)?;

        crash_handler::run_exceptions_server(
            &job,
            moniker.clone(),
            resolved_url.clone(),
            self.crash_records.clone(),
        )
        .map_err(StartComponentError::ExceptionRegistrationFailed)?;

        // Convert the directories into proxies, so we can find "/pkg" and open "lib" and bin_path
        let ns = runner::component::ComponentNamespace::try_from(
            start_info.ns.take().unwrap_or_else(|| vec![]),
        )
        .map_err(StartComponentError::ComponentNamespaceError)?;

        let config_vmo = start_info
            .encoded_config
            .take()
            .map(ElfRunner::encoded_config_into_vmo)
            .transpose()
            .map_err(StartComponentError::ConfigDataError)?;

        let next_vdso = program_config
            .use_next_vdso
            .then(get_next_vdso_vmo)
            .transpose()
            .map_err(StartComponentError::VdsoError)?;

        let (lifecycle_client, lifecycle_server) = if program_config.notify_lifecycle_stop {
            // Creating a channel is not expected to fail.
            let (client, server) = fidl::endpoints::create_proxy::<LifecycleMarker>().unwrap();
            (Some(client), Some(server.into_channel()))
        } else {
            (None, None)
        };

        // Take the UTC clock handle out of `start_info.numbered_handles`, if available.
        let utc_handle = start_info
            .numbered_handles
            .as_mut()
            .map(|handles| {
                handles
                    .iter()
                    .position(|handles| {
                        handles.id == HandleInfo::new(HandleType::ClockUtc, 0).as_raw()
                    })
                    .map(|position| handles.swap_remove(position).handle)
            })
            .flatten();

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
        let job_koid = job.get_koid().map_err(StartComponentError::JobGetKoidFailed)?.raw_koid();

        let runtime_dir = RuntimeDirBuilder::new(runtime_dir_server_end)
            .args(program_config.args.clone())
            .job_id(job_koid)
            .serve();

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
        if let Some(handles) = start_info.numbered_handles {
            handle_infos.extend(handles);
        }

        // Configure the process launcher.
        let job_dup = job
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(StartComponentError::JobDuplicateFailed)?;

        let name = Path::new(&resolved_url)
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
                args: Some(program_config.args),
                ns,
                job: Some(job_dup),
                handle_infos: Some(handle_infos),
                name_infos: None,
                environs: program_config.environ,
                launcher: &launcher,
                loader_proxy_chan: None,
                executable_vmo: None,
            })
            .await
            .map_err(StartComponentError::ConfigureLauncherError)?;

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

        // Add process ID to the runtime dir.
        runtime_dir.add_process_id(
            process.get_koid().map_err(StartComponentError::ProcessGetKoidFailed)?.raw_koid(),
        );

        // Add process start time to the runtime dir.
        let process_start_time =
            process.info().map_err(StartComponentError::ProcessInfoFailed)?.start_time;
        runtime_dir.add_process_start_time(process_start_time);

        // Add UTC estimate of the process start time to the runtime dir.
        let utc_clock_started = fasync::OnSignals::new(&utc_clock_dup, zx::Signals::CLOCK_STARTED)
            .on_timeout(zx::Time::after(zx::Duration::default()), || Err(zx::Status::TIMED_OUT))
            .await
            .is_ok();
        let clock_transformation = utc_clock_started
            .then(|| utc_clock_dup.get_details().map(|details| details.mono_to_synthetic).ok())
            .flatten();
        if let Some(clock_transformation) = clock_transformation {
            let utc_timestamp =
                clock_transformation.apply(zx::Time::from_nanos(process_start_time)).into_nanos();
            let seconds = (utc_timestamp / 1_000_000_000) as i64;
            let nanos = (utc_timestamp % 1_000_000_000) as u32;
            let dt = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(seconds, nanos), Utc);
            runtime_dir.add_process_start_time_utc_estimate(dt.to_string())
        };

        Ok(ElfComponent::new(
            runtime_dir,
            job,
            process,
            lifecycle_client,
            program_config.main_process_critical,
            stdout_and_stderr_tasks,
            resolved_url.clone(),
        ))
    }

    pub fn get_scoped_runner(self: Arc<Self>, checker: ScopedPolicyChecker) -> Arc<dyn Runner> {
        Arc::new(ScopedElfRunner { runner: self, checker })
    }
}

struct ScopedElfRunner {
    runner: Arc<ElfRunner>,
    checker: ScopedPolicyChecker,
}

#[async_trait]
impl Runner for ScopedElfRunner {
    /// Starts a component by creating a new Job and Process for the component.
    /// The Runner also creates and hosts a namespace for the component. The
    /// namespace and other runtime state of the component will live until the
    /// Future returned is dropped or the `server_end` is sent either
    /// `ComponentController.Stop` or `ComponentController.Kill`. Sending
    /// `ComponentController.Stop` or `ComponentController.Kill` causes the
    /// Future to complete.
    async fn start(
        &self,
        start_info: fcrunner::ComponentStartInfo,
        server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
    ) {
        let resolved_url =
            start_info.resolved_url.clone().unwrap_or_else(|| "<unknown>".to_string());

        let elf_component = match self.runner.start_component(start_info, &self.checker).await {
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

        let (epitaph_tx, epitaph_rx) = oneshot::channel::<ChannelEpitaph>();
        // This function waits for something from the channel and
        // returns it or Error::Internal if the channel is closed
        let epitaph_fn = Box::pin(async move {
            epitaph_rx
                .await
                .unwrap_or_else(|_| {
                    warn!("epitaph oneshot channel closed unexpectedly");
                    fcomp::Error::Internal.into()
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

        // Spawn a future that watches for the process to exit
        fasync::Task::spawn(async move {
            fasync::OnSignals::new(&proc_copy.as_handle_ref(), zx::Signals::PROCESS_TERMINATED)
                .await
                .map(|_: fidl::Signals| ()) // Discard.
                .unwrap_or_else(|error| warn!(%error, "error creating signal handler"));
            // Process exit code '0' is considered a clean return.
            // TODO (fxbug.dev/57024) If we create an epitaph that indicates
            // intentional, non-zero exit, use that for all non-0 exit
            // codes.
            let exit_status: ChannelEpitaph = match proc_copy.info() {
                Ok(zx::ProcessInfo { return_code: 0, .. }) => zx::Status::OK.try_into().unwrap(),
                Ok(_) => fcomp::Error::InstanceDied.into(),
                Err(error) => {
                    warn!(%error, "Unable to query process info");
                    fcomp::Error::Internal.into()
                }
            };
            epitaph_tx.send(exit_status).unwrap_or_else(|_| warn!("error sending epitaph"));
        })
        .detach();

        // Create a future which owns and serves the controller
        // channel. The `epitaph_fn` future completes when the
        // component's main process exits. The controller then sets the
        // epitaph on the controller channel, closes it, and stops
        // serving the protocol.
        fasync::Task::spawn(async move {
            let (server_stream, control) = match server_end.into_stream_and_control_handle() {
                Ok(s) => s,
                Err(error) => {
                    warn!(%error, "Converting Controller channel to stream failed");
                    return;
                }
            };
            if let Some(component_diagnostics) = component_diagnostics {
                control.send_on_publish_diagnostics(component_diagnostics).unwrap_or_else(
                    |error| warn!(url=%resolved_url, %error, "sending diagnostics failed"),
                );
            }
            runner::component::Controller::new(elf_component, server_stream)
                .serve(epitaph_fn)
                .await
                .unwrap_or_else(|error| warn!(%error, "serving ComponentController"));
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use {
        super::runtime_dir::RuntimeDirectory,
        super::*,
        ::routing::{
            config::{AllowlistEntryBuilder, JobPolicyAllowlists, SecurityPolicy},
            policy::ScopedPolicyChecker,
        },
        anyhow::{Context, Error},
        assert_matches::assert_matches,
        fidl::endpoints::{
            create_endpoints, create_proxy, spawn_stream_handler, ClientEnd,
            DiscoverableProtocolMarker, Proxy, ServerEnd,
        },
        fidl_connector::Connect,
        fidl_fuchsia_component as fcomp, fidl_fuchsia_component_runner as fcrunner,
        fidl_fuchsia_data as fdata,
        fidl_fuchsia_diagnostics_types::{
            ComponentDiagnostics, ComponentTasks, Task as DiagnosticsTask,
        },
        fidl_fuchsia_io as fio,
        fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequest, LogSinkRequestStream},
        fidl_fuchsia_process_lifecycle::{LifecycleMarker, LifecycleProxy},
        fuchsia_async as fasync,
        fuchsia_component::server::{ServiceFs, ServiceObjLocal},
        fuchsia_fs,
        fuchsia_zircon::{self as zx, AsHandleRef, Task},
        futures::{channel::mpsc, join, lock::Mutex, StreamExt, TryStreamExt},
        moniker::{Moniker, MonikerBase},
        runner::component::Controllable,
        scoped_task,
        std::{convert::TryFrom, sync::Arc, task::Poll},
    };

    pub enum MockServiceRequest {
        LogSink(LogSinkRequestStream),
    }

    pub type MockServiceFs<'a> = ServiceFs<ServiceObjLocal<'a, MockServiceRequest>>;

    /// Create a new local fs and install a mock LogSink service into.
    /// Returns the created directory and corresponding namespace entries.
    pub fn create_fs_with_mock_logsink(
    ) -> Result<(MockServiceFs<'static>, Vec<fcrunner::ComponentNamespaceEntry>), Error> {
        let (client, server) = create_endpoints::<fio::DirectoryMarker>();
        let mut dir = ServiceFs::new_local();
        dir.add_fidl_service_at(LogSinkMarker::PROTOCOL_NAME, MockServiceRequest::LogSink);
        dir.serve_connection(server).context("Failed to add serving channel.")?;
        let entries = vec![fcrunner::ComponentNamespaceEntry {
            path: Some("/svc".to_string()),
            directory: Some(client),
            ..Default::default()
        }];

        Ok((dir, entries))
    }

    fn new_elf_runner_for_test() -> Arc<ElfRunner> {
        Arc::new(ElfRunner::new(
            Box::new(process_launcher::BuiltInConnector {}),
            None,
            CrashRecords::new(),
        ))
    }

    fn hello_world_startinfo(
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> fcrunner::ComponentStartInfo {
        // Get a handle to /pkg
        let pkg_path = "/pkg".to_string();
        let pkg_chan = fuchsia_fs::directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .unwrap()
        .into_channel()
        .unwrap()
        .into_zx_channel();
        let pkg_handle = ClientEnd::new(pkg_chan);

        let ns = vec![fcrunner::ComponentNamespaceEntry {
            path: Some(pkg_path),
            directory: Some(pkg_handle),
            ..Default::default()
        }];

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
            ..Default::default()
        }
    }

    /// ComponentStartInfo that points to a non-existent binary.
    fn invalid_binary_startinfo(
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> fcrunner::ComponentStartInfo {
        // Get a handle to /pkg
        let pkg_path = "/pkg".to_string();
        let pkg_chan = fuchsia_fs::directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .unwrap()
        .into_channel()
        .unwrap()
        .into_zx_channel();
        let pkg_handle = ClientEnd::new(pkg_chan);

        let ns = vec![fcrunner::ComponentNamespaceEntry {
            path: Some(pkg_path),
            directory: Some(pkg_handle),
            ..Default::default()
        }];

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
            ..Default::default()
        }
    }

    /// Creates start info for a component which runs until told to exit. The
    /// ComponentController protocol can be used to stop the component when the
    /// test is done inspecting the launched component.
    fn lifecycle_startinfo(
        runtime_dir: ServerEnd<fio::DirectoryMarker>,
    ) -> fcrunner::ComponentStartInfo {
        // Get a handle to /pkg
        let pkg_path = "/pkg".to_string();
        let pkg_chan = fuchsia_fs::directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .unwrap()
        .into_channel()
        .unwrap()
        .into_zx_channel();
        let pkg_handle = ClientEnd::new(pkg_chan);

        let ns = vec![fcrunner::ComponentNamespaceEntry {
            path: Some(pkg_path),
            directory: Some(pkg_handle),
            ..Default::default()
        }];

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
            job_copy,
            process,
            lifecycle_client,
            critical,
            Vec::new(),
            "".to_string(),
        );
        (job, component)
    }

    // TODO(fxbug.dev/122225): A variation of this is used in a couple of places. We should consider
    // refactoring this into a test util file.
    async fn read_file<'a>(root_proxy: &'a fio::DirectoryProxy, path: &'a str) -> String {
        let file_proxy = fuchsia_fs::directory::open_file_no_describe(
            &root_proxy,
            path,
            fuchsia_fs::OpenFlags::RIGHT_READABLE,
        )
        .expect("Failed to open file.");
        let res = fuchsia_fs::file::read_to_string(&file_proxy).await;
        res.expect("Unable to read file.")
    }

    #[fuchsia::test]
    async fn test_runtime_dir_entries() -> Result<(), Error> {
        let (runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>()?;
        let start_info = lifecycle_startinfo(runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>()
            .expect("could not create component controller endpoints");

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
        let (job, component) = make_default_elf_component(None, false);

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
        let (lifecycle_client, _lifecycle_server) = create_proxy::<LifecycleMarker>()?;
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
        let (lifecycle_client, lifecycle_server) = create_proxy::<LifecycleMarker>()?;
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
        let (lifecycle_client, lifecycle_server) = create_proxy::<LifecycleMarker>()?;
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
        let (lifecycle_client, lifecycle_server) = create_proxy::<LifecycleMarker>()?;
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
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>()
            .expect("could not create component controller endpoints");

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
        let (runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>()?;
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
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>()
            .expect("could not create component controller endpoints");
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
        expect_channel_closed(
            &mut event_stream,
            zx::Status::from_raw(
                i32::try_from(fcomp::Error::InstanceDied.into_primitive()).unwrap(),
            ),
        )
        .await;
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
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>()
            .expect("could not create component controller endpoints");

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
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>()
            .expect("could not create component controller endpoints");

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
        let pkg_path = "/pkg".to_string();
        let pkg_chan = fuchsia_fs::directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .unwrap()
        .into_channel()
        .unwrap()
        .into_zx_channel();
        let pkg_handle = ClientEnd::new(pkg_chan);

        ns.push(fcrunner::ComponentNamespaceEntry {
            path: Some(pkg_path),
            directory: Some(pkg_handle),
            ..Default::default()
        });

        fcrunner::ComponentStartInfo {
            resolved_url: Some(
                "fuchsia-pkg://fuchsia.com/hello-world#meta/hello-world-rust.cm".to_string(),
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
            ..Default::default()
        }
    }

    // TODO(fxbug.dev/69634): Following function shares a lot of code with
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
                create_proxy::<fcrunner::ComponentControllerMarker>()
                    .expect("could not create component controller endpoints");

            runner.start(start_info, server_controller).await;
            let mut event_stream = client_controller.take_event_stream();
            expect_diagnostics_event(&mut event_stream).await;
            expect_channel_closed(&mut event_stream, zx::Status::OK).await;
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
        let (runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>()?;
        let start_info = lifecycle_startinfo(runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::root(),
        ));
        let (controller, server_controller) = create_proxy::<fcrunner::ComponentControllerMarker>()
            .expect("could not create component controller endpoints");

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

    async fn expect_channel_closed(
        event_stream: &mut fcrunner::ComponentControllerEventStream,
        expected_status: zx::Status,
    ) {
        let event = event_stream.try_next().await;
        match event {
            Err(fidl::Error::ClientChannelClosed { status, .. }) => {
                assert_eq!(status, expected_status);
            }
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

            spawn_stream_handler(move |launcher_request| {
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
            })
            .map_err(anyhow::Error::new)
        }
    }

    #[fuchsia::test]
    async fn process_created_with_utc_clock_from_numbered_handles() -> Result<(), Error> {
        let (payload_tx, mut payload_rx) = mpsc::unbounded();

        let connector = LauncherConnectorForTest { sender: payload_tx };
        let runner = ElfRunner::new(Box::new(connector), None, CrashRecords::new());
        let policy_checker = ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::try_from(vec!["foo"]).unwrap(),
        );

        // Create a clock and pass it to the component as the UTC clock through numbered_handles.
        let clock = zx::Clock::create(zx::ClockOpts::AUTO_START | zx::ClockOpts::MONOTONIC, None)?;
        let clock_koid = clock.get_koid().unwrap();

        let (_runtime_dir, runtime_dir_server) = create_proxy::<fio::DirectoryMarker>()?;
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
}
