// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        builtin_environment::BuiltinEnvironment,
        model::{
            actions::{ActionSet, ShutdownAction, StartAction, StopAction},
            component::{ComponentInstance, InstanceState, StartReason},
            error::{ModelError, StartActionError},
            events::registry::EventSubscription,
            hooks::{Event, EventType, Hook, HooksRegistration},
            model::Model,
            testing::{
                mocks::*, out_dir::OutDir, routing_test_helpers::RoutingTestBuilder,
                test_helpers::*, test_hook::TestHook,
            },
        },
    },
    ::routing::{config::AllowlistEntryBuilder, policy::PolicyError},
    assert_matches::assert_matches,
    async_trait::async_trait,
    cm_rust::{
        Availability, ComponentDecl, RegistrationSource, RunnerDecl, RunnerRegistration,
        UseEventStreamDecl, UseSource,
    },
    cm_rust_testing::*,
    cm_types::Name,
    fidl::endpoints::ProtocolMarker,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_hardware_power_statecontrol as fstatecontrol, fidl_fuchsia_sys2 as fsys,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::{channel::mpsc, future::pending, join, lock::Mutex, prelude::*},
    moniker::{ChildMoniker, Moniker, MonikerBase},
    std::sync::{Arc, Weak},
    std::{collections::HashSet, convert::TryFrom},
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
        TestEnvironmentBuilder::new().set_components(components).build().await;
    model.root().hooks.install(additional_hooks).await;
    (model, builtin_environment, mock_runner)
}

#[fuchsia::test]
async fn bind_root() {
    let (model, _builtin_environment, mock_runner) =
        new_model(vec![("root", component_decl_with_test_runner())]).await;
    let m: Moniker = Moniker::root();
    let res = model.start_instance(&m, &StartReason::Root).await;
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
    let res = model.start_instance(&m, &StartReason::Root).await;
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
            ("root", ComponentDeclBuilder::new().add_lazy_child("system").build()),
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
    let system_component = model.find(&vec!["system"].try_into().unwrap()).await.unwrap();
    let first_start = {
        let mut actions = system_component.lock_actions().await;
        actions.register_no_wait(
            &system_component,
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
        )
    };

    // While the first start is paused, simulate a second start by explicitly scheduling a second
    // Start action. This should just be deduplicated to the first start by the action system.
    let second_start = {
        let mut actions = system_component.lock_actions().await;
        actions.register_no_wait(
            &system_component,
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
        )
    };

    // Unblock the start hook, then check the result of both starts.
    unblocker.try_send(()).unwrap();

    // The first and second start results must:
    // * be the same
    // * be successful
    // * indicate that the component was just started
    let first_result = first_start.await.expect("first start failed");
    let second_result = second_start.await.expect("second start failed");
    assert_eq!(first_result, fsys::StartResult::Started);
    assert_eq!(first_result, second_result);

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
                ComponentDeclBuilder::new().add_lazy_child("system").add_lazy_child("echo").build(),
            ),
            ("system", component_decl_with_test_runner()),
            ("echo", component_decl_with_test_runner()),
        ],
        hook.hooks(),
    )
    .await;
    // Start the system.
    let m: Moniker = vec!["system"].try_into().unwrap();
    assert!(model.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///system_resolved"]).await;

    // Validate children. system is resolved, but not echo.
    let actual_children = get_live_children(&*model.root()).await;
    let mut expected_children: HashSet<ChildMoniker> = HashSet::new();
    expected_children.insert("system".try_into().unwrap());
    expected_children.insert("echo".try_into().unwrap());
    assert_eq!(actual_children, expected_children);

    let system_component = get_live_child(&*model.root(), "system").await;
    let echo_component = get_live_child(&*model.root(), "echo").await;
    let actual_children = get_live_children(&*system_component).await;
    assert!(actual_children.is_empty());
    assert_matches!(
        *echo_component.lock_state().await,
        InstanceState::New | InstanceState::Unresolved
    );
    // Start echo.
    let m: Moniker = vec!["echo"].try_into().unwrap();
    assert!(model.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///system_resolved", "test:///echo_resolved"]).await;

    // Validate children. Now echo is resolved.
    let echo_component = get_live_child(&*model.root(), "echo").await;
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
            ("root", ComponentDeclBuilder::new().add_lazy_child("system").build()),
            (
                "system",
                ComponentDeclBuilder::new()
                    .add_lazy_child("logger")
                    .add_lazy_child("netstack")
                    .build(),
            ),
            ("logger", component_decl_with_test_runner()),
            ("netstack", component_decl_with_test_runner()),
        ],
        hook.hooks(),
    )
    .await;

    // Start logger (before ever starting system).
    let m: Moniker = vec!["system", "logger"].try_into().unwrap();
    assert!(model.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///logger_resolved"]).await;

    // Start netstack.
    let m: Moniker = vec!["system", "netstack"].try_into().unwrap();
    assert!(model.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///logger_resolved", "test:///netstack_resolved"]).await;

    // Finally, start the system.
    let m: Moniker = vec!["system"].try_into().unwrap();
    assert!(model.start_instance(&m, &StartReason::Root).await.is_ok());
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
        ("root", ComponentDeclBuilder::new().add_lazy_child("system").build()),
        ("system", component_decl_with_test_runner()),
    ])
    .await;
    // Start the system.
    let m: Moniker = vec!["system"].try_into().unwrap();
    assert!(model.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///system_resolved"]).await;

    // Can't start the logger. It does not exist.
    let m: Moniker = vec!["system", "logger"].try_into().unwrap();
    let res = model.start_instance(&m, &StartReason::Root).await;
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
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_eager_child("b").add_eager_child("c").build()),
            ("b", component_decl_with_test_runner()),
            ("c", ComponentDeclBuilder::new().add_eager_child("d").build()),
            ("d", ComponentDeclBuilder::new().add_lazy_child("e").build()),
            ("e", component_decl_with_test_runner()),
        ],
        hook.hooks(),
    )
    .await;

    // Start the top component, and check that it and the eager components were started.
    {
        let m = Moniker::parse_str("/a").unwrap();
        let res = model.start_instance(&m, &StartReason::Root).await;
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
    // Verify that the component topology matches expectations.
    assert_eq!("(a(b,c(d(e))))", hook.print());
}

/// `b` is an eager child of `a` that uses a runner provided by `a`. In the process of binding
/// to `a`, `b` will be eagerly started, which requires re-binding to `a`. This should work
/// without causing reentrance issues.
#[fuchsia::test]
async fn bind_eager_children_reentrant() {
    let hook = Arc::new(TestHook::new());
    let (model, _builtin_environment, mock_runner) = new_model_with(
        vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .add_child(
                        ChildDeclBuilder::new()
                            .name("b")
                            .url("test:///b")
                            .startup(fdecl::StartupMode::Eager)
                            .environment("env")
                            .build(),
                    )
                    .runner(RunnerDecl {
                        name: "foo".parse().unwrap(),
                        source_path: Some("/svc/runner".parse().unwrap()),
                    })
                    .add_environment(
                        EnvironmentDeclBuilder::new()
                            .extends(fdecl::EnvironmentExtends::Realm)
                            .name("env")
                            .add_runner(RunnerRegistration {
                                source_name: "foo".parse().unwrap(),
                                source: RegistrationSource::Self_,
                                target_name: "foo".parse().unwrap(),
                            })
                            .build(),
                    )
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().add_program("foo").build()),
        ],
        hook.hooks(),
    )
    .await;

    // Set up the runner.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let mut out_dir = OutDir::new();
    out_dir.add_entry("/svc/runner".parse().unwrap(), runner_service);
    mock_runner.add_host_fn("test:///a_resolved", out_dir.host_fn());

    // Start the top component, and check that it and the eager components were started.
    {
        let (f, bind_handle) = async move {
            let m = Moniker::parse_str("/a").unwrap();
            model.start_instance(&m, &StartReason::Root).await
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
    // Verify that the component topology matches expectations.
    assert_eq!("(a(b))", hook.print());
}

#[fuchsia::test]
async fn bind_no_execute() {
    // Create a non-executable component with an eagerly-started child.
    let (model, _builtin_environment, mock_runner) = new_model(vec![
        ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
        ("a", ComponentDeclBuilder::new_empty_component().add_eager_child("b").build()),
        ("b", component_decl_with_test_runner()),
    ])
    .await;

    // Start the parent component. The child should be started. However, the parent component
    // is non-executable so it is not run.
    let m: Moniker = vec!["a"].try_into().unwrap();
    assert!(model.start_instance(&m, &StartReason::Root).await.is_ok());
    mock_runner.wait_for_urls(&["test:///b_resolved"]).await;
}

#[fuchsia::test]
async fn bind_action_sequence() {
    // Test that binding registers the expected actions in the expected sequence
    // (Discover -> Resolve -> Start).

    // Set up the tree.
    let (model, builtin_environment, _mock_runner) = new_model(vec![
        ("root", ComponentDeclBuilder::new().add_lazy_child("system").build()),
        ("system", component_decl_with_test_runner()),
    ])
    .await;
    let events =
        vec![EventType::Discovered.into(), EventType::Resolved.into(), EventType::Started.into()];
    let mut event_source = builtin_environment
        .lock()
        .await
        .event_source_factory
        .create_for_above_root()
        .await
        .unwrap();
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
    event_stream.wait_until(EventType::Discovered, m.clone()).await.unwrap();
    event_stream.wait_until(EventType::Started, vec![].try_into().unwrap()).await.unwrap();

    // Start child and check that it gets resolved, with a Resolve event and action.
    model.start_instance(&m, &StartReason::Root).await.unwrap();
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
                .add_child(
                    ChildDeclBuilder::new_lazy_child("system")
                        .on_terminate(fdecl::OnTerminate::Reboot)
                        .build(),
                )
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("other").build()])
        .build()
        .await;

    let res =
        test.model.start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug).await;
    let expected_moniker = Moniker::try_from(vec!["system"]).unwrap();
    assert_matches!(res, Err(ModelError::StartActionError {
        err: StartActionError::RebootOnTerminateForbidden {
            err: PolicyError::ChildPolicyDisallowed {
                policy,
                moniker: m2
            },
            moniker: m1
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
                .add_child(
                    ChildDeclBuilder::new_lazy_child("system")
                        .on_terminate(fdecl::OnTerminate::Reboot)
                        .build(),
                )
                .protocol(
                    ProtocolDeclBuilder::new(REBOOT_PROTOCOL).path(&reboot_protocol_path).build(),
                )
                .expose(cm_rust::ExposeDecl::Protocol(cm_rust::ExposeProtocolDecl {
                    source: cm_rust::ExposeSource::Self_,
                    source_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target: cm_rust::ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
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

    // Start the critical component and make it stop. This should cause the Admin protocol to
    // receive a reboot request.
    test.model
        .start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug)
        .await
        .unwrap();
    let component = test.model.look_up(&vec!["system"].try_into().unwrap()).await.unwrap();
    let stop = async move {
        ActionSet::register(component.clone(), StopAction::new(false)).await.unwrap();
    };
    let recv_reboot = async move {
        let reason = match receiver.next().await.unwrap() {
            fstatecontrol::AdminRequest::Reboot { reason, .. } => reason,
            _ => panic!("unexpected request"),
        };
        assert_eq!(reason, fstatecontrol::RebootReason::CriticalComponentFailure);
    };
    join!(stop, recv_reboot);
    assert!(test.model.top_instance().has_reboot_task().await);
}

#[fuchsia::test]
async fn on_terminate_exit_triggers_reboot() {
    // Create a topology with a reboot component and a fake reboot protocol
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .add_child(
                    ChildDeclBuilder::new_lazy_child("system")
                        .on_terminate(fdecl::OnTerminate::Reboot)
                        .build(),
                )
                .protocol(
                    ProtocolDeclBuilder::new(REBOOT_PROTOCOL).path(&reboot_protocol_path).build(),
                )
                .expose(cm_rust::ExposeDecl::Protocol(cm_rust::ExposeProtocolDecl {
                    source: cm_rust::ExposeSource::Self_,
                    source_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target: cm_rust::ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
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

    // Start the critical component and cause it to 'exit' by making the runner close its end
    // of the controller channel. This should cause the Admin protocol to receive a reboot request.
    test.model
        .start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug)
        .await
        .unwrap();
    let component = test.model.look_up(&vec!["system"].try_into().unwrap()).await.unwrap();
    let info = ComponentInfo::new(component.clone()).await;
    test.mock_runner.abort_controller(&info.channel_id);
    let reason = match receiver.next().await.unwrap() {
        fstatecontrol::AdminRequest::Reboot { reason, .. } => reason,
        _ => panic!("unexpected request"),
    };
    assert_eq!(reason, fstatecontrol::RebootReason::CriticalComponentFailure);
    assert!(test.model.top_instance().has_reboot_task().await);
}

#[fuchsia::test]
async fn reboot_shutdown_does_not_trigger_reboot() {
    // Create a topology with a reboot-on-terminate component and a fake reboot protocol
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .add_child(
                    ChildDeclBuilder::new_lazy_child("system")
                        .on_terminate(fdecl::OnTerminate::Reboot)
                        .build(),
                )
                .protocol(
                    ProtocolDeclBuilder::new(REBOOT_PROTOCOL).path(&reboot_protocol_path).build(),
                )
                .expose(cm_rust::ExposeDecl::Protocol(cm_rust::ExposeProtocolDecl {
                    source: cm_rust::ExposeSource::Self_,
                    source_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target: cm_rust::ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
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

    // Start the critical component and make it stop. This should cause the Admin protocol to
    // receive a reboot request.
    test.model
        .start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug)
        .await
        .unwrap();
    let component = test.model.look_up(&vec!["system"].try_into().unwrap()).await.unwrap();
    ActionSet::register(component.clone(), ShutdownAction::new()).await.unwrap();
    assert!(!test.model.top_instance().has_reboot_task().await);
}

#[fuchsia::test]
#[should_panic(expected = "Component with on_terminate=REBOOT terminated, but triggering \
                          reboot failed. Crashing component_manager instead: \
                          StateControl Admin protocol encountered FIDL error: A FIDL client's \
                          channel to the protocol fuchsia.hardware.power.statecontrol.Admin was \
                          closed: NOT_FOUND")]
async fn on_terminate_with_missing_reboot_protocol_panics() {
    // Create a topology with a reboot-on-terminate component but no reboot protocol routed to root.
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .add_child(
                    ChildDeclBuilder::new_lazy_child("system")
                        .on_terminate(fdecl::OnTerminate::Reboot)
                        .build(),
                )
                .protocol(
                    ProtocolDeclBuilder::new(REBOOT_PROTOCOL).path(&reboot_protocol_path).build(),
                )
                .expose(cm_rust::ExposeDecl::Protocol(cm_rust::ExposeProtocolDecl {
                    source: cm_rust::ExposeSource::Self_,
                    source_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target: cm_rust::ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        ("system", ComponentDeclBuilder::new().build()),
    ];
    let test = RoutingTestBuilder::new("root", components)
        .set_reboot_on_terminate_policy(vec![AllowlistEntryBuilder::new().exact("system").build()])
        .build()
        .await;

    // Start the critical component and cause it to 'exit' by making the runner close its end of
    // the controller channel. component_manager should attempt to send a reboot request, which
    // should fail because the reboot protocol isn't exposed to it -- expect component_manager to
    // respond by crashing.
    test.model
        .start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug)
        .await
        .unwrap();
    let component = test.model.look_up(&vec!["system"].try_into().unwrap()).await.unwrap();
    let info = ComponentInfo::new(component.clone()).await;
    test.mock_runner.abort_controller(&info.channel_id);
    let () = pending().await;
}

#[fuchsia::test]
#[should_panic(expected = "Component with on_terminate=REBOOT terminated, but triggering \
                          reboot failed. Crashing component_manager instead: \
                          StateControl Admin responded with status: INTERNAL")]
async fn on_terminate_with_failed_reboot_panics() {
    // Create a topology with a reboot-on-terminate component and a fake reboot protocol
    const REBOOT_PROTOCOL: &str = fstatecontrol::AdminMarker::DEBUG_NAME;
    let reboot_protocol_path = format!("/svc/{}", REBOOT_PROTOCOL);
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .add_child(
                    ChildDeclBuilder::new_lazy_child("system")
                        .on_terminate(fdecl::OnTerminate::Reboot)
                        .build(),
                )
                .protocol(
                    ProtocolDeclBuilder::new(REBOOT_PROTOCOL).path(&reboot_protocol_path).build(),
                )
                .expose(cm_rust::ExposeDecl::Protocol(cm_rust::ExposeProtocolDecl {
                    source: cm_rust::ExposeSource::Self_,
                    source_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target_name: REBOOT_PROTOCOL.parse().unwrap(),
                    target: cm_rust::ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
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

    // Start the critical component and cause it to 'exit' by making the runner close its end
    // of the controller channel. Admin protocol should receive a reboot request -- make it fail
    // and expect component_manager to respond by crashing.
    test.model
        .start_instance(&vec!["system"].try_into().unwrap(), &StartReason::Debug)
        .await
        .unwrap();
    let component = test.model.look_up(&vec!["system"].try_into().unwrap()).await.unwrap();
    let info = ComponentInfo::new(component.clone()).await;
    test.mock_runner.abort_controller(&info.channel_id);
    match receiver.next().await.unwrap() {
        fstatecontrol::AdminRequest::Reboot { responder, .. } => {
            responder.send(Err(zx::sys::ZX_ERR_INTERNAL)).unwrap();
        }
        _ => panic!("unexpected request"),
    };
    let () = pending().await;
}
