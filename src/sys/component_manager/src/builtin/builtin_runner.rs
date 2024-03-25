// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use cm_config::SecurityPolicy;
use cm_util::TaskGroup;
use elf_runner::{crash_info::CrashRecords, process_launcher::NamespaceConnector};
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_memory_report as freport;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, Clock};
use futures::{future::BoxFuture, Future, FutureExt, TryStreamExt};
use namespace::{Namespace, NamespaceError};
use routing::policy::ScopedPolicyChecker;
use runner::component::{ChannelEpitaph, Controllable, Controller};
use sandbox::{Capability, CapabilityTrait, Dict, Open};
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;
use vfs::execution_scope::ExecutionScope;
use zx::{AsHandleRef, HandleBased, Task};

use crate::{
    builtin::runner::BuiltinRunnerFactory,
    model::component::WeakComponentInstance,
    model::token::{InstanceRegistry, InstanceToken},
    sandbox_util,
    sandbox_util::LaunchTaskOnReceive,
};

const TYPE: &str = "type";
const SVC: &str = "svc";

/// The builtin runner runs components implemented inside component_manager.
///
/// Builtin components are still defined by a declaration. When a component uses
/// the builtin runner, the `type` field in the program block will identify which
/// builtin component to run (e.g `type: "elf_runner"`).
///
/// When bootstrapping the system, builtin components may be resolved by the builtin URL
/// scheme, e.g. fuchsia-builtin://#elf_runner.cm. However, it's entirely possible to resolve
/// a builtin component via other schemes. A component is a builtin component if and only
/// if it uses the builtin runner.
pub struct BuiltinRunner {
    root_job: zx::Unowned<'static, zx::Job>,
    task_group: TaskGroup,
    elf_runner_resources: Arc<ElfRunnerResources>,
}

/// Pure data type holding some resources needed by the ELF runner.
// TODO(https://fxbug.dev/318697539): Most of this should be replaced by
// capabilities in the incoming namespace of the ELF runner component.
pub struct ElfRunnerResources {
    /// Job policy requests in the program block of ELF components will be checked against
    /// the provided security policy.
    pub security_policy: Arc<SecurityPolicy>,
    pub utc_clock: Option<Arc<Clock>>,
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

    #[error("\"program.type\" must be specified")]
    MissingProgramType,

    #[error("cannot create job: {}", _0)]
    JobCreation(fuchsia_zircon_status::Status),

    #[error("unsupported \"program.type\": {}", _0)]
    UnsupportedProgramType(String),
}

impl From<BuiltinRunnerError> for zx::Status {
    fn from(value: BuiltinRunnerError) -> Self {
        match value {
            BuiltinRunnerError::MissingOutgoingDir
            | BuiltinRunnerError::MissingNamespace
            | BuiltinRunnerError::NamespaceError(_)
            | BuiltinRunnerError::MissingProgramType
            | BuiltinRunnerError::UnsupportedProgramType(_) => {
                zx::Status::from_raw(fcomponent::Error::InvalidArguments.into_primitive() as i32)
            }
            BuiltinRunnerError::JobCreation(status) => status,
        }
    }
}

impl BuiltinRunner {
    /// Creates a builtin runner with its required resources.
    /// - `task_group`: The tasks associated with the builtin runner.
    pub fn new(task_group: TaskGroup, elf_runner_resources: ElfRunnerResources) -> Self {
        let root_job = fuchsia_runtime::job_default();
        BuiltinRunner { root_job, task_group, elf_runner_resources: Arc::new(elf_runner_resources) }
    }

    /// Starts a builtin component.
    fn start(
        self: Arc<BuiltinRunner>,
        mut start_info: fcrunner::ComponentStartInfo,
    ) -> Result<(impl Controllable, impl Future<Output = ChannelEpitaph> + Unpin), BuiltinRunnerError>
    {
        let outgoing_dir =
            start_info.outgoing_dir.take().ok_or(BuiltinRunnerError::MissingOutgoingDir)?;
        let namespace: Namespace =
            start_info.ns.take().ok_or(BuiltinRunnerError::MissingNamespace)?.try_into()?;
        let program_type = runner::get_program_string(&start_info, TYPE)
            .ok_or(BuiltinRunnerError::MissingProgramType)?;

        match program_type {
            "elf_runner" => {
                let job =
                    self.root_job.create_child_job().map_err(BuiltinRunnerError::JobCreation)?;
                let program = ElfRunnerProgram::new(
                    job.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                    namespace,
                    self.elf_runner_resources.clone(),
                );
                program.serve_outgoing(outgoing_dir);
                Ok((program, Box::pin(wait_for_job_termination(job))))
            }
            _ => Err(BuiltinRunnerError::UnsupportedProgramType(program_type.to_string())),
        }
    }
}

/// Waits for the job used by an ELF runner to run components to terminate, and translate
/// the return code to an epitaph.
///
/// Normally, the job will terminate when the builtin runner requests to stop the ELF runner.
/// We'll observe the asynchronous termination here and consider the ELF runner stopped.
async fn wait_for_job_termination(job: zx::Job) -> ChannelEpitaph {
    fasync::OnSignals::new(&job.as_handle_ref(), zx::Signals::JOB_TERMINATED)
        .await
        .map(|_: fidl::Signals| ())
        .unwrap_or_else(|error| warn!(%error, "error waiting for job termination"));

    use fidl_fuchsia_component::Error;
    let exit_status: ChannelEpitaph = match job.info() {
        Ok(zx::JobInfo { return_code: zx::sys::ZX_TASK_RETCODE_SYSCALL_KILL, .. }) => {
            // Stopping the ELF runner will destroy the job, so this is the only
            // normal exit code path.
            ChannelEpitaph::ok()
        }
        Ok(zx::JobInfo { return_code, .. }) => {
            warn!(%return_code, "job terminated with abnormal return code");
            Error::InstanceDied.into()
        }
        Err(error) => {
            warn!(%error, "Unable to query job info");
            Error::Internal.into()
        }
    };
    exit_status
}

impl BuiltinRunnerFactory for BuiltinRunner {
    fn get_scoped_runner(
        self: Arc<Self>,
        _checker: ScopedPolicyChecker,
        server_end: ServerEnd<fcrunner::ComponentRunnerMarker>,
    ) {
        let runner = self.clone();
        let mut stream = server_end.into_stream().unwrap();
        runner.clone().task_group.spawn(async move {
            while let Ok(Some(request)) = stream.try_next().await {
                let fcrunner::ComponentRunnerRequest::Start { start_info, controller, .. } =
                    request;
                match runner.clone().start(start_info) {
                    Ok((program, on_exit)) => {
                        let controller =
                            Controller::new(program, controller.into_stream().unwrap());
                        runner.task_group.spawn(controller.serve(on_exit));
                    }
                    Err(err) => {
                        warn!("Builtin runner failed to run component: {err}");
                        let _ = controller.close_with_epitaph(err.into());
                    }
                }
            }
        });
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
        let connector = NamespaceConnector { namespace: namespace.clone() };
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

        let inner_clone = inner.clone();
        let snapshot_provider = Arc::new(LaunchTaskOnReceive::new(
            task_group.as_weak(),
            freport::SnapshotProviderMarker::PROTOCOL_NAME,
            None,
            Arc::new(move |server_end, _| {
                inner_clone.clone().elf_runner.serve_memory_reporter(
                    sandbox_util::take_handle_as_stream::<freport::SnapshotProviderMarker>(
                        server_end,
                    ),
                );
                std::future::ready(Result::<(), anyhow::Error>::Ok(())).boxed()
            }),
        ));
        let output = Dict::new();
        let svc = Dict::new();
        {
            let mut entries = svc.lock_entries();
            entries.insert(
                fcrunner::ComponentRunnerMarker::PROTOCOL_NAME.to_string(),
                Capability::Open(elf_runner.into_open(WeakComponentInstance::invalid())),
            );
            entries.insert(
                freport::SnapshotProviderMarker::PROTOCOL_NAME.to_string(),
                Capability::Open(snapshot_provider.into_open(WeakComponentInstance::invalid())),
            );
        }
        output.lock_entries().insert(SVC.to_string(), Capability::Dictionary(svc));

        let this = Self { task_group, execution_scope: ExecutionScope::new(), output, job };
        this
    }

    /// Serves requests coming from `outgoing_dir` using `self.output`.
    fn serve_outgoing(&self, outgoing_dir: ServerEnd<fio::DirectoryMarker>) {
        let output = self.output.clone();
        let open = Open::new(output.try_into_directory_entry().unwrap());
        open.open(
            self.execution_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            ".".to_string(),
            outgoing_dir.into_channel(),
        );
    }
}

/// In case `Controller` did not call `stop`, this will ensure that the job is destroyed.
impl Drop for ElfRunnerProgram {
    fn drop(&mut self) {
        _ = self.job.kill();
    }
}

impl Inner {
    async fn serve_component_runner(
        self: Arc<Self>,
        mut stream: fcrunner::ComponentRunnerRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            let fcrunner::ComponentRunnerRequest::Start { mut start_info, controller, .. } =
                request;
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
                    "The provided ComponentStartInfo.component_instance  token is invalid. \
                    The component has either already been destroyed, or the token is not minted by \
                    component_manager."
                );
                _ = controller.close_with_epitaph(zx::Status::NOT_SUPPORTED);
                continue;
            };
            start_info.component_instance = Some(token.into());
            let checker = ScopedPolicyChecker::new(
                self.resources.security_policy.clone(),
                target_moniker.clone(),
            );
            self.elf_runner.clone().get_scoped_runner(checker).start(start_info, controller).await;
        }
        Ok(())
    }
}

#[async_trait]
impl Controllable for ElfRunnerProgram {
    async fn kill(&mut self) {
        warn!("Timed out stopping ElfRunner tasks");
        self.stop().await
    }

    fn stop<'a>(&mut self) -> BoxFuture<'a, ()> {
        _ = self.job.kill();
        self.execution_scope.shutdown();
        self.task_group.clone().join().boxed()
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use cm_types::NamespacePath;
    use fcrunner::{ComponentNamespaceEntry, ComponentStartInfo};
    use fidl::endpoints::ClientEnd;
    use fidl_fuchsia_data::{Dictionary, DictionaryEntry, DictionaryValue};
    use fidl_fuchsia_io::DirectoryProxy;
    use fidl_fuchsia_process as fprocess;
    use fuchsia_fs::directory::open_channel_in_namespace;
    use fuchsia_runtime::{HandleInfo, HandleType};
    use futures::channel::{self, oneshot};
    use moniker::{Moniker, MonikerBase};
    use sandbox::Directory;
    use serve_processargs::NamespaceBuilder;

    use crate::{
        bedrock::program::{Program, StartInfo},
        model::escrow::EscrowedState,
        runner::RemoteRunner,
    };

    use super::*;

    fn make_security_policy() -> Arc<SecurityPolicy> {
        Arc::new(Default::default())
    }

    fn make_scoped_policy_checker() -> ScopedPolicyChecker {
        ScopedPolicyChecker::new(make_security_policy(), Moniker::new(vec![]))
    }

    fn make_builtin_runner() -> Arc<BuiltinRunner> {
        let task_group = TaskGroup::new();
        let security_policy = make_security_policy();
        let crash_records = CrashRecords::new();
        let instance_registry = InstanceRegistry::new();
        let elf_runner_resources = ElfRunnerResources {
            security_policy,
            utc_clock: None,
            crash_records,
            instance_registry,
        };
        Arc::new(BuiltinRunner::new(task_group, elf_runner_resources))
    }

    fn make_start_info(
        program_type: &str,
        svc_dir: ClientEnd<fio::DirectoryMarker>,
    ) -> (ComponentStartInfo, DirectoryProxy) {
        let (outgoing_dir, outgoing_server_end) = fidl::endpoints::create_proxy().unwrap();
        let start_info = ComponentStartInfo {
            resolved_url: Some("fuchsia-builtin://elf_runner.cm".to_string()),
            program: Some(Dictionary {
                entries: Some(vec![DictionaryEntry {
                    key: "type".to_string(),
                    value: Some(Box::new(DictionaryValue::Str(program_type.to_string()))),
                }]),
                ..Default::default()
            }),
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
        (start_info, outgoing_dir)
    }

    /// Tests that:
    /// - The builtin runner is able to start an ELF runner component.
    /// - The ELF runner component started from it can start an ELF component.
    /// - The ELF runner should be stopped in time, and doing so should also kill all
    ///   components run by it.
    #[fuchsia::test]
    async fn start_stop_elf_runner() {
        let builtin_runner = make_builtin_runner();
        let (client, server_end) = fidl::endpoints::create_proxy().unwrap();
        builtin_runner.clone().get_scoped_runner(make_scoped_policy_checker(), server_end);
        let (elf_runner_controller, server_end) = fidl::endpoints::create_proxy().unwrap();

        // Start the ELF runner.
        let (svc, svc_server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace("/svc", fio::OpenFlags::RIGHT_READABLE, svc_server_end).unwrap();
        let (start_info, outgoing_dir) = make_start_info("elf_runner", svc);
        client.start(start_info, server_end).unwrap();

        // Use the ComponentRunner FIDL in the outgoing directory of the ELF runner to run
        // an ELF component.
        let component_runner = fuchsia_component::client::connect_to_protocol_at_dir_svc::<
            fcrunner::ComponentRunnerMarker,
        >(&outgoing_dir)
        .unwrap();

        // Open the current package which contains a `signal-then-hang` component.
        let (pkg, server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            server_end,
        )
        .unwrap();

        // Run the `signal-then-hang` component and add a numbered handle.
        // This way we can monitor when that program is running.
        let (ch1, ch2) = zx::Channel::create();
        let (not_found, _) = channel::mpsc::unbounded();
        let mut namespace = NamespaceBuilder::new(ExecutionScope::new(), not_found);
        namespace
            .add_entry(
                Capability::Directory(Directory::new(pkg)),
                &NamespacePath::new("/pkg").unwrap(),
            )
            .unwrap();

        let moniker = Moniker::try_from(vec!["signal_then_hang"]).unwrap();
        let token = builtin_runner.elf_runner_resources.instance_registry.add_for_tests(moniker);
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
            ExecutionScope::new(),
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
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == zx::Status::OK
        );

        // The ELF component controller channel should close (abnormally, because its runner died).
        let result = program.on_terminate().await;
        let instance_died =
            zx::Status::from_raw(fcomponent::Error::InstanceDied.into_primitive() as i32);
        assert_eq!(result, instance_died);
    }

    /// Test that the builtin runner reports errors when starting unknown types.
    #[fuchsia::test]
    async fn start_error_unknown_type() {
        let builtin_runner = make_builtin_runner();
        let (client, server_end) = fidl::endpoints::create_proxy().unwrap();
        builtin_runner.get_scoped_runner(make_scoped_policy_checker(), server_end);
        let (controller, server_end) = fidl::endpoints::create_proxy().unwrap();
        let (svc, svc_server_end) = fidl::endpoints::create_endpoints();
        open_channel_in_namespace("/svc", fio::OpenFlags::RIGHT_READABLE, svc_server_end).unwrap();
        let (start_info, _outgoing_dir) = make_start_info("foobar", svc);
        client.start(start_info, server_end).unwrap();
        let event = controller.take_event_stream().try_next().await;
        let invalid_arguments =
            zx::Status::from_raw(fcomponent::Error::InvalidArguments.into_primitive() as i32);
        assert_matches!(
            event,
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == invalid_arguments
        );
    }
}
