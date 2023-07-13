// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::framework::controller,
    crate::model::{
        actions::{Action, ActionKey},
        component::{
            ComponentInstance, ExecutionState, InstanceState, Package, Runtime, StartReason,
        },
        error::{NamespacePopulateError, StartActionError, StructuredConfigError},
        hooks::{Event, EventPayload, RuntimeInfo},
        namespace::IncomingNamespace,
    },
    ::routing::{component_instance::ComponentInstanceInterface, policy::GlobalPolicyChecker},
    async_trait::async_trait,
    cm_runner::Runner,
    config_encoder::ConfigFields,
    fidl::{
        endpoints::{self, Proxy, ServerEnd},
        Vmo,
    },
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_io as fio, fidl_fuchsia_mem as fmem, fidl_fuchsia_process as fprocess,
    fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::lock::Mutex,
    moniker::Moniker,
    std::sync::Arc,
    tracing::warn,
};

/// Starts a component instance.
pub struct StartAction {
    start_reason: StartReason,
    execution_controller_task: Mutex<Option<controller::ExecutionControllerTask>>,
    numbered_handles: Mutex<Option<Vec<fprocess::HandleInfo>>>,
    additional_namespace_entries: Mutex<Option<Vec<fcrunner::ComponentNamespaceEntry>>>,
}

impl StartAction {
    pub fn new(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        numbered_handles: Vec<fprocess::HandleInfo>,
        additional_namespace_entries: Vec<fcrunner::ComponentNamespaceEntry>,
    ) -> Self {
        Self {
            start_reason,
            execution_controller_task: Mutex::new(execution_controller_task),
            numbered_handles: Mutex::new(Some(numbered_handles)),
            additional_namespace_entries: Mutex::new(Some(additional_namespace_entries)),
        }
    }
}

#[async_trait]
impl Action for StartAction {
    type Output = Result<fsys::StartResult, StartActionError>;
    async fn handle(&self, component: &Arc<ComponentInstance>) -> Self::Output {
        do_start(
            component,
            &self.start_reason,
            self.execution_controller_task.lock().await.take(),
            self.numbered_handles.lock().await.take().expect("StartAction was run twice"),
            self.additional_namespace_entries.lock().await.take().expect(
                "StartAction was run
                 twice",
            ),
        )
        .await
    }

    fn key(&self) -> ActionKey {
        ActionKey::Start
    }
}

struct StartContext {
    runner: Arc<dyn Runner>,
    start_info: fcrunner::ComponentStartInfo,
    controller_server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
}

async fn do_start(
    component: &Arc<ComponentInstance>,
    start_reason: &StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    numbered_handles: Vec<fprocess::HandleInfo>,
    additional_namespace_entries: Vec<fcrunner::ComponentNamespaceEntry>,
) -> Result<fsys::StartResult, StartActionError> {
    // Pre-flight check: if the component is already started, or was shut down, return now. Note
    // that `start` also performs this check before scheduling the action here. We do it again
    // while the action is registered to avoid the risk of dispatching the Started event twice.
    {
        let state = component.lock_state().await;
        let execution = component.lock_execution().await;
        if let Some(res) = should_return_early(&state, &execution, &component.moniker) {
            return res;
        }
    }

    let result = async move {
        // Resolve the component.
        let component_info = component.resolve().await.map_err(|err|
            StartActionError::ResolveActionError { moniker: component.moniker.clone(), err }
        )?;

        // Find the runner to use.
        let runner = component.resolve_runner().await.map_err(|err| {
            warn!(moniker = %component.moniker, %err, "Failed to resolve runner. A runner must be registered in a component's environment before being referenced. https://fuchsia.dev/go/components/runners#register");
            err
        })?;

        // Generate the Runtime which will be set in the Execution.
        let checker = component.policy_checker();
        let (pending_runtime, start_info, controller_server_end, break_on_start) =
            make_execution_runtime(
                &component,
                &checker,
                component_info.resolved_url.clone(),
                component_info.package,
                &component_info.decl,
                component_info.config,
                start_reason.clone(),
                execution_controller_task,
                numbered_handles,
                additional_namespace_entries,
            )
            .await?;

        Ok((
            StartContext {
                runner,
                start_info,
                controller_server_end,
            },
            component_info.decl,
            pending_runtime,
            break_on_start,
        ))
    }
    .await;

    // Dispatch the Started and the DebugStarted event.
    let (start_context, pending_runtime) = match result {
        Ok((start_context, component_decl, mut pending_runtime, break_on_start)) => {
            component
                .hooks
                .dispatch(&Event::new_with_timestamp(
                    component,
                    EventPayload::Started {
                        runtime: RuntimeInfo::from_runtime(&mut pending_runtime),
                        component_decl,
                    },
                    pending_runtime.timestamp,
                ))
                .await;
            component
                .hooks
                .dispatch(&Event::new_with_timestamp(
                    component,
                    EventPayload::DebugStarted {
                        runtime_dir: pending_runtime.runtime_dir.clone(),
                        break_on_start: Arc::new(break_on_start),
                    },
                    pending_runtime.timestamp,
                ))
                .await;
            (start_context, pending_runtime)
        }
        Err(e) => {
            return Err(e);
        }
    };

    let res = configure_component_runtime(&component, pending_runtime).await;
    match res {
        Ok(fsys::StartResult::AlreadyStarted) => {}
        Ok(fsys::StartResult::Started) => {
            // It's possible that the component is stopped before getting here. If so, that's fine: the
            // runner will start the component, but its stop or kill signal will be immediately set on the
            // component controller.
            start_context
                .runner
                .start(start_context.start_info, start_context.controller_server_end)
                .await;
        }
        Err(ref _e) => {
            // Since we dispatched a start event, dispatch a stop event
            // TODO(fxbug.dev/87507): It is possible this issues Stop after
            // Destroyed is issued.
            component
                .hooks
                .dispatch(&Event::new(component, EventPayload::Stopped { status: zx::Status::OK }))
                .await;
        }
    };
    res
}

/// Set the Runtime in the Execution and start the exit water. From component manager's
/// perspective, this indicates that the component has started. If this returns an error, the
/// component was shut down and the Runtime is not set, otherwise the function returns the
/// start context with the runtime set. This function acquires the state and execution locks on
/// `Component`.
async fn configure_component_runtime(
    component: &Arc<ComponentInstance>,
    mut pending_runtime: Runtime,
) -> Result<fsys::StartResult, StartActionError> {
    let state = component.lock_state().await;
    let mut execution = component.lock_execution().await;

    if let Some(r) = should_return_early(&state, &execution, &component.moniker) {
        return r;
    }

    pending_runtime.watch_for_exit(component.as_weak());
    execution.runtime = Some(pending_runtime);
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
        InstanceState::New | InstanceState::Unresolved | InstanceState::Resolved(_) => {}
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

/// Returns a configured Runtime for a component and the start info (without actually starting
/// the component).
async fn make_execution_runtime(
    component: &Arc<ComponentInstance>,
    checker: &GlobalPolicyChecker,
    url: String,
    package: Option<Package>,
    decl: &cm_rust::ComponentDecl,
    config: Option<ConfigFields>,
    start_reason: StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    numbered_handles: Vec<fprocess::HandleInfo>,
    additional_namespace_entries: Vec<fcrunner::ComponentNamespaceEntry>,
) -> Result<
    (
        Runtime,
        fcrunner::ComponentStartInfo,
        ServerEnd<fcrunner::ComponentControllerMarker>,
        zx::EventPair,
    ),
    StartActionError,
> {
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

    // Create incoming/outgoing directories, and populate them.
    let (outgoing_dir_client, outgoing_dir_server) = zx::Channel::create();
    let (runtime_dir_client, runtime_dir_server) = zx::Channel::create();
    let mut namespace = IncomingNamespace::new(package);
    let original_ns = namespace.populate(component, decl).await.map_err(|err| {
        StartActionError::NamespacePopulateError { moniker: component.moniker.clone(), err }
    })?;
    let ns = merge_namespace_entries(original_ns, additional_namespace_entries).map_err(|err| {
        StartActionError::NamespacePopulateError { moniker: component.moniker.clone(), err }
    })?;

    let (controller_client, controller_server) =
        endpoints::create_endpoints::<fcrunner::ComponentControllerMarker>();
    let controller =
        controller_client.into_proxy().expect("failed to create ComponentControllerProxy");
    // Set up channels into/out of the new component. These are absent from non-executable
    // components.
    let outgoing_dir_client = decl.get_runner().map(|_| {
        fio::DirectoryProxy::from_channel(
            fasync::Channel::from_channel(outgoing_dir_client).unwrap(),
        )
    });
    let runtime_dir_client = decl.get_runner().map(|_| {
        fio::DirectoryProxy::from_channel(
            fasync::Channel::from_channel(runtime_dir_client).unwrap(),
        )
    });

    let encoded_config = if let Some(config) = config {
        let (vmo, size) = (|| {
            let encoded = config.encode_as_fidl_struct();
            let size = encoded.len() as u64;
            let vmo = Vmo::create(size)?;
            vmo.write(&encoded, 0)?;
            Ok((vmo, size))
        })()
        .map_err(|s| StartActionError::StructuredConfigError {
            err: StructuredConfigError::VmoCreateFailed(s),
            moniker: component.moniker.clone(),
        })?;
        Some(fmem::Data::Buffer(fmem::Buffer { vmo, size }))
    } else {
        None
    };

    let runtime = Runtime::start_from(
        Some(namespace),
        outgoing_dir_client,
        runtime_dir_client,
        Some(controller),
        start_reason,
        execution_controller_task,
    );
    let (break_on_start_left, break_on_start_right) = zx::EventPair::create();
    let start_info = fcrunner::ComponentStartInfo {
        resolved_url: Some(url),
        program: decl.program.as_ref().map(|p| p.info.clone()),
        ns: Some(ns),
        outgoing_dir: Some(ServerEnd::new(outgoing_dir_server)),
        runtime_dir: Some(ServerEnd::new(runtime_dir_server)),
        numbered_handles: if numbered_handles.is_empty() { None } else { Some(numbered_handles) },
        encoded_config,
        break_on_start: Some(break_on_start_left),
        ..Default::default()
    };

    Ok((runtime, start_info, controller_server, break_on_start_right))
}

fn merge_namespace_entries(
    original_entries: Vec<fcrunner::ComponentNamespaceEntry>,
    mut additional_entries: Vec<fcrunner::ComponentNamespaceEntry>,
) -> Result<Vec<fcrunner::ComponentNamespaceEntry>, NamespacePopulateError> {
    let mut output = vec![];
    for original_entry in original_entries {
        let mut conflicts_exist = false;
        for additional_entry in &additional_entries {
            conflicts_exist |=
                namespace_paths_conflict(&original_entry.path, &additional_entry.path);
        }
        if conflicts_exist {
            return Err(NamespacePopulateError::ConflictBetweenUsesAndAdditionalEntries);
        }
        output.push(original_entry);
    }
    output.append(&mut additional_entries);
    Ok(output)
}

fn namespace_paths_conflict(path_1: &Option<String>, path_2: &Option<String>) -> bool {
    match (path_1.as_ref(), path_2.as_ref()) {
        (Some(p1), Some(p2)) => p1.starts_with(p2) || p2.starts_with(p1),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::model::{
            actions::{
                start::should_return_early, ActionSet, ShutdownAction, StartAction, StopAction,
            },
            component::{
                ComponentInstance, ExecutionState, InstanceState, ResolvedInstanceState, Runtime,
                StartReason,
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
        fidl_fuchsia_sys2 as fsys, fuchsia, fuchsia_zircon as zx,
        moniker::Moniker,
        routing::resolving::ComponentAddress,
        std::sync::{Arc, Weak},
    };

    // Child name for test child components instantiated during tests.
    const TEST_CHILD_NAME: &str = "child";

    struct StartHook {
        component: Arc<ComponentInstance>,
    }

    #[async_trait]
    impl Hook for StartHook {
        async fn on(self: Arc<Self>, _event: &Event) -> Result<(), ModelError> {
            ActionSet::register(self.component.clone(), ShutdownAction::new())
                .await
                .expect("shutdown failed");
            Ok(())
        }
    }

    #[fuchsia::test]
    /// Validate that if a start action is issued and the component stops
    /// the action completes we see a Stop event emitted.
    async fn start_issues_stop() {
        let (test_topology, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let start_hook = Arc::new(StartHook { component: child.clone() });
        child
            .hooks
            .install(vec![HooksRegistration::new(
                "my_start_hook",
                vec![EventType::Started],
                Arc::downgrade(&start_hook) as Weak<dyn Hook>,
            )])
            .await;

        match ActionSet::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
        )
        .await
        {
            Err(StartActionError::InstanceShutDown { moniker: m }) => {
                assert_eq!(Moniker::try_from(vec![TEST_CHILD_NAME]).unwrap(), m);
            }
            e => panic!("Unexpected result from component start: {:?}", e),
        }

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
        assert!(should_return_early(&InstanceState::Unresolved, &es, &m).is_none());
        assert_matches!(
            should_return_early(&InstanceState::Destroyed, &es, &m),
            Some(Err(StartActionError::InstanceDestroyed { moniker: _ }))
        );
        let (_, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let decl = ComponentDeclBuilder::new().add_lazy_child("bar").build();
        let ris = ResolvedInstanceState::new(
            &child,
            decl,
            None,
            None,
            ComponentAddress::from_absolute_url(&child.component_url).unwrap(),
            None,
        )
        .await
        .unwrap();
        assert!(should_return_early(&InstanceState::Resolved(ris), &es, &m).is_none());

        // Check for already_started:
        {
            let mut es = ExecutionState::new();
            es.runtime =
                Some(Runtime::start_from(None, None, None, None, StartReason::Debug, None));
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
