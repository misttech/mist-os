// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::builtin_environment::BuiltinEnvironment;
use crate::model::actions::{
    ActionsManager, ShutdownAction, ShutdownType, StartAction, StopAction,
};
use crate::model::component::instance::InstanceState;
use crate::model::component::{ComponentInstance, IncomingCapabilities, StartReason};
use crate::model::events::registry::EventSubscription;
use crate::model::model::Model;
use crate::model::start::Start;
use crate::model::testing::mocks::*;
use crate::model::testing::out_dir::OutDir;
use crate::model::testing::routing_test_helpers::RoutingTestBuilder;
use crate::model::testing::test_helpers::*;
use crate::model::testing::test_hook::{Lifecycle, TestHook};
use ::routing::policy::PolicyError;
use assert_matches::assert_matches;
use async_trait::async_trait;
use cm_config::AllowlistEntryBuilder;
use cm_rust::{
    Availability, ComponentDecl, RegistrationSource, RunnerRegistration, UseEventStreamDecl,
    UseSource,
};
use cm_rust_testing::*;
use cm_types::Name;
use errors::{ActionError, ModelError, StartActionError};
use fidl::endpoints::{create_endpoints, ProtocolMarker, ServerEnd};
use futures::channel::mpsc;
use futures::future::pending;
use futures::join;
use futures::lock::Mutex;
use futures::prelude::*;
use hooks::{Event, EventPayload, EventType, Hook, HooksRegistration};
use moniker::{ChildName, Moniker};
use std::collections::HashSet;
use std::sync::{Arc, Weak};
use test_case::test_case;
use vfs::directory::entry::OpenRequest;
use vfs::execution_scope::ExecutionScope;
use vfs::ToObjectRequest;
use zx::AsHandleRef;
use {
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_hardware_power_statecontrol as fstatecontrol, fidl_fuchsia_io as fio,
    fuchsia_async as fasync, fuchsia_sync as fsync,
};

async fn new_model(
    components: Vec<(&'static str, ComponentDecl)>,
) -> (Arc<Model>, Arc<Mutex<BuiltinEnvironment>>, Arc<MockRunner>) {
    new_model_with(components, vec![]).await
}

async fn new_model_with(
    components: Vec<(&'static str, ComponentDecl)>,
    additional_hooks: Vec<HooksRegistration>,
) -> (Arc<Model>, Arc<Mutex<BuiltinEnvironment>>, Arc<MockRunner>) {
    let TestModelResult { model, builtin_environment, mock_runner, .. } =
        TestEnvironmentBuilder::new()
            .set_components(components)
            .set_hooks(additional_hooks)
            .build()
            .await;
    (model, builtin_environment, mock_runner)
}

#[fuchsia::test]
async fn bind_root() {
    let (model, _builtin_environment, mock_runner) =
        new_model(vec![("root", component_decl_with_test_runner())]).await;
    let res = model.root().start_instance(&Moniker::root(), &StartReason::Root).await;
    assert!(res.is_ok());
    mock_runner.wait_for_url("test:///root_resolved").await;
    let actual_children = get_live_children(&model.root()).await;
    assert!(actual_children.is_empty());
}

#[fuchsia::test]
async fn bind_non_existent_root_child() {
    let (model, _builtin_environment, _mock_runner) =
        new_model(vec![("root", component_decl_with_test_runner())]).await;
    let m: Moniker = vec!["no-such-instance"].try_into().unwrap();
    let res = model.root().start_instance(&m, &StartReason::Root).await;
    let expected_res: Result<Arc<ComponentInstance>, ModelError> =
        Err(ModelError::instance_not_found(vec!["no-such-instance"].try_into().unwrap()));
    assert_eq!(format!("{:?}", res), format!("{:?}", expected_res));
}

// Blocks the Start action for the "system" component
pub struct StartBlocker {
    rx: Mutex<mpsc::Receiver<()>>,
}

impl StartBlocker {
    pub fn new() -> (Arc<Self>, mpsc::Sender<()>) {
        let (tx, rx) = mpsc::channel::<()>(0);
        let blocker = Arc::new(Self { rx: Mutex::new(rx) });
        (blocker, tx)
    }
}

#[async_trait]
impl Hook for StartBlocker {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        let moniker = event
            .target_moniker
            .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)
            .unwrap();
        let expected_moniker: Moniker = vec!["system"].try_into().unwrap();
        if moniker == &expected_moniker {
            let mut rx = self.rx.lock().await;
            rx.next().await.unwrap();
        }
        Ok(())
    }
}

#[fuchsia::test]
async fn bind_concurrent() {
    // Test binding twice concurrently to the same component. The component should only be
    // started once.
    let (blocker, mut unblocker) = StartBlocker::new();
    let (model, _builtin_environment, mock_runner) = new_model_with(
        vec![
            ("root", ComponentDeclBuilder::new().child_default("system").build()),
            ("system", component_decl_with_test_runner()),
        ],
        vec![HooksRegistration::new(
            "start_blocker",
            vec![EventType::Started],
            Arc::downgrade(&blocker) as Weak<dyn Hook>,
        )],
    )
    .await;

    // Start the root component.
    model.start().await;

    // Attempt to start the "system" component
    let system_component = model.root().find(&vec!["system"].try_into().unwrap()).await.unwrap();
    let first_start = system_component
        .actions()
        .register_no_wait(StartAction::new(
            StartReason::Debug,
            None,
            IncomingCapabilities::default(),
        ))
        .await;

    // While the first start is paused, simulate a second start by explicitly scheduling a second
    // Start action. This should just be deduplicated to the first start by the action system.
    let second_start = system_component
        .actions()
        .register_no_wait(StartAction::new(
            StartReason::Debug,
            None,
            IncomingCapabilities::default(),
        ))
        .await;

    // Unblock the start hook, then check the result of both starts.
    unblocker.try_send(()).unwrap();

    // The first and second start results must both be successful.
    first_start.await.expect("first start failed");
    second_start.await.expect("second start failed");

    // Verify that the component was started only once.
    mock_runner.wait_for_urls(&["test:///system_resolved"]).await;
}

#[fuchsia::test]
async fn bind_parent_then_child() {
    let hook = Arc::new(TestHook::new());
    let (model, _builtin_environment, mock_runner) = new_model_with(
        vec![
            (
                "root",
                ComponentDeclBuilder::new().child_default("system").child_default("echo").build(),
            ),
            ("system", component_decl_with_test_runner()),
            ("echo", component_decl_with_test_runner()),
        ],
        hook.hooks(),
    )
    .await;
    let root = model.root();

    // Start the system.
    let m: Moniker = vec!["system"].try_into().unwrap();
    assert!(root.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///system_resolved"]).await;

    // Validate children. system is resolved, but not echo.
    let actual_children = get_live_children(&*root).await;
    let mut expected_children: HashSet<ChildName> = HashSet::new();
    expected_children.insert("system".try_into().unwrap());
    expected_children.insert("echo".try_into().unwrap());
    assert_eq!(actual_children, expected_children);

    let system_component = get_live_child(&*root, "system").await;
    let echo_component = get_live_child(&*root, "echo").await;
    let actual_children = get_live_children(&*system_component).await;
    assert!(actual_children.is_empty());
    assert_matches!(*echo_component.lock_state().await, InstanceState::Unresolved(_));
    // Start echo.
    let m: Moniker = vec!["echo"].try_into().unwrap();
    assert!(root.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///system_resolved", "test:///echo_resolved"]).await;

    // Validate children. Now echo is resolved.
    let echo_component = get_live_child(&*root, "echo").await;
    let actual_children = get_live_children(&*echo_component).await;
    assert!(actual_children.is_empty());

    // Verify that the component topology matches expectations.
    assert_eq!("(echo,system)", hook.print());
}

#[fuchsia::test]
async fn bind_child_doesnt_bind_parent() {
    let hook = Arc::new(TestHook::new());
    let (model, _builtin_environment, mock_runner) = new_model_with(
        vec![
            ("root", ComponentDeclBuilder::new().child_default("system").build()),
            (
                "system",
                ComponentDeclBuilder::new()
                    .child_default("logger")
                    .child_default("netstack")
                    .build(),
            ),
            ("logger", component_decl_with_test_runner()),
            ("netstack", component_decl_with_test_runner()),
        ],
        hook.hooks(),
    )
    .await;
    let root = model.root();

    // Start logger (before ever starting system).
    let m: Moniker = vec!["system", "logger"].try_into().unwrap();
    assert!(root.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///logger_resolved"]).await;

    // Start netstack.
    let m: Moniker = vec!["system", "netstack"].try_into().unwrap();
    assert!(root.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///logger_resolved", "test:///netstack_resolved"]).await;

    // Finally, start the system.
    let m: Moniker = vec!["system"].try_into().unwrap();
    assert!(root.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner
        .wait_for_urls(&[
            "test:///system_resolved",
            "test:///logger_resolved",
            "test:///netstack_resolved",
        ])
        .await;

    // validate the component topology.
    assert_eq!("(system(logger,netstack))", hook.print());
}

#[fuchsia::test]
async fn bind_child_non_existent() {
    let (model, _builtin_environment, mock_runner) = new_model(vec![
        ("root", ComponentDeclBuilder::new().child_default("system").build()),
        ("system", component_decl_with_test_runner()),
    ])
    .await;
    let root = model.root();

    // Start the system.
    let m: Moniker = vec!["system"].try_into().unwrap();
    assert!(root.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///system_resolved"]).await;

    // Can't start the logger. It does not exist.
    let m: Moniker = vec!["system", "logger"].try_into().unwrap();
    let res = root.start_instance(&m, &StartReason::Root).await;
    let expected_res: Result<(), ModelError> = Err(ModelError::instance_not_found(m));
    assert_eq!(format!("{:?}", res), format!("{:?}", expected_res));
    mock_runner.wait_for_urls(&["test:///system_resolved"]).await;
}

/// Create a hierarchy of children:
///
///   a
///  / \
/// b   c
///      \
///       d
///        \
///         e
///
/// `b`, `c`, and `d` are started eagerly. `a` and `e` are lazy.
#[fuchsia::test]
async fn bind_eager_children() {
    let hook = Arc::new(TestHook::new());
    let (model, _builtin_environment, mock_runner) = new_model_with(
        vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager())
                    .child(ChildBuilder::new().name("c").eager())
                    .build(),
            ),
            ("b", component_decl_with_test_runner()),
            ("c", ComponentDeclBuilder::new().child(ChildBuilder::new().name("d").eager()).build()),
            ("d", ComponentDeclBuilder::new().child_default("e").build()),
            ("e", component_decl_with_test_runner()),
        ],
        hook.hooks(),
    )
    .await;

    // Start the top component, and check that it and the eager components were started.
    {
        let m = Moniker::parse_str("/a").unwrap();
        let res = model.root().start_instance(&m, &StartReason::Root).await;
        assert!(res.is_ok());
        mock_runner
            .wait_for_urls(&[
                "test:///a_resolved",
                "test:///b_resolved",
                "test:///c_resolved",
                "test:///d_resolved",
            ])
            .await;
    }
    // Verify that the topology of started components matches expectations.
    assert_eq!("(a(b,c(d)))", hook.print());
}

/// `b` is an eager child of `a` that uses a runner provided by `a`. In the process of binding
/// to `a`, `b` will be eagerly started, which requires re-binding to `a`. This should work
/// without causing reentrance issues.
#[fuchsia::test]
async fn bind_eager_children_reentrant() {
    let hook = Arc::new(TestHook::new());
    let (model, _builtin_environment, mock_runner) = new_model_with(
        vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(
                        ChildBuilder::new()
                            .name("b")
                            .url("test:///b")
                            .startup(fdecl::StartupMode::Eager)
                            .environment("env"),
                    )
                    .runner_default("foo")
                    .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                        source_name: "foo".parse().unwrap(),
                        source: RegistrationSource::Self_,
                        target_name: "foo".parse().unwrap(),
                    }))
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("foo").build()),
        ],
        hook.hooks(),
    )
    .await;

    // Set up the runner.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let mut out_dir = OutDir::new();
    out_dir.add_entry(
        format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
        runner_service,
    );
    mock_runner.add_host_fn("test:///a_resolved", out_dir.host_fn());

    // Start the top component, and check that it and the eager components were started.
    {
        let (f, bind_handle) = async move {
            let m = Moniker::parse_str("/a").unwrap();
            model.root().start_instance(&m, &StartReason::Root).await
        }
        .remote_handle();
        fasync::Task::spawn(f).detach();
        // `b` uses the runner offered by `a`.
        assert_eq!(
            wait_for_runner_request(&mut receiver).await.resolved_url,
            Some("test:///b_resolved".to_string())
        );
        bind_handle.await.expect("start `a` failed");
        // `root` and `a` use the test runner.
        mock_runner.wait_for_urls(&["test:///a_resolved"]).await;
    }
    // Verify that the topology of started components matches expectations.
    assert_eq!("(a(b))", hook.print());
}

#[fuchsia::test]
async fn bind_no_execute() {
    // Create a non-executable component with an eagerly-started child.
    let (model, _builtin_environment, mock_runner) = new_model(vec![
        ("root", ComponentDeclBuilder::new().child_default("a").build()),
        (
            "a",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("b").eager())
                .build(),
        ),
        ("b", component_decl_with_test_runner()),
    ])
    .await;

    // Start the parent component. The child should be started. However, the parent component
    // is non-executable so it is not run.
    let m: Moniker = vec!["a"].try_into().unwrap();
    assert!(model.root().start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///b_resolved"]).await;
}

#[fuchsia::test]
async fn bind_action_sequence() {
    // Test that binding registers the expected actions in the expected sequence
    // (Discover -> Resolve -> Start).

    // Set up the tree.
    let (model, builtin_environment, _mock_runner) = new_model(vec![
        ("root", ComponentDeclBuilder::new().child_default("system").build()),
        ("system", component_decl_with_test_runner()),
    ])
    .await;
    let events = vec![EventType::Resolved.into(), EventType::Started.into()];
    let mut event_source =
        builtin_environment.lock().await.event_source_factory.create_for_above_root();
    let mut event_stream = event_source
        .subscribe(
            events
                .into_iter()
                .map(|event: Name| EventSubscription {
                    event_name: UseEventStreamDecl {
                        source_name: event,
                        source: UseSource::Parent,
                        scope: None,
                        target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                        filter: None,
                        availability: Availability::Required,
                    },
                })
                .collect(),
        )
        .await
        .expect("subscribe to event stream");

    // Child of root should start out discovered but not resolved yet.
    let m = Moniker::parse_str("/system").unwrap();
    model.start().await;
    event_stream.wait_until(EventType::Resolved, vec![].try_into().unwrap()).await.unwrap();
    event_stream.wait_until(EventType::Started, vec![].try_into().unwrap()).await.unwrap();

    // Start child and check that it gets resolved, with a Resolve event and action.
    model.root().start_instance(&m, &StartReason::Root).await.unwrap();
    event_stream.wait_until(EventType::Resolved, m.clone()).await.unwrap();

    // Check that the child is started, with a Start event and action.
    event_stream.wait_until(EventType::Started, m.clone()).await.unwrap();
}

#[fuchsia::test]
async fn reboot_on_terminate_disallowed() {
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("system").on_terminate(fdecl::OnTerminate::Reboot))
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("other").build()])
        .build()
        .await;

    let res = test
        .model
        .root()
        .start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug)
        .await;
    let expected_moniker = Moniker::try_from(vec!["system"]).unwrap();
    assert_matches!(res, Err(ModelError::ActionError {
        err: ActionError::StartError {
            err: StartActionError::RebootOnTerminateForbidden {
                err: PolicyError::ChildPolicyDisallowed {
                    policy,
                    moniker: m2
                },
                moniker: m1
            }
        }
    }) if &policy == "reboot_on_terminate" && m1 == expected_moniker && m2 == expected_moniker);
}

const REBOOT_PROTOCOL: &str = fstatecontrol::AdminMarker::DEBUG_NAME;

#[fuchsia::test]
async fn on_terminate_stop_triggers_reboot() {
    // Create a topology with a reboot-on-terminate component and a fake reboot protocol
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("system").on_terminate(fdecl::OnTerminate::Reboot))
                .protocol_default(REBOOT_PROTOCOL)
                .expose(
                    ExposeBuilder::protocol()
                        .name(REBOOT_PROTOCOL)
                        .source(cm_rust::ExposeSource::Self_),
                )
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let (reboot_service, mut receiver) =
        create_service_directory_entry::<fstatecontrol::AdminMarker>();
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("system").build()])
        .add_outgoing_path("root", reboot_protocol_path.parse().unwrap(), reboot_service)
        .build()
        .await;
    let root = test.model.root();

    // First stop of the critical component exits cleanly and so does not trigger a reboot.
    test.mock_runner.add_controller_response(
        "test:///system_resolved",
        Box::new(|| ControllerActionResponse {
            close_channel: true,
            delay: None,
            termination_status: Some(zx::Status::OK),
            exit_code: Some(0),
        }),
    );
    root.start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug).await.unwrap();
    let component = root.find_and_maybe_resolve(&vec!["system"].try_into().unwrap()).await.unwrap();
    ActionsManager::register(component.clone(), StopAction::new(false)).await.unwrap();
    assert!(!test.model.top_instance().has_reboot_task());

    // Second stop of the critical component does not exit cleanly and so does trigger a reboot.
    test.mock_runner.add_controller_response(
        "test:///system_resolved",
        Box::new(|| ControllerActionResponse {
            close_channel: true,
            delay: None,
            termination_status: Some(zx::Status::OK),
            exit_code: Some(1),
        }),
    );
    root.start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug).await.unwrap();
    let component = root.find_and_maybe_resolve(&vec!["system"].try_into().unwrap()).await.unwrap();
    let stop = async move {
        ActionsManager::register(component.clone(), StopAction::new(false)).await.unwrap();
    };
    let recv_reboot = async move {
        let reason = match receiver.next().await.unwrap() {
            fstatecontrol::AdminRequest::Reboot { reason, .. } => reason,
            _ => panic!("unexpected request"),
        };
        assert_eq!(reason, fstatecontrol::RebootReason::CriticalComponentFailure);
    };
    join!(stop, recv_reboot);
    assert!(test.model.top_instance().has_reboot_task());
}

#[fuchsia::test]
async fn on_terminate_exit_triggers_reboot() {
    // Create a topology with a reboot component and a fake reboot protocol
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("system").on_terminate(fdecl::OnTerminate::Reboot))
                .protocol_default(REBOOT_PROTOCOL)
                .expose(
                    ExposeBuilder::protocol()
                        .name(REBOOT_PROTOCOL)
                        .source(cm_rust::ExposeSource::Self_),
                )
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let (reboot_service, mut receiver) =
        create_service_directory_entry::<fstatecontrol::AdminMarker>();
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("system").build()])
        .add_outgoing_path("root", reboot_protocol_path.parse().unwrap(), reboot_service)
        .build()
        .await;
    let root = test.model.root();

    // Start the critical component and cause it to 'exit' by making the runner close its end
    // of the controller channel. This should cause the Admin protocol to receive a reboot request.
    test.start_instance_and_wait_start(&vec!["system"].try_into().unwrap()).await.unwrap();
    let component = root.find_and_maybe_resolve(&vec!["system"].try_into().unwrap()).await.unwrap();
    let info = ComponentInfo::new(component.clone()).await;
    test.mock_runner.wait_for_url("test:///system_resolved").await;
    test.mock_runner.abort_controller(&info.channel_id);
    let reason = match receiver.next().await.unwrap() {
        fstatecontrol::AdminRequest::Reboot { reason, .. } => reason,
        _ => panic!("unexpected request"),
    };
    assert_eq!(reason, fstatecontrol::RebootReason::CriticalComponentFailure);
    assert!(test.model.top_instance().has_reboot_task());
}

#[fuchsia::test]
async fn reboot_shutdown_does_not_trigger_reboot() {
    // Create a topology with a reboot-on-terminate component and a fake reboot protocol
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("system").on_terminate(fdecl::OnTerminate::Reboot))
                .protocol_default(REBOOT_PROTOCOL)
                .expose(
                    ExposeBuilder::protocol()
                        .name(REBOOT_PROTOCOL)
                        .source(cm_rust::ExposeSource::Self_),
                )
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let (reboot_service, _receiver) =
        create_service_directory_entry::<fstatecontrol::AdminMarker>();
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("system").build()])
        .add_outgoing_path("root", reboot_protocol_path.parse().unwrap(), reboot_service)
        .build()
        .await;
    let root = test.model.root();

    // Start the critical component and make it stop. This should cause the Admin protocol to
    // receive a reboot request.
    root.start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug).await.unwrap();
    let component = root.find_and_maybe_resolve(&vec!["system"].try_into().unwrap()).await.unwrap();
    ActionsManager::register(component.clone(), ShutdownAction::new(ShutdownType::Instance))
        .await
        .unwrap();
    assert!(!test.model.top_instance().has_reboot_task());
}

#[fuchsia::test]
#[should_panic(expected = "Component with on_terminate=REBOOT terminated, but triggering \
                          reboot failed. Crashing component_manager instead: \
                          StateControl Admin FIDL:\n\tA FIDL client's channel to the protocol \
                          fuchsia.hardware.power.statecontrol.Admin was closed: NOT_FOUND")]
async fn on_terminate_with_missing_reboot_protocol_panics() {
    // Create a topology with a reboot-on-terminate component but no reboot protocol routed to root.
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("system").on_terminate(fdecl::OnTerminate::Reboot))
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("system").build()])
        .build()
        .await;
    let root = test.model.root();

    // Start the critical component and cause it to 'exit' by making the runner close its end of
    // the controller channel. component_manager should attempt to send a reboot request, which
    // should fail because the reboot protocol isn't exposed to it -- expect component_manager to
    // respond by crashing.
    test.start_instance_and_wait_start(&vec!["system"].try_into().unwrap()).await.unwrap();
    let component = root.find_and_maybe_resolve(&vec!["system"].try_into().unwrap()).await.unwrap();
    let info = ComponentInfo::new(component.clone()).await;
    test.mock_runner.wait_for_url("test:///system_resolved").await;
    test.mock_runner.abort_controller(&info.channel_id);
    let () = pending().await;
}

#[fuchsia::test]
#[should_panic(expected = "Component with on_terminate=REBOOT terminated, but triggering \
                          reboot failed. Crashing component_manager instead: \
                          StateControl Admin: INTERNAL")]
async fn on_terminate_with_failed_reboot_panics() {
    // Create a topology with a reboot-on-terminate component and a fake reboot protocol
    const REBOOT_PROTOCOL: &str = fstatecontrol::AdminMarker::DEBUG_NAME;
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .child(
                    ChildBuilder::new()
                        .name("system")
                        .on_terminate(fdecl::OnTerminate::Reboot)
                        .build(),
                )
                .protocol_default(REBOOT_PROTOCOL)
                .expose(
                    ExposeBuilder::protocol()
                        .name(REBOOT_PROTOCOL)
                        .source(cm_rust::ExposeSource::Self_),
                )
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let (reboot_service, mut receiver) =
        create_service_directory_entry::<fstatecontrol::AdminMarker>();
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("system").build()])
        .add_outgoing_path("root", reboot_protocol_path.parse().unwrap(), reboot_service)
        .build()
        .await;
    let root = test.model.root();

    // Start the critical component and cause it to 'exit' by making the runner close its end
    // of the controller channel. Admin protocol should receive a reboot request -- make it fail
    // and expect component_manager to respond by crashing.
    test.start_instance_and_wait_start(&vec!["system"].try_into().unwrap()).await.unwrap();
    let component = root.find_and_maybe_resolve(&vec!["system"].try_into().unwrap()).await.unwrap();
    let info = ComponentInfo::new(component.clone()).await;
    test.mock_runner.wait_for_url("test:///system_resolved").await;
    test.mock_runner.abort_controller(&info.channel_id);
    match receiver.next().await.unwrap() {
        fstatecontrol::AdminRequest::Reboot { responder, .. } => {
            responder.send(Err(zx::sys::ZX_ERR_INTERNAL)).unwrap();
        }
        _ => panic!("unexpected request"),
    };
    let () = pending().await;
}

/// If a component escrows its outgoing directory and stops, it should be started again,
/// and it should get back the queued open requests.
#[fuchsia::test(allow_stalls = false)]
async fn open_then_stop_with_escrow() {
    let (out_dir_tx, mut out_dir_rx) = mpsc::channel(1);
    let out_dir_tx = fsync::Mutex::new(out_dir_tx);

    // Create and start a component.
    let components = vec![("root", ComponentDeclBuilder::new().build())];
    let url = "test:///root_resolved";
    let test = ActionsTest::new(components[0].0, components, None).await;
    test.runner.add_host_fn(
        url,
        Box::new(move |server_end: ServerEnd<fio::DirectoryMarker>| {
            out_dir_tx.lock().try_send(server_end).unwrap();
        }),
    );
    let root = test.model.root();
    root.ensure_started(&StartReason::Debug).await.unwrap();
    test.runner.wait_for_url(url).await;

    // Queue an open request.
    let (client_end, server_end) = create_endpoints::<fio::DirectoryMarker>();
    let execution_scope = ExecutionScope::new();
    let mut object_request = fio::OpenFlags::empty().to_object_request(server_end);
    root.open_outgoing(OpenRequest::new(
        execution_scope.clone(),
        fio::OpenFlags::empty(),
        "echo".try_into().unwrap(),
        &mut object_request,
    ))
    .await
    .unwrap();

    // Get a hold of the outgoing directory server endpoint.
    let outgoing_server_end = out_dir_rx.next().await.unwrap();

    // Escrow the outgoing directory, then have the program stop itself.
    let info = ComponentInfo::new(root.clone()).await;
    test.runner.send_on_escrow(
        &info.channel_id,
        fcrunner::ComponentControllerOnEscrowRequest {
            outgoing_dir: Some(outgoing_server_end),
            ..Default::default()
        },
    );
    test.runner.reset_wait_for_url(url);
    test.runner.abort_controller(&info.channel_id);

    // We should observe the program getting started again.
    test.runner.wait_for_url(url).await;
    _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
    let events: Vec<_> = test
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
            Lifecycle::Start(vec![].try_into().unwrap()),
            Lifecycle::Stop(vec![].try_into().unwrap()),
            Lifecycle::Start(vec![].try_into().unwrap()),
        ]
    );

    // And we should get back the same outgoing directory, with that earlier request in it.
    let outgoing_server_end = out_dir_rx.next().await.unwrap();
    let mut out_dir = OutDir::new();
    let (request_tx, mut request_rx) = mpsc::channel(1);
    let request_tx = fsync::Mutex::new(request_tx);
    out_dir.add_entry(
        "/echo".parse().unwrap(),
        vfs::service::endpoint(move |_scope, server_end| {
            request_tx.lock().try_send(server_end).unwrap();
        }),
    );
    out_dir.host_fn()(outgoing_server_end);
    let server_end = request_rx.next().await.unwrap();
    assert_eq!(
        client_end.basic_info().unwrap().related_koid,
        server_end.basic_info().unwrap().koid
    );
}

#[test_case(0)]
#[test_case(-1000)]
#[test_case(123456)]
#[fuchsia::test]
async fn stop_with_exit_code(expected_code: i64) {
    // Build test realm.
    let (model, builtin_environment, mock_runner) =
        new_model(vec![("root", component_decl_with_test_runner())]).await;
    let events = vec![EventType::Started.into(), EventType::Stopped.into()];
    let mut event_source =
        builtin_environment.lock().await.event_source_factory.create_for_above_root();
    let mut event_stream = event_source
        .subscribe(
            events
                .into_iter()
                .map(|event: Name| EventSubscription {
                    event_name: UseEventStreamDecl {
                        source_name: event,
                        source: UseSource::Parent,
                        scope: None,
                        target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                        filter: None,
                        availability: Availability::Required,
                    },
                })
                .collect(),
        )
        .await
        .expect("subscribe to event stream");

    model.start().await;
    event_stream.wait_until(EventType::Started, Moniker::root()).await.unwrap();
    let url = "test:///root_resolved";
    mock_runner.wait_for_url(url).await;

    // Stop the root component with an exit code.
    let root = model.root();
    let info = ComponentInfo::new(root.clone()).await;
    mock_runner.send_on_stop_info(
        &info.channel_id,
        fcrunner::ComponentStopInfo { exit_code: Some(expected_code), ..Default::default() },
    );
    root.stop().await.unwrap();

    // Assert that the event stream contains the exit code.
    let stopped_event = event_stream.wait_until(EventType::Stopped, Moniker::root()).await.unwrap();
    assert_matches!(
        stopped_event.event.payload,
        EventPayload::Stopped { status, exit_code: Some(exit_code), .. }
        if status == zx::Status::OK && exit_code == expected_code
    );
}
