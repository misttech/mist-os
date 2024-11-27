// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ::routing::capability_source::InternalCapability;
use async_trait::async_trait;
use cm_config::SecurityPolicy;
use cm_types::Name;
use cm_util::TaskGroup;
use elf_runner::crash_info::CrashRecords;
use elf_runner::process_launcher::NamespaceConnector;
use fidl::endpoints;
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, RequestStream, ServerEnd};
use fidl_fuchsia_data::Dictionary;
use fuchsia_runtime::UtcClock;

use cm_util::{AbortHandle, AbortableScope};
use futures::future::{BoxFuture, Shared};
use futures::{Future, FutureExt, TryStreamExt};
use namespace::{Namespace, NamespaceError};
use routing::capability_source::{BuiltinSource, CapabilitySource};
use routing::policy::ScopedPolicyChecker;
use runner::component::{Controllable, Controller, StopInfo};
use sandbox::{Capability, Dict, DirEntry, RemotableCapability};
use std::sync::Arc;
use thiserror::Error;
use tracing::{error, warn};
use vfs::directory::entry::OpenRequest;
use vfs::execution_scope::ExecutionScope;
use vfs::service::endpoint;
use zx::{AsHandleRef, HandleBased, Task};
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_io as fio, fidl_fuchsia_memory_attribution as fattribution,
    fidl_fuchsia_process as fprocess, fidl_fuchsia_process_lifecycle as fprocess_lifecycle,
    fuchsia_async as fasync,
};

use crate::builtin::runner::BuiltinRunnerFactory;
use crate::model::component::WeakComponentInstance;
use crate::model::token::{InstanceRegistry, InstanceToken};
use crate::sandbox_util;
use crate::sandbox_util::LaunchTaskOnReceive;

const SVC: &str = "svc";

pub type BuiltinProgramGen = Box<dyn Fn() -> BuiltinProgramFn + Send + Sync + 'static>;

/// The builtin runner runs a component implemented inside component_manager.
///
/// Builtin components are still defined by a declaration. Each builtin runner can
/// run only a single program. For example, the "builtin_elf_runner" runs the elf
/// runner component.
///
/// When bootstrapping the system, builtin components may be resolved by the builtin URL
/// scheme, e.g. fuchsia-builtin://#elf_runner.cm. However, it's entirely possible to resolve
/// a builtin component via other schemes. A component is a builtin component if and only
/// if it uses the builtin runner.
pub struct BuiltinRunner {
    root_job: zx::Unowned<'static, zx::Job>,
    task_group: TaskGroup,
    program: BuiltinProgramGen,
}

/// Pure data type holding some resources needed by the ELF runner.
// TODO(https://fxbug.dev/318697539): Most of this should be replaced by
// capabilities in the incoming namespace of the ELF runner component.
pub struct ElfRunnerResources {
    /// Job policy requests in the program block of ELF components will be checked against
    /// the provided security policy.
    pub security_policy: Arc<SecurityPolicy>,
    pub utc_clock: Option<Arc<UtcClock>>,
    pub crash_records: CrashRecords,
    pub instance_registry: Arc<InstanceRegistry>,
}

#[derive(Debug, Error)]
enum BuiltinRunnerError {
    #[error("missing outgoing_dir in StartInfo")]
    MissingOutgoingDir,

    #[error("missing ns in StartInfo")]
    MissingNamespace,

    #[error("namespace error: {0}")]
    NamespaceError(#[from] NamespaceError),

    #[error("\"program\" has an illegal field or value")]
    IllegalProgram,

    #[error("cannot create job: {}", _0)]
    JobCreation(zx_status::Status),
}

impl From<BuiltinRunnerError> for zx::Status {
    fn from(value: BuiltinRunnerError) -> Self {
        match value {
            BuiltinRunnerError::MissingOutgoingDir
            | BuiltinRunnerError::MissingNamespace
            | BuiltinRunnerError::NamespaceError(_)
            | BuiltinRunnerError::IllegalProgram => {
                zx::Status::from_raw(fcomponent::Error::InvalidArguments.into_primitive() as i32)
            }
            BuiltinRunnerError::JobCreation(status) => status,
        }
    }
}

impl BuiltinRunner {
    /// Creates a builtin runner with its required resources.
    /// - `task_group`: The tasks associated with the builtin runner.
    /// - `program': The program that this builtin runner can run.
    ///   Each builtin runner can only run a single program.
    pub fn new(
        root_job: zx::Unowned<'static, zx::Job>,
        task_group: TaskGroup,
        program: BuiltinProgramGen,
    ) -> Self {
        Self { root_job, task_group, program }
    }

    /// Starts a builtin component.
    fn start(
        self: Arc<BuiltinRunner>,
        mut start_info: fcrunner::ComponentStartInfo,
    ) -> Result<(BuiltinProgram, impl Future<Output = StopInfo> + Unpin), BuiltinRunnerError> {
        let outgoing_dir =
            start_info.outgoing_dir.take().ok_or(BuiltinRunnerError::MissingOutgoingDir)?;
        let namespace: Namespace =
            start_info.ns.take().ok_or(BuiltinRunnerError::MissingNamespace)?.try_into()?;
        let main_process_critical =
            runner::get_program_string(&start_info, "main_process_critical").unwrap_or("false");
        let root_job_critical = match main_process_critical {
            "true" => Some(self.root_job.clone()),
            "false" => None,
            _ => return Err(BuiltinRunnerError::IllegalProgram),
        };
        let program_section: Option<Dictionary> = start_info.program.take();

        let job = self.root_job.create_child_job().map_err(BuiltinRunnerError::JobCreation)?;
        let job2 = job.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
        let (lifecycle_client, lifecycle_server) =
            endpoints::create_proxy::<fprocess_lifecycle::LifecycleMarker>();
        let body: BuiltinProgramFn = (self.program)();
        let program = BuiltinProgram::new(
            body,
            job,
            lifecycle_client,
            root_job_critical,
            namespace,
            outgoing_dir,
            lifecycle_server,
            program_section,
        );
        Ok((program, Box::pin(wait_for_job_termination(job2))))
    }

    pub fn get_elf_program(elf_runner_resources: Arc<ElfRunnerResources>) -> BuiltinProgramGen {
        Box::new(move || {
            let elf_runner_resources = elf_runner_resources.clone();
            Box::new(
                move |job: zx::Job,
                      namespace: Namespace,
                      outgoing_dir: ServerEnd<fio::DirectoryMarker>,
                      lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
                      _program: Option<Dictionary>| {
                    async move {
                        let program = ElfRunnerProgram::new(job, namespace, elf_runner_resources);
                        program.serve_outgoing(outgoing_dir);
                        program.wait_for_shutdown(lifecycle_server).await;
                    }
                    .boxed()
                },
            )
        })
    }

    pub fn get_devfs_program() -> BuiltinProgramGen {
        Box::new(|| {
            Box::new(
                move |_job: zx::Job,
                      namespace: Namespace,
                      outgoing_dir: ServerEnd<fio::DirectoryMarker>,
                      lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
                      _program: Option<Dictionary>| {
                    async move {
                        let ns_entries: Vec<fprocess::NameInfo> = namespace.into();
                        let res = devfs::main(ns_entries, outgoing_dir, lifecycle_server).await;
                        if let Err(e) = res {
                            error!("[devfs] {e}");
                        }
                    }
                    .boxed()
                },
            )
        })
    }

    pub fn get_shutdown_shim_program() -> BuiltinProgramGen {
        Box::new(|| {
            Box::new(
                move |_job: zx::Job,
                      namespace: Namespace,
                      outgoing_dir: ServerEnd<fio::DirectoryMarker>,
                      lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
                      _program: Option<Dictionary>| {
                    async move {
                        let _lifecycle_server = lifecycle_server;
                        let ns_entries: Vec<fprocess::NameInfo> = namespace.into();
                        let Some(svc) = ns_entries.into_iter().find_map(|e| {
                            if e.path == "/svc" {
                                Some(e.directory.into_proxy())
                            } else {
                                None
                            }
                        }) else {
                            error!("[shutdown-shim] no /svc in namespace");
                            return;
                        };
                        let res = shutdown_shim::main(svc, outgoing_dir).await;
                        if let Err(e) = res {
                            error!("[shutdown-shim] {e}");
                        }
                    }
                    .boxed()
                },
            )
        })
    }

    pub fn get_service_broker_program() -> BuiltinProgramGen {
        Box::new(|| {
            Box::new(
                move |_job: zx::Job,
                      namespace: Namespace,
                      outgoing_dir: ServerEnd<fio::DirectoryMarker>,
                      lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
                      program: Option<Dictionary>| {
                    async move {
                        let ns_entries: Vec<fprocess::NameInfo> = namespace.into();
                        let res = service_broker::main(
                            ns_entries,
                            outgoing_dir,
                            lifecycle_server,
                            program,
                        )
                        .await;
                        if let Err(e) = res {
                            error!("[service-broker] {e}");
                        }
                    }
                    .boxed()
                },
            )
        })
    }
}

/// Waits for the job used by a builtin component to terminate, and translate the return code to an
/// epitaph.
///
/// Normally, the job will terminate when the builtin runner requests to stop the ELF runner.
/// We'll observe the asynchronous termination here and consider the ELF runner stopped.
async fn wait_for_job_termination(job: zx::Job) -> StopInfo {
    fasync::OnSignals::new(&job.as_handle_ref(), zx::Signals::JOB_TERMINATED)
        .await
        .map(|_: fidl::Signals| ())
        .unwrap_or_else(|error| warn!(%error, "error waiting for job termination"));

    use fidl_fuchsia_component::Error;
    let exit_status = match job.info() {
        Ok(zx::JobInfo { return_code: zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL, .. }) => {
            // Stopping the ELF runner will destroy the job, so this is the only
            // normal exit code path.
            StopInfo::from_ok(None)
        }
        Ok(zx::JobInfo { return_code, .. }) => {
            warn!(%return_code, "job terminated with abnormal return code");
            StopInfo::from_error(Error::InstanceDied, None)
        }
        Err(error) => {
            warn!(%error, "Unable to query job info");
            StopInfo::from_error(Error::Internal, None)
        }
    };
    exit_status
}

impl BuiltinRunnerFactory for BuiltinRunner {
    fn get_scoped_runner(
        self: Arc<Self>,
        _checker: ScopedPolicyChecker,
        open_request: OpenRequest<'_>,
    ) -> Result<(), zx::Status> {
        open_request.open_service(endpoint(move |_scope, server_end| {
            let runner = self.clone();
            let mut stream = fcrunner::ComponentRunnerRequestStream::from_channel(server_end);
            runner.clone().task_group.spawn(async move {
                while let Ok(Some(request)) = stream.try_next().await {
                    match request {
                        fcrunner::ComponentRunnerRequest::Start {
                            start_info, controller, ..
                        } => match runner.clone().start(start_info) {
                            Ok((program, on_exit)) => {
                                let (stream, control) = controller.into_stream_and_control_handle();
                                let controller = Controller::new(program, stream, control);
                                runner.task_group.spawn(controller.serve(on_exit));
                            }
                            Err(err) => {
                                warn!("Builtin runner failed to run component: {err}");
                                let _ = controller.close_with_epitaph(err.into());
                            }
                        },
                        fcrunner::ComponentRunnerRequest::_UnknownMethod { ordinal, .. } => {
                            warn!(%ordinal, "Unknown ComponentRunner request");
                        }
                    }
                }
            });
        }))
    }
}

/// The program state of a builtin component running in the builtin runner.
struct BuiltinProgram {
    lifecycle_client: Option<fprocess_lifecycle::LifecycleProxy>,
    main_process_critical: bool,
    task: Shared<BoxFuture<'static, ()>>,
    task_abortable: AbortHandle,
}

type BuiltinProgramFn = Box<
    dyn FnOnce(
            zx::Job,
            Namespace,
            ServerEnd<fio::DirectoryMarker>,
            ServerEnd<fprocess_lifecycle::LifecycleMarker>,
            Option<Dictionary>,
        ) -> BoxFuture<'static, ()>
        + Send
        + 'static,
>;

impl BuiltinProgram {
    fn new(
        body: BuiltinProgramFn,
        job: zx::Job,
        lifecycle_client: fprocess_lifecycle::LifecycleProxy,
        root_job_if_critical: Option<zx::Unowned<'static, zx::Job>>,
        namespace: Namespace,
        outgoing_dir: ServerEnd<fio::DirectoryMarker>,
        lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
        program: Option<Dictionary>,
    ) -> Self {
        let (abort_scope, task_abort) = AbortableScope::new();
        let main_process_critical = root_job_if_critical.is_some();
        let job2 = job.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
        // Kill the job at the end of this task. Ensure the job is killed if this task is
        // dropped before completion.
        //
        // We don't expect the runner to drop this task before it completes, but there's no
        // cost to being defensive.
        struct Finalize {
            job: zx::Job,
            root_job_if_critical: Option<zx::Unowned<'static, zx::Job>>,
        }
        impl Drop for Finalize {
            fn drop(&mut self) {
                if let Some(j) = self.root_job_if_critical.as_ref() {
                    _ = j.kill();
                }
                _ = self.job.kill();
            }
        }
        let f = Finalize { job, root_job_if_critical };
        let task = fasync::Task::spawn(async move {
            let _f = f;
            _ = abort_scope
                .run(body(job2, namespace, outgoing_dir, lifecycle_server, program))
                .await;
        })
        .boxed()
        .shared();
        Self {
            lifecycle_client: Some(lifecycle_client),
            main_process_critical,
            task,
            task_abortable: task_abort,
        }
    }
}

#[async_trait]
impl Controllable for BuiltinProgram {
    async fn kill(&mut self) {
        self.task_abortable.abort();
        self.task.clone().await;
    }

    fn stop<'a>(&mut self) -> BoxFuture<'a, ()> {
        let main_process_critical = self.main_process_critical;
        let lifecycle_client = self.lifecycle_client.take();
        let task_abortable = self.task_abortable.clone();
        let task = self.task.clone();
        match lifecycle_client {
            Some(lifecycle) => {
                let _ = lifecycle.stop();
                async move {
                    lifecycle
                    .on_closed()
                    .await
                    .map(|_: fidl::Signals| ()) // Discard.
                    .unwrap_or_else(|err| {
                        warn!(
                            %err,
                            "killing builtin component after failure waiting on lifecycle channel"
                        )
                    });
                    if !main_process_critical {
                        task_abortable.abort();
                    } else {
                        // To mimic the behavior of elf_runner, if main_process_critical is set,
                        // wait for the program's task to exit before signaling stop completed.
                        // The task will kill the job at the end, so we don't need to do it.
                        // ourselves.
                    }
                    // The task will kill the job at the end.
                    task.await;
                }
                .boxed()
            }
            None => {
                // Duplicate stop request?
                warn!("builtin runner received duplicate Stop for component");
                async {}.boxed()
            }
        }
    }
}

/// The program of the ELF runner component.
struct ElfRunnerProgram {
    task_group: TaskGroup,
    execution_scope: ExecutionScope,
    output: Dict,
    job: zx::Job,
}

struct Inner {
    resources: Arc<ElfRunnerResources>,
    elf_runner: Arc<elf_runner::ElfRunner>,
}

impl ElfRunnerProgram {
    /// Creates an ELF runner program.
    /// - `job`: Each ELF component run by this runner will live inside a job that is a
    ///   child of the provided job.
    fn new(job: zx::Job, namespace: Namespace, resources: Arc<ElfRunnerResources>) -> Self {
        let namespace = Arc::new(namespace);
        let connector = NamespaceConnector { namespace: namespace };
        let elf_runner = elf_runner::ElfRunner::new(
            job.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
            Box::new(connector),
            resources.utc_clock.clone(),
            resources.crash_records.clone(),
        );
        let inner = Arc::new(Inner { resources, elf_runner: Arc::new(elf_runner) });

        let task_group = TaskGroup::new();

        let inner_clone = inner.clone();
        let elf_runner = Arc::new(LaunchTaskOnReceive::new(
            CapabilitySource::Builtin(BuiltinSource {
                capability: InternalCapability::Runner(Name::new("elf").unwrap()),
            }),
            task_group.as_weak(),
            fcrunner::ComponentRunnerMarker::PROTOCOL_NAME,
            None,
            Arc::new(move |server_end, _| {
                inner_clone
                    .clone()
                    .serve_component_runner(sandbox_util::take_handle_as_stream::<
                        fcrunner::ComponentRunnerMarker,
                    >(server_end))
                    .boxed()
            }),
        ));

        let inner_clone = inner;
        let snapshot_provider = Arc::new(LaunchTaskOnReceive::new(
            CapabilitySource::Builtin(BuiltinSource {
                capability: InternalCapability::Protocol(
                    Name::new(fattribution::ProviderMarker::PROTOCOL_NAME).unwrap(),
                ),
            }),
            task_group.as_weak(),
            fattribution::ProviderMarker::PROTOCOL_NAME,
            None,
            Arc::new(move |server_end, _| {
                inner_clone.clone().elf_runner.serve_memory_reporter(
                    sandbox_util::take_handle_as_stream::<fattribution::ProviderMarker>(server_end),
                );
                std::future::ready(Result::<(), anyhow::Error>::Ok(())).boxed()
            }),
        ));
        let output = Dict::new();
        let svc = {
            let svc = Dict::new();
            svc.insert(
                fcrunner::ComponentRunnerMarker::PROTOCOL_NAME.parse().unwrap(),
                elf_runner.into_sender(WeakComponentInstance::invalid()).into(),
            )
            .ok();
            svc.insert(
                fattribution::ProviderMarker::PROTOCOL_NAME.parse().unwrap(),
                snapshot_provider.into_sender(WeakComponentInstance::invalid()).into(),
            )
            .ok();
            svc
        };
        output.insert(SVC.parse().unwrap(), Capability::Dictionary(svc)).ok();

        let this = Self { task_group, execution_scope: ExecutionScope::new(), output, job };
        this
    }

    /// Serves requests coming from `outgoing_dir` using `self.output`.
    fn serve_outgoing(&self, outgoing_dir: ServerEnd<fio::DirectoryMarker>) {
        let output = self.output.clone();
        let dir_entry =
            DirEntry::new(output.try_into_directory_entry(self.execution_scope.clone()).unwrap());
        dir_entry.open(
            self.execution_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            ".".to_string(),
            outgoing_dir.into_channel(),
        );
    }

    async fn wait_for_shutdown(self, lifecycle: ServerEnd<fprocess_lifecycle::LifecycleMarker>) {
        let mut stream = lifecycle.into_stream();
        #[allow(clippy::never_loop)]
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fprocess_lifecycle::LifecycleRequest::Stop { .. } => {
                    break;
                }
            }
        }
        let task_group = self.task_group.clone();
        drop(self);
        task_group.join().await;
    }
}

// If `Stop` timed out and the component was killed, this will ensure it gets torn down properly.
impl Drop for ElfRunnerProgram {
    fn drop(&mut self) {
        _ = self.job.kill();
        self.execution_scope.shutdown();
    }
}

impl Inner {
    async fn serve_component_runner(
        self: Arc<Self>,
        mut stream: fcrunner::ComponentRunnerRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fcrunner::ComponentRunnerRequest::Start { mut start_info, controller, .. } => {
                    let Some(token) = start_info.component_instance.take() else {
                        warn!(
                            "When calling the ComponentRunner protocol of an ELF runner, \
                            one must provide the ComponentStartInfo.component_instance field."
                        );
                        _ = controller.close_with_epitaph(zx::Status::INVALID_ARGS);
                        continue;
                    };
                    let token = InstanceToken::from(token);
                    let Some(target_moniker) = self.resources.instance_registry.get(&token) else {
                        warn!(
                            "The provided ComponentStartInfo.component_instance token is invalid. \
                            The component has either already been destroyed, or the token is not \
                            minted by component_manager."
                        );
                        _ = controller.close_with_epitaph(zx::Status::NOT_SUPPORTED);
                        continue;
                    };
                    start_info.component_instance = Some(token.into());
                    let checker = ScopedPolicyChecker::new(
                        self.resources.security_policy.clone(),
                        target_moniker.clone(),
                    );
                    self.elf_runner
                        .clone()
                        .get_scoped_runner(checker)
                        .start(start_info, controller)
                        .await;
                }
                fcrunner::ComponentRunnerRequest::_UnknownMethod { ordinal, .. } => {
                    warn!(%ordinal, "Unknown ComponentRunner request");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use cm_types::NamespacePath;
    use fcrunner::{ComponentNamespaceEntry, ComponentStartInfo};
    use fidl::endpoints::ClientEnd;
    use fidl_fuchsia_data::{DictionaryEntry, DictionaryValue};
    use fidl_fuchsia_io::{self as fio, DirectoryProxy};
    use fidl_fuchsia_process as fprocess;
    use fuchsia_async::TestExecutor;
    use fuchsia_fs::directory::open_channel_in_namespace;
    use fuchsia_runtime::{HandleInfo, HandleType};
    use futures::channel::{self, oneshot};
    use moniker::Moniker;
    use sandbox::Directory;
    use serve_processargs::NamespaceBuilder;
    use std::pin::pin;
    use std::sync::LazyLock;
    use std::task::Poll;
    use test_case::test_case;
    use vfs::path::Path as VfsPath;
    use vfs::ToObjectRequest;

    use crate::bedrock::program::{Program, StartInfo};
    use crate::model::escrow::EscrowedState;
    use crate::runner::RemoteRunner;

    use super::*;

    fn make_security_policy() -> Arc<SecurityPolicy> {
        Arc::new(Default::default())
    }

    fn make_scoped_policy_checker() -> ScopedPolicyChecker {
        ScopedPolicyChecker::new(make_security_policy(), Moniker::new(vec![]))
    }

    fn make_elf_runner_resources() -> Arc<ElfRunnerResources> {
        let security_policy = make_security_policy();
        let crash_records = CrashRecords::new();
        let instance_registry = InstanceRegistry::new();
        Arc::new(ElfRunnerResources {
            security_policy,
            utc_clock: None,
            crash_records,
            instance_registry,
        })
    }

    fn make_builtin_runner(
        root_job: zx::Unowned<'static, zx::Job>,
        program: BuiltinProgramGen,
    ) -> Arc<BuiltinRunner> {
        let task_group = TaskGroup::new();
        Arc::new(BuiltinRunner::new(root_job, task_group, program))
    }

    fn make_start_info(
        svc_dir: ClientEnd<fio::DirectoryMarker>,
        main_process_critical: bool,
    ) -> (ComponentStartInfo, DirectoryProxy) {
        let (outgoing_dir, outgoing_server_end) = fidl::endpoints::create_proxy();
        let mut start_info = ComponentStartInfo {
            resolved_url: Some("fuchsia-builtin://elf_runner.cm".to_string()),
            program: Some(Dictionary { entries: Some(vec![]), ..Default::default() }),
            ns: Some(vec![ComponentNamespaceEntry {
                path: Some("/svc".to_string()),
                directory: Some(svc_dir),
                ..Default::default()
            }]),
            outgoing_dir: Some(outgoing_server_end),
            runtime_dir: None,
            numbered_handles: None,
            encoded_config: None,
            break_on_start: None,
            ..Default::default()
        };
        if main_process_critical {
            start_info.program.as_mut().unwrap().entries.as_mut().unwrap().push(DictionaryEntry {
                key: "main_process_critical".into(),
                value: Some(Box::new(DictionaryValue::Str("true".into()))),
            });
        }
        (start_info, outgoing_dir)
    }

    fn make_test_exit_immediately() -> BuiltinProgramGen {
        Box::new(|| {
            Box::new(
                move |_job: zx::Job,
                      _namespace: Namespace,
                      _outgoing_dir: ServerEnd<fio::DirectoryMarker>,
                      lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
                      _program: Option<Dictionary>| {
                    async move {
                        let _lifecycle_server = lifecycle_server;
                    }
                    .boxed()
                },
            )
        })
    }

    fn make_test_watch_lifecycle() -> BuiltinProgramGen {
        Box::new(|| {
            Box::new(
                move |_job: zx::Job,
                      _namespace: Namespace,
                      _outgoing_dir: ServerEnd<fio::DirectoryMarker>,
                      lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
                      _program: Option<Dictionary>| {
                    async move {
                        let mut stream = lifecycle_server.into_stream();
                        #[allow(clippy::never_loop)]
                        while let Ok(Some(request)) = stream.try_next().await {
                            match request {
                                fprocess_lifecycle::LifecycleRequest::Stop { .. } => {
                                    break;
                                }
                            }
                        }
                        drop(stream);
                        // We'd like to test what happens when the program continues running
                        // after it's dropped its lifecycle channel.
                        std::future::pending::<()>().await;
                    }
                    .boxed()
                },
            )
        })
    }

    fn make_test_hang() -> BuiltinProgramGen {
        Box::new(|| {
            Box::new(
                move |_job: zx::Job,
                      _namespace: Namespace,
                      _outgoing_dir: ServerEnd<fio::DirectoryMarker>,
                      lifecycle_server: ServerEnd<fprocess_lifecycle::LifecycleMarker>,
                      _program: Option<Dictionary>| {
                    async move {
                        let _lifecycle_server = lifecycle_server;
                        std::future::pending::<()>().await;
                    }
                    .boxed()
                },
            )
        })
    }

    #[test_case(false; "non-critical")]
    #[test_case(true; "critical")]
    #[fuchsia::test]
    async fn builtin_component_exit(main_process_critical: bool) {
        static ROOT_JOB: LazyLock<zx::Job> =
            LazyLock::new(|| fuchsia_runtime::job_default().create_child_job().unwrap());
        let root_job_handle = ROOT_JOB.as_handle_ref().cast();

        let builtin_runner = make_builtin_runner(root_job_handle, make_test_exit_immediately());
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fcrunner::ComponentRunnerMarker>();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        builtin_runner
            .clone()
            .get_scoped_runner(
                make_scoped_policy_checker(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    VfsPath::dot(),
                    &mut object_request,
                ),
            )
            .unwrap();
        let (component_controller, server_end) = fidl::endpoints::create_proxy();

        // Start the "test" component.
        let (svc, svc_server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace("/svc", fio::PERM_READABLE, svc_server_end).unwrap();
        let (start_info, _outgoing_dir) = make_start_info(svc, main_process_critical);
        client.start(start_info, server_end).unwrap();

        // The test component exits immediately, so we should observe a stop event on the
        // controller channel.
        let mut stream = component_controller.take_event_stream();
        let event = stream.try_next().await;
        assert_matches!(
            event,
            Ok(Some(fcrunner::ComponentControllerEvent::OnStop {
                payload: fcrunner::ComponentStopInfo {
                    termination_status: Some(0),
                    exit_code: None,
                    ..
                }
            }))
        );
        let event = stream.try_next().await;
        assert_matches!(event, Ok(None));

        if main_process_critical {
            assert_matches!(
                ROOT_JOB.info(),
                Ok(zx::JobInfo {
                    exited: true,
                    return_code: zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL,
                    ..
                })
            );
        } else {
            assert_matches!(ROOT_JOB.info(), Ok(zx::JobInfo { exited: false, .. }));
        }
    }

    #[test_case(false; "non-critical")]
    #[test_case(true; "critical")]
    #[fuchsia::test(allow_stalls = false)]
    async fn builtin_component_stop_gracefully(main_process_critical: bool) {
        static ROOT_JOB: LazyLock<zx::Job> =
            LazyLock::new(|| fuchsia_runtime::job_default().create_child_job().unwrap());
        let root_job_handle = ROOT_JOB.as_handle_ref().cast();

        let builtin_runner = make_builtin_runner(root_job_handle, make_test_watch_lifecycle());
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fcrunner::ComponentRunnerMarker>();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        builtin_runner
            .clone()
            .get_scoped_runner(
                make_scoped_policy_checker(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    VfsPath::dot(),
                    &mut object_request,
                ),
            )
            .unwrap();
        let (controller, server_end) = fidl::endpoints::create_proxy();

        // Start the "test" component.
        let (svc, svc_server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace("/svc", fio::PERM_READABLE, svc_server_end).unwrap();
        let (start_info, _outgoing_dir) = make_start_info(svc, main_process_critical);
        client.start(start_info, server_end).unwrap();

        let controller2 = controller.clone();
        let mut on_stop = pin!(async move {
            let mut stream = controller2.take_event_stream();
            let event = stream.try_next().await;
            assert_matches!(
                event,
                Ok(Some(fcrunner::ComponentControllerEvent::OnStop {
                    payload: fcrunner::ComponentStopInfo {
                        termination_status: Some(0),
                        exit_code: None,
                        ..
                    }
                }))
            );
            let event = stream.try_next().await;
            assert_matches!(event, Ok(None));
        });
        assert_matches!(TestExecutor::poll_until_stalled(&mut on_stop).await, Poll::Pending);
        controller.stop().unwrap();

        if main_process_critical {
            // If main_process_critical, stop waits for the task. Expect it to hang, and kill
            // the component to terminate it.
            assert_matches!(TestExecutor::poll_until_stalled(&mut on_stop).await, Poll::Pending);
            controller.kill().unwrap();
        }
        on_stop.await;

        if main_process_critical {
            assert_matches!(
                ROOT_JOB.info(),
                Ok(zx::JobInfo {
                    exited: true,
                    return_code: zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL,
                    ..
                })
            );
        } else {
            assert_matches!(ROOT_JOB.info(), Ok(zx::JobInfo { exited: false, .. }));
        }
    }

    #[test_case(false; "non-critical")]
    #[test_case(true; "critical")]
    #[fuchsia::test(allow_stalls = false)]
    async fn builtin_component_kill_forcefully(main_process_critical: bool) {
        static ROOT_JOB: LazyLock<zx::Job> =
            LazyLock::new(|| fuchsia_runtime::job_default().create_child_job().unwrap());
        let root_job_handle = ROOT_JOB.as_handle_ref().cast();

        let builtin_runner = make_builtin_runner(root_job_handle, make_test_hang());
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fcrunner::ComponentRunnerMarker>();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        builtin_runner
            .clone()
            .get_scoped_runner(
                make_scoped_policy_checker(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    VfsPath::dot(),
                    &mut object_request,
                ),
            )
            .unwrap();
        let (controller, server_end) = fidl::endpoints::create_proxy();

        // Start the "test" component.
        let (svc, svc_server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace("/svc", fio::PERM_READABLE, svc_server_end).unwrap();
        let (start_info, _outgoing_dir) = make_start_info(svc, main_process_critical);
        client.start(start_info, server_end).unwrap();

        let controller2 = controller.clone();
        let mut on_stop = pin!(async move {
            let mut stream = controller2.take_event_stream();
            let event = stream.try_next().await;
            assert_matches!(
                event,
                Ok(Some(fcrunner::ComponentControllerEvent::OnStop {
                    payload: fcrunner::ComponentStopInfo {
                        termination_status: Some(0),
                        exit_code: None,
                        ..
                    }
                }))
            );
            let event = stream.try_next().await;
            assert_matches!(event, Ok(None));
        });

        // The component doesn't pay attention to the Stop event, so stop will hang.
        assert_matches!(TestExecutor::poll_until_stalled(&mut on_stop).await, Poll::Pending);
        controller.stop().unwrap();

        // Kill should force kill the component.
        assert_matches!(TestExecutor::poll_until_stalled(&mut on_stop).await, Poll::Pending);
        controller.kill().unwrap();
        on_stop.await;

        if main_process_critical {
            assert_matches!(
                ROOT_JOB.info(),
                Ok(zx::JobInfo {
                    exited: true,
                    return_code: zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL,
                    ..
                })
            );
        } else {
            assert_matches!(ROOT_JOB.info(), Ok(zx::JobInfo { exited: false, .. }));
        }
    }

    /// Tests that:
    /// - The builtin runner is able to start an ELF runner component.
    /// - The ELF runner component started from it can start an ELF component.
    /// - The ELF runner should be stopped in time, and doing so should also kill all
    ///   components run by it.
    #[fuchsia::test]
    async fn elf_runner_start_stop() {
        let elf_runner_resources = make_elf_runner_resources();
        let program = BuiltinRunner::get_elf_program(elf_runner_resources.clone());
        let builtin_runner = make_builtin_runner(fuchsia_runtime::job_default(), program);
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fcrunner::ComponentRunnerMarker>();
        let scope = ExecutionScope::new();
        let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
        builtin_runner
            .clone()
            .get_scoped_runner(
                make_scoped_policy_checker(),
                OpenRequest::new(
                    scope.clone(),
                    fio::OpenFlags::empty(),
                    VfsPath::dot(),
                    &mut object_request,
                ),
            )
            .unwrap();
        let (elf_runner_controller, server_end) = fidl::endpoints::create_proxy();

        // Start the ELF runner.
        let (svc, svc_server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace("/svc", fio::PERM_READABLE, svc_server_end).unwrap();
        let (start_info, outgoing_dir) = make_start_info(svc, false);
        client.start(start_info, server_end).unwrap();

        // Use the ComponentRunner FIDL in the outgoing directory of the ELF runner to run
        // an ELF component.
        let component_runner = fuchsia_component::client::connect_to_protocol_at_dir_svc::<
            fcrunner::ComponentRunnerMarker,
        >(&outgoing_dir)
        .unwrap();

        // Open the current package which contains a `signal-then-hang` component.
        let (pkg, server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace("/pkg", fio::PERM_READABLE | fio::PERM_EXECUTABLE, server_end)
            .unwrap();

        // Run the `signal-then-hang` component and add a numbered handle.
        // This way we can monitor when that program is running.
        let (ch1, ch2) = zx::Channel::create();
        let (not_found, _) = channel::mpsc::unbounded();
        let mut namespace = NamespaceBuilder::new(scope, not_found);
        namespace
            .add_entry(
                Capability::Directory(Directory::new(pkg)),
                &NamespacePath::new("/pkg").unwrap(),
            )
            .unwrap();

        let moniker = Moniker::try_from(vec!["signal_then_hang"]).unwrap();
        let token = elf_runner_resources.instance_registry.add_for_tests(moniker);
        let start_info = StartInfo {
            resolved_url: "fuchsia://signal-then-hang.cm".to_string(),
            program: Dictionary {
                entries: Some(vec![
                    DictionaryEntry {
                        key: "runner".to_string(),
                        value: Some(Box::new(DictionaryValue::Str("elf".to_string()))),
                    },
                    DictionaryEntry {
                        key: "binary".to_string(),
                        value: Some(Box::new(DictionaryValue::Str(
                            "bin/signal_then_hang".to_string(),
                        ))),
                    },
                ]),
                ..Default::default()
            },
            namespace,
            numbered_handles: vec![fprocess::HandleInfo {
                handle: ch1.into(),
                id: HandleInfo::new(HandleType::User0, 0).as_raw(),
            }],
            encoded_config: None,
            break_on_start: None,
            component_instance: token,
        };

        let elf_runner = RemoteRunner::new(component_runner);
        let (diagnostics_sender, _) = oneshot::channel();
        let program = Program::start(
            &elf_runner,
            start_info,
            EscrowedState::outgoing_dir_closed(),
            diagnostics_sender,
        )
        .unwrap();

        // Wait for the ELF component to signal on the channel.
        let signals = fasync::OnSignals::new(&ch2, zx::Signals::USER_0).await.unwrap();
        assert!(signals.contains(zx::Signals::USER_0));

        // Stop the ELF runner component.
        elf_runner_controller.stop().unwrap();

        // The ELF runner controller channel should close normally.
        let event = elf_runner_controller.take_event_stream().try_next().await;
        assert_matches!(
            event,
            Ok(Some(fcrunner::ComponentControllerEvent::OnStop {
                payload: fcrunner::ComponentStopInfo {
                    termination_status: Some(0),
                    exit_code: None,
                    ..
                }
            }))
        );

        // The ELF component controller channel should close (abnormally, because its runner died).
        let result = program.on_terminate().await;
        let instance_died =
            zx::Status::from_raw(fcomponent::Error::InstanceDied.into_primitive() as i32);
        assert_eq!(result.termination_status, instance_died);
    }
}
