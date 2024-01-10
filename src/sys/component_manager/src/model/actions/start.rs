// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::bedrock::program::{Program, StartInfo},
    crate::framework::controller,
    crate::model::{
        actions::{Action, ActionKey},
        component::{
            ComponentInstance, ComponentRuntime, ExecutionState, InstanceState, StartReason,
        },
        error::{CreateNamespaceError, StartActionError, StructuredConfigError},
        hooks::{Event, EventPayload, RuntimeInfo},
        namespace::create_namespace,
        routing::{route_and_open_capability, OpenOptions, RouteRequest},
    },
    crate::runner::RemoteRunner,
    ::namespace::Entry as NamespaceEntry,
    ::routing::{
        component_instance::ComponentInstanceInterface, error::RoutingError,
        policy::GlobalPolicyChecker,
    },
    async_trait::async_trait,
    cm_logger::scoped::ScopedLogger,
    cm_rust::ComponentDecl,
    config_encoder::ConfigFields,
    fidl::{
        endpoints::{create_proxy, DiscoverableProtocolMarker},
        Vmo,
    },
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger,
    fidl_fuchsia_mem as fmem, fidl_fuchsia_process as fprocess, fidl_fuchsia_sys2 as fsys,
    fuchsia_zircon as zx,
    futures::channel::oneshot,
    moniker::Moniker,
    serve_processargs::NamespaceBuilder,
    std::string::ToString,
    std::sync::Arc,
    tracing::warn,
};

/// Starts a component instance.
pub struct StartAction {
    start_reason: StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    numbered_handles: Vec<fprocess::HandleInfo>,
    additional_namespace_entries: Vec<NamespaceEntry>,
}

impl StartAction {
    pub fn new(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        numbered_handles: Vec<fprocess::HandleInfo>,
        additional_namespace_entries: Vec<NamespaceEntry>,
    ) -> Self {
        Self {
            start_reason,
            execution_controller_task,
            numbered_handles,
            additional_namespace_entries,
        }
    }
}

#[async_trait]
impl Action for StartAction {
    type Output = Result<fsys::StartResult, StartActionError>;
    async fn handle(self, component: &Arc<ComponentInstance>) -> Self::Output {
        do_start(
            component,
            &self.start_reason,
            self.execution_controller_task,
            self.numbered_handles,
            self.additional_namespace_entries,
        )
        .await
    }

    fn key(&self) -> ActionKey {
        ActionKey::Start
    }
}

struct StartContext {
    runner: Option<RemoteRunner>,
    url: String,
    namespace_builder: NamespaceBuilder,
    numbered_handles: Vec<fprocess::HandleInfo>,
    encoded_config: Option<fmem::Data>,
}

async fn do_start(
    component: &Arc<ComponentInstance>,
    start_reason: &StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    numbered_handles: Vec<fprocess::HandleInfo>,
    additional_namespace_entries: Vec<NamespaceEntry>,
) -> Result<fsys::StartResult, StartActionError> {
    // Resolve the component.
    let resolved_component = component.resolve().await.map_err(|err| {
        StartActionError::ResolveActionError { moniker: component.moniker.clone(), err }
    })?;

    // Find the runner to use.
    let runner = {
        // Obtain the runner declaration under a short lock, as `open_runner` may lock the
        // resolved state re-entrantly.
        let resolved_state = component.lock_resolved_state().await.map_err(|err| {
            StartActionError::ResolveActionError { moniker: component.moniker.clone(), err }
        })?;
        resolved_state.decl().get_runner()
    };
    let runner = match runner {
        Some(runner) => open_runner(component, runner).await.map_err(|err| {
            warn!(moniker = %component.moniker, %err, "Failed to resolve runner.");
            err
        })?,
        None => None,
    };

    // Create the component's namespace.
    let mut namespace_builder =
        create_namespace(resolved_component.package.as_ref(), component, &resolved_component.decl)
            .await
            .map_err(|err| StartActionError::CreateNamespaceError {
                moniker: component.moniker.clone(),
                err,
            })?;
    for NamespaceEntry { directory, path } in additional_namespace_entries {
        let directory: sandbox::Directory = directory.into();
        namespace_builder.add_entry(Box::new(directory), &path).map_err(|err| {
            StartActionError::CreateNamespaceError {
                moniker: component.moniker.clone(),
                err: CreateNamespaceError::BuildNamespaceError(err),
            }
        })?;
    }

    // Generate the Runtime which will be set in the Execution.
    let decl = &resolved_component.decl;
    let pending_runtime = make_execution_runtime(
        &component,
        component.policy_checker(),
        decl,
        start_reason.clone(),
        execution_controller_task,
    )
    .await?;

    let encoded_config = match resolved_component.config.clone() {
        Some(mut config) => {
            if has_config_capabilities(decl) {
                update_config_with_capabilities(&mut config, decl, &component).await?;
                update_component_config(&component, config.clone()).await?;
            }
            Some(encode_config(config, &component.moniker).await?)
        }
        None => None,
    };

    let start_context = StartContext {
        runner,
        url: resolved_component.resolved_url.clone(),
        namespace_builder,
        numbered_handles,
        encoded_config,
    };

    start_component(&component, resolved_component.decl, pending_runtime, start_context).await
}

/// Set the Runtime in the Execution and start the exit watcher. From component manager's
/// perspective, this indicates that the component has started. If this returns an error, the
/// component was shut down and the Runtime is not set, otherwise the function returns the
/// start context with the runtime set. This function acquires the state and execution locks on
/// `Component`.
async fn start_component(
    component: &Arc<ComponentInstance>,
    decl: ComponentDecl,
    mut pending_runtime: ComponentRuntime,
    start_context: StartContext,
) -> Result<fsys::StartResult, StartActionError> {
    let _actions = component.lock_actions().await;
    let mut state = component.lock_state().await;
    let mut execution = component.lock_execution().await;

    if let Some(r) = should_return_early(&state, &execution, &component.moniker) {
        return r;
    }

    let (diagnostics_sender, diagnostics_receiver) = oneshot::channel();
    let (break_on_start_left, break_on_start_right) = zx::EventPair::create();

    let StartContext { runner, url, namespace_builder, numbered_handles, encoded_config } =
        start_context;
    if let Some(runner) = runner {
        let moniker = &component.moniker;
        let component_instance = state
            .instance_token(moniker, &component.context)
            .ok_or(StartActionError::InstanceDestroyed { moniker: moniker.clone() })?;

        let start_info = StartInfo {
            resolved_url: url,
            program: decl
                .program
                .as_ref()
                .map(|p| p.info.clone())
                .unwrap_or_else(|| fdata::Dictionary::default()),
            namespace: namespace_builder,
            numbered_handles,
            encoded_config,
            break_on_start: Some(break_on_start_left),
            component_instance,
        };

        pending_runtime.set_program(
            Program::start(&runner, start_info, diagnostics_sender).map_err(|err| {
                StartActionError::StartProgramError { moniker: moniker.clone(), err }
            })?,
            component.as_weak(),
        );
    }

    let runtime_info = RuntimeInfo::from_runtime(&pending_runtime, diagnostics_receiver);
    let runtime_dir = pending_runtime.runtime_dir().cloned();
    let timestamp = pending_runtime.timestamp;

    execution.runtime = Some(pending_runtime);
    drop(execution);
    drop(state);

    // Dispatch Started and DebugStarted events outside of the execution lock and state
    // lock, but under the actions lock, so that:
    //
    // - Hooks implementations can use the execution state (such as re-entrantly ensuring
    //   the component is started).
    // - The Started events will be ordered before any stop events that may happen if the
    //   program terminated in the meantime.
    //
    component
        .hooks
        .dispatch(&Event::new_with_timestamp(
            component,
            EventPayload::Started { runtime: runtime_info, component_decl: decl },
            timestamp,
        ))
        .await;
    component
        .hooks
        .dispatch(&Event::new_with_timestamp(
            component,
            EventPayload::DebugStarted {
                runtime_dir,
                break_on_start: Arc::new(break_on_start_right),
            },
            timestamp,
        ))
        .await;

    Ok(fsys::StartResult::Started)
}

/// Returns `Some(Result)` if `start` should return early due to any of the following:
/// - The component instance is destroyed.
/// - The component instance is shut down.
/// - The component instance is already started.
pub fn should_return_early(
    component: &InstanceState,
    execution: &ExecutionState,
    moniker: &Moniker,
) -> Option<Result<fsys::StartResult, StartActionError>> {
    match component {
        InstanceState::New | InstanceState::Unresolved(_) | InstanceState::Resolved(_) => {}
        InstanceState::Destroyed => {
            return Some(Err(StartActionError::InstanceDestroyed { moniker: moniker.clone() }));
        }
    }
    if execution.is_shut_down() {
        Some(Err(StartActionError::InstanceShutDown { moniker: moniker.clone() }))
    } else if execution.runtime.is_some() {
        Some(Ok(fsys::StartResult::AlreadyStarted))
    } else {
        None
    }
}

/// Returns a RemoteRunner routed to the component's runner, if it specifies one.
///
/// Returns None if the component's decl does not specify a runner.
async fn open_runner(
    component: &Arc<ComponentInstance>,
    runner: cm_rust::UseRunnerDecl,
) -> Result<Option<RemoteRunner>, StartActionError> {
    // Open up a channel to the runner.
    let (client, server) = create_proxy::<fcrunner::ComponentRunnerMarker>().unwrap();
    let mut server_channel = server.into_channel();
    let options = OpenOptions {
        flags: fio::OpenFlags::NOT_DIRECTORY,
        relative_path: "".into(),
        server_chan: &mut server_channel,
    };
    route_and_open_capability(RouteRequest::UseRunner(runner.clone()), component, options)
        .await
        .map_err(|err| StartActionError::ResolveRunnerError {
            moniker: component.moniker.clone(),
            err: Box::new(err),
            runner: runner.source_name,
        })?;

    return Ok(Some(RemoteRunner::new(client)));
}

fn get_config_field<'a>(
    name: &str,
    decl: &'a cm_rust::ComponentDecl,
) -> Option<&'a cm_rust::UseConfigurationDecl> {
    decl.uses.iter().find_map(|use_| match use_ {
        cm_rust::UseDecl::Config(c) => (c.target_name == name).then_some(c),
        _ => None,
    })
}

/// Returns true if the decl uses configuration capabilities.
fn has_config_capabilities(decl: &cm_rust::ComponentDecl) -> bool {
    decl.uses.iter().find(|u| matches!(u, cm_rust::UseDecl::Config(_))).is_some()
}

/// Update the component's configuration fields.
async fn update_component_config(
    component: &Arc<ComponentInstance>,
    config: ConfigFields,
) -> Result<(), StartActionError> {
    let mut resolved_state = component.lock_resolved_state().await.unwrap();
    resolved_state.resolved_component.config = Some(config);
    Ok(())
}

/// Update config fields with the values received through configuration
/// capabilities.  This will perform routing on each of the configuration `use`
/// decls to get the values. Updating the fields is fine because configuration
/// capabilities take precedence over both the CVF value and "mutability: parent".
async fn update_config_with_capabilities(
    config: &mut ConfigFields,
    decl: &cm_rust::ComponentDecl,
    component: &Arc<ComponentInstance>,
) -> Result<(), StartActionError> {
    for field in config.fields.iter_mut() {
        let Some(use_config) = get_config_field(&field.key, decl) else {
            continue;
        };

        let source = routing::route_capability(
            RouteRequest::UseConfig(use_config.clone()),
            component,
            &mut routing::mapper::NoopRouteMapper,
        )
        .await
        .map_err(|err| StartActionError::StructuredConfigError {
            moniker: component.moniker.clone(),
            err: err.into(),
        })?;

        let cap = match source.source {
            routing::capability_source::CapabilitySource::Void { .. } => continue,

            routing::capability_source::CapabilitySource::Capability {
                source_capability, ..
            } => source_capability,
            routing::capability_source::CapabilitySource::Component { capability, .. } => {
                capability
            }
            o => {
                return Err(StartActionError::StructuredConfigError {
                    moniker: component.moniker.clone(),
                    err: RoutingError::UnsupportedRouteSource {
                        source_type: o.type_name().to_string(),
                    }
                    .into(),
                })
            }
        };

        let cap = match cap {
            routing::capability_source::ComponentCapability::Config(c) => c,
            c => {
                return Err(StartActionError::StructuredConfigError {
                    moniker: component.moniker.clone(),
                    err: RoutingError::UnsupportedCapabilityType {
                        type_name: c.type_name().into(),
                    }
                    .into(),
                })
            }
        };

        if !field.value.matches_type(&cap.value) {
            return Err(StartActionError::StructuredConfigError {
                moniker: component.moniker.clone(),
                err: StructuredConfigError::ValueMismatch { key: field.key.clone() },
            });
        }
        field.value = cap.value;
    }
    Ok(())
}

/// Encode the configuration into a VMO.
async fn encode_config(
    config: ConfigFields,
    moniker: &Moniker,
) -> Result<fmem::Data, StartActionError> {
    let (vmo, size) = (|| {
        let encoded = config.encode_as_fidl_struct();
        let size = encoded.len() as u64;
        let vmo = Vmo::create(size)?;
        vmo.write(&encoded, 0)?;
        Ok((vmo, size))
    })()
    .map_err(|s| StartActionError::StructuredConfigError {
        err: StructuredConfigError::VmoCreateFailed(s),
        moniker: moniker.clone(),
    })?;
    Ok(fmem::Data::Buffer(fmem::Buffer { vmo, size }))
}

/// Returns a configured Runtime for a component and the start info (without actually starting
/// the component).
async fn make_execution_runtime(
    component: &Arc<ComponentInstance>,
    checker: &GlobalPolicyChecker,
    decl: &cm_rust::ComponentDecl,
    start_reason: StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
) -> Result<ComponentRuntime, StartActionError> {
    // TODO(https://fxbug.dev/120713): Consider moving this check to ComponentInstance::add_child
    match component.on_terminate {
        fdecl::OnTerminate::Reboot => {
            checker.reboot_on_terminate_allowed(&component.moniker).map_err(|err| {
                StartActionError::RebootOnTerminateForbidden {
                    moniker: component.moniker.clone(),
                    err,
                }
            })?;
        }
        fdecl::OnTerminate::None => {}
    }

    let logger = if let Some(logsink_decl) = get_logsink_decl(&decl) {
        match create_scoped_logger(component, logsink_decl.clone()).await {
            Ok(logger) => Some(logger),
            Err(err) => {
                warn!(moniker = %component.moniker, %err, "Could not create logger for component. Logs will be attributed to component_manager");
                None
            }
        }
    } else {
        None
    };

    Ok(ComponentRuntime::new(start_reason, execution_controller_task, logger))
}

/// Returns the UseProtocolDecl for the LogSink protocol, if any.
fn get_logsink_decl<'a>(decl: &'a cm_rust::ComponentDecl) -> Option<&'a cm_rust::UseProtocolDecl> {
    decl.uses.iter().find_map(|use_| match use_ {
        cm_rust::UseDecl::Protocol(decl) => {
            (decl.source_name == flogger::LogSinkMarker::PROTOCOL_NAME).then_some(decl)
        }
        _ => None,
    })
}

/// Returns a ScopedLogger attributed to the component, given its use declaration for the
/// `fuchsia.logger.LogSink` protocol.
async fn create_scoped_logger(
    component: &Arc<ComponentInstance>,
    logsink_decl: cm_rust::UseProtocolDecl,
) -> Result<ScopedLogger, anyhow::Error> {
    let (logsink, logsink_server_end) = create_proxy::<flogger::LogSinkMarker>().unwrap();
    let route_request = RouteRequest::UseProtocol(logsink_decl);
    let open_options = OpenOptions {
        flags: fio::OpenFlags::empty(),
        relative_path: String::new(),
        server_chan: &mut logsink_server_end.into_channel(),
    };
    route_and_open_capability(route_request, component, open_options).await?;
    Ok(ScopedLogger::create(logsink)?)
}

#[cfg(test)]
mod tests {
    use {
        crate::model::{
            actions::{
                start::should_return_early, ActionSet, ShutdownAction, ShutdownType, StartAction,
                StopAction,
            },
            component::{
                Component, ComponentInstance, ComponentRuntime, ExecutionState, InstanceState,
                ResolvedInstanceState, StartReason, UnresolvedInstanceState,
            },
            error::{ModelError, StartActionError},
            hooks::{Event, EventType, Hook, HooksRegistration},
            testing::{
                test_helpers::{self, ActionsTest},
                test_hook::Lifecycle,
            },
        },
        assert_matches::assert_matches,
        async_trait::async_trait,
        cm_rust::ComponentDecl,
        cm_rust_testing::{ChildDeclBuilder, ComponentDeclBuilder},
        fidl_fuchsia_sys2 as fsys, fuchsia, fuchsia_async as fasync, fuchsia_zircon as zx,
        futures::{channel::mpsc, StreamExt},
        moniker::Moniker,
        routing::resolving::ComponentAddress,
        sandbox::Dict,
        std::sync::{Arc, Mutex, Weak},
    };

    // Child name for test child components instantiated during tests.
    const TEST_CHILD_NAME: &str = "child";

    struct ShutdownOnStartHook {
        component: Arc<ComponentInstance>,
        done: Mutex<mpsc::Sender<()>>,
    }

    #[async_trait]
    impl Hook for ShutdownOnStartHook {
        async fn on(self: Arc<Self>, _event: &Event) -> Result<(), ModelError> {
            fasync::Task::spawn(async move {
                ActionSet::register(
                    self.component.clone(),
                    ShutdownAction::new(ShutdownType::Instance),
                )
                .await
                .expect("shutdown failed");
                self.done.lock().unwrap().try_send(()).unwrap();
            })
            .detach();
            Ok(())
        }
    }

    #[fuchsia::test]
    /// Validate that if a start action is issued and the component shuts down
    /// the action completes we see a Stop event emitted.
    async fn start_issues_shutdown() {
        let (test_topology, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let (done, mut hook_done_receiver) = mpsc::channel(1);
        let start_hook =
            Arc::new(ShutdownOnStartHook { component: child.clone(), done: Mutex::new(done) });
        child
            .hooks
            .install(vec![HooksRegistration::new(
                "my_start_hook",
                vec![EventType::Started],
                Arc::downgrade(&start_hook) as Weak<dyn Hook>,
            )])
            .await;

        let start_result = ActionSet::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
        )
        .await
        .unwrap();
        assert_eq!(start_result, fsys::StartResult::Started);

        // Wait until the action in the hook is done.
        hook_done_receiver.next().await.unwrap();

        let events: Vec<_> = test_topology
            .test_hook
            .lifecycle()
            .into_iter()
            .filter(|event| match event {
                Lifecycle::Start(_) | Lifecycle::Stop(_) => true,
                _ => false,
            })
            .collect();
        assert_eq!(
            events,
            vec![
                Lifecycle::Start(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap()),
                Lifecycle::Stop(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap())
            ]
        );
    }

    struct StopOnStartHook {
        component: Arc<ComponentInstance>,
        done: Mutex<mpsc::Sender<()>>,
    }

    #[async_trait]
    impl Hook for StopOnStartHook {
        async fn on(self: Arc<Self>, _event: &Event) -> Result<(), ModelError> {
            fasync::Task::spawn(async move {
                ActionSet::register(self.component.clone(), StopAction::new(false))
                    .await
                    .expect("stop failed");
                self.done.lock().unwrap().try_send(()).unwrap();
            })
            .detach();
            Ok(())
        }
    }

    #[fuchsia::test]
    /// Validate that if a start action is issued and the component stops
    /// the action completes we see a Stop event emitted.
    async fn start_issues_stop() {
        let (test_topology, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let (done, mut hook_done_receiver) = mpsc::channel(1);
        let start_hook =
            Arc::new(StopOnStartHook { component: child.clone(), done: Mutex::new(done) });
        child
            .hooks
            .install(vec![HooksRegistration::new(
                "my_start_hook",
                vec![EventType::Started],
                Arc::downgrade(&start_hook) as Weak<dyn Hook>,
            )])
            .await;

        let start_result = ActionSet::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
        )
        .await
        .unwrap();
        assert_eq!(start_result, fsys::StartResult::Started);

        // Wait until the action in the hook is done.
        hook_done_receiver.next().await.unwrap();

        let events: Vec<_> = test_topology
            .test_hook
            .lifecycle()
            .into_iter()
            .filter(|event| match event {
                Lifecycle::Start(_) | Lifecycle::Stop(_) => true,
                _ => false,
            })
            .collect();
        assert_eq!(
            events,
            vec![
                Lifecycle::Start(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap()),
                Lifecycle::Stop(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap())
            ]
        );
    }

    #[fuchsia::test]
    async fn restart_set_execution_runtime() {
        let (_test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        {
            let timestamp = zx::Time::get_monotonic();
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![]),
            )
            .await
            .expect("failed to start child");
            let execution = child.lock_execution().await;
            let runtime = execution.runtime.as_ref().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }

        {
            ActionSet::register(child.clone(), StopAction::new(false))
                .await
                .expect("failed to stop child");
            let execution = child.lock_execution().await;
            assert!(execution.runtime.is_none());
        }

        {
            let timestamp = zx::Time::get_monotonic();
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![]),
            )
            .await
            .expect("failed to start child");
            let execution = child.lock_execution().await;
            let runtime = execution.runtime.as_ref().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }
    }

    #[fuchsia::test]
    async fn restart_does_not_refresh_resolved_state() {
        let (mut test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        {
            let timestamp = zx::Time::get_monotonic();
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![]),
            )
            .await
            .expect("failed to start child");
            let execution = child.lock_execution().await;
            let runtime = execution.runtime.as_ref().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }

        {
            let () = ActionSet::register(child.clone(), StopAction::new(false))
                .await
                .expect("failed to stop child");
            let execution = child.lock_execution().await;
            assert!(execution.runtime.is_none());
        }

        let resolver = test_harness.resolver.as_mut();
        let original_decl =
            resolver.get_component_decl(TEST_CHILD_NAME).expect("child decl not stored");
        let mut modified_decl = original_decl.clone();
        modified_decl.children.push(ChildDeclBuilder::new().name("foo").build());
        resolver.add_component(TEST_CHILD_NAME, modified_decl.clone());

        ActionSet::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
        )
        .await
        .expect("failed to start child");

        let resolved_decl = get_resolved_decl(&child).await;
        assert_ne!(resolved_decl, modified_decl);
        assert_eq!(resolved_decl, original_decl);
    }

    async fn build_tree_with_single_child(
        child_name: &'static str,
    ) -> (ActionsTest, Arc<ComponentInstance>) {
        let root_name = "root";
        let components = vec![
            (root_name, ComponentDeclBuilder::new().add_lazy_child(child_name).build()),
            (child_name, test_helpers::component_decl_with_test_runner()),
        ];
        let test_topology = ActionsTest::new(components[0].0, components, None).await;

        let child = test_topology.look_up(vec![child_name].try_into().unwrap()).await;

        (test_topology, child)
    }

    async fn get_resolved_decl(component: &Arc<ComponentInstance>) -> ComponentDecl {
        let state = component.lock_state().await;
        let resolved_state = match &*state {
            InstanceState::Resolved(resolve_state) => resolve_state,
            _ => panic!("expected component to be resolved"),
        };

        resolved_state.decl().clone()
    }

    #[fuchsia::test]
    async fn check_should_return_early() {
        let m = Moniker::try_from(vec!["foo"]).unwrap();
        let es = ExecutionState::new();

        // Checks based on InstanceState:
        assert!(should_return_early(&InstanceState::New, &es, &m).is_none());
        assert!(should_return_early(
            &InstanceState::Unresolved(UnresolvedInstanceState::new(Dict::new())),
            &es,
            &m
        )
        .is_none());
        assert_matches!(
            should_return_early(&InstanceState::Destroyed, &es, &m),
            Some(Err(StartActionError::InstanceDestroyed { moniker: _ }))
        );
        let (_, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let decl = ComponentDeclBuilder::new().add_lazy_child(TEST_CHILD_NAME).build();
        let resolved_component = Component {
            resolved_url: "".to_string(),
            context_to_resolve_children: None,
            decl,
            package: None,
            config: None,
            abi_revision: None,
        };
        let ris = ResolvedInstanceState::new(
            &child,
            resolved_component,
            ComponentAddress::from_absolute_url(&child.component_url).unwrap(),
            Default::default(),
            Dict::new(),
        )
        .await
        .unwrap();
        assert!(should_return_early(&InstanceState::Resolved(ris), &es, &m).is_none());

        // Check for already_started:
        {
            let mut es = ExecutionState::new();
            es.runtime = Some(ComponentRuntime::new(StartReason::Debug, None, None));
            assert!(!es.is_shut_down());
            assert_matches!(
                should_return_early(&InstanceState::New, &es, &m),
                Some(Ok(fsys::StartResult::AlreadyStarted))
            );
        }

        // Check for shut_down:
        let _ = child.stop_instance_internal(true).await;
        let execution = child.lock_execution().await;
        assert!(execution.is_shut_down());
        assert_matches!(
            should_return_early(&InstanceState::New, &execution, &m),
            Some(Err(StartActionError::InstanceShutDown { moniker: _ }))
        );
    }

    #[fuchsia::test]
    async fn check_already_started() {
        let (_test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        assert_eq!(
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![])
            )
            .await
            .expect("failed to start child"),
            fsys::StartResult::Started
        );

        assert_eq!(
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![])
            )
            .await
            .expect("failed to start child"),
            fsys::StartResult::AlreadyStarted
        );
    }
}
