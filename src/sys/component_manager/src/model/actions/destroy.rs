// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        actions::{
            Action, ActionKey, ActionSet, DestroyChildAction, DiscoverAction, ResolveAction,
            ShutdownAction, StartAction,
        },
        component::{ComponentInstance, InstanceState, StartReason},
        error::DestroyActionError,
    },
    async_trait::async_trait,
    futures::{
        future::{join_all, BoxFuture},
        Future,
    },
    std::sync::Arc,
};

/// Destroy this component instance, including all instances nested in its component.
pub struct DestroyAction {}

impl DestroyAction {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Action for DestroyAction {
    type Output = Result<(), DestroyActionError>;
    async fn handle(&self, component: &Arc<ComponentInstance>) -> Self::Output {
        do_destroy(component).await
    }
    fn key(&self) -> ActionKey {
        ActionKey::Destroy
    }
}

async fn do_destroy(component: &Arc<ComponentInstance>) -> Result<(), DestroyActionError> {
    // Do nothing if already destroyed.
    {
        if let InstanceState::Destroyed = *component.lock_state().await {
            return Ok(());
        }
    }

    // Require the component to be discovered before deleting it so a Destroyed event is
    // always preceded by a Discovered.
    ActionSet::register(component.clone(), DiscoverAction::new()).await?;

    // For destruction to behave correctly, the component has to be shut down first.
    // NOTE: This will recursively shut down the whole subtree. If this component has children,
    // we'll call DestroyChild on them which in turn will call Shutdown on the child. Because
    // the parent's subtree was shutdown, this shutdown is a no-op.
    ActionSet::register(component.clone(), ShutdownAction::new())
        .await
        .map_err(|e| DestroyActionError::ShutdownFailed { err: Box::new(e) })?;

    let nfs = {
        match *component.lock_state().await {
            InstanceState::Resolved(ref s) => {
                let mut nfs = vec![];
                for (m, c) in s.children() {
                    let component = component.clone();
                    let m = m.clone();
                    let incarnation = c.incarnation_id();
                    let nf = async move {
                        ActionSet::register(component, DestroyChildAction::new(m, incarnation))
                            .await
                    };
                    nfs.push(nf);
                }
                nfs
            }
            InstanceState::New | InstanceState::Unresolved | InstanceState::Destroyed => {
                // Component was never resolved. No explicit cleanup is required for children.
                vec![]
            }
        }
    };
    let results = join_all(nfs).await;
    ok_or_first_error(results)?;

    // Now that all children have been destroyed, destroy the parent.
    component.destroy_instance().await?;

    // Only consider the component fully destroyed once it's no longer executing any lifecycle
    // transitions.
    {
        let mut state = component.lock_state().await;
        state.set(InstanceState::Destroyed);
    }
    fn wait(nf: Option<impl Future + Send + 'static>) -> BoxFuture<'static, ()> {
        Box::pin(async {
            if let Some(nf) = nf {
                nf.await;
            }
        })
    }

    // Wait for any remaining blocking tasks and actions finish up.
    let task_shutdown = Box::pin(component.blocking_task_scope().shutdown());
    let nfs = {
        let actions = component.lock_actions().await;
        vec![
            wait(actions.wait(ResolveAction::new())),
            wait(actions.wait(StartAction::new(StartReason::Debug))),
            task_shutdown,
        ]
    };
    join_all(nfs.into_iter()).await;

    Ok(())
}

fn ok_or_first_error(
    results: Vec<Result<(), DestroyActionError>>,
) -> Result<(), DestroyActionError> {
    results.into_iter().fold(Ok(()), |acc, r| acc.and_then(|_| r))
}

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::model::{
            actions::{
                test_utils::{is_child_deleted, is_destroyed, is_executing},
                ActionNotifier, ShutdownAction,
            },
            component::{Component, StartReason},
            error::{DiscoverActionError, ResolveActionError, StartActionError},
            events::{registry::EventSubscription, stream::EventStream},
            hooks::EventType,
            testing::{
                test_helpers::{
                    component_decl_with_test_runner, execution_is_shut_down, get_incarnation_id,
                    has_child, ActionsTest,
                },
                test_hook::Lifecycle,
            },
        },
        assert_matches::assert_matches,
        cm_rust::{Availability, CapabilityPath, ComponentDecl, UseEventStreamDecl, UseSource},
        cm_rust_testing::ComponentDeclBuilder,
        fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
        fuchsia_zircon as zx,
        futures::{channel::mpsc, lock::Mutex, StreamExt},
        moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ChildMoniker},
        std::fmt::Debug,
        std::str::FromStr,
        std::sync::atomic::Ordering,
    };

    #[fuchsia::test]
    async fn destroy_one_component() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        // Start the component. This should cause the component to have an `Execution`.
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        test.model
            .start_instance(&component_a.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(is_executing(&component_a).await);

        // Register shutdown first because DestroyChild requires the component to be shut down.
        ActionSet::register(component_a.clone(), ShutdownAction::new())
            .await
            .expect("shutdown failed");
        // Register destroy child action, and wait for it. Component should be destroyed.
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["a"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ],
            );
        }

        // Trying to start the component should fail because it's shut down.
        test.model
            .start_instance(&component_a.abs_moniker, &StartReason::Eager)
            .await
            .expect_err("successfully bound to a after shutdown");

        // Destroy the component again. This succeeds, but has no additional effect.
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
    }

    #[fuchsia::test]
    async fn destroy_collection() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("container").build()),
            ("container", ComponentDeclBuilder::new().add_transient_collection("coll").build()),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
        ];
        let test =
            ActionsTest::new("root", components, Some(vec!["container"].try_into().unwrap())).await;

        // Create dynamic instances in "coll".
        test.create_dynamic_child("coll", "a").await;
        test.create_dynamic_child("coll", "b").await;

        // Start the components. This should cause them to have an `Execution`.
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_container = test.look_up(vec!["container"].try_into().unwrap()).await;
        let component_a = test.look_up(vec!["container", "coll:a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["container", "coll:b"].try_into().unwrap()).await;
        test.model
            .start_instance(&component_container.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start container");
        test.model
            .start_instance(&component_a.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:a");
        test.model
            .start_instance(&component_b.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:b");
        assert!(is_executing(&component_container).await);
        assert!(is_executing(&component_a).await);
        assert!(is_executing(&component_b).await);

        // Register destroy child action, and wait for it. Components should be destroyed.
        let component_container = test.look_up(vec!["container"].try_into().unwrap()).await;
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("container".try_into().unwrap(), 0),
        )
        .await
        .expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_container).await);
        assert!(is_destroyed(&component_container).await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
    }

    #[fuchsia::test]
    async fn destroy_already_shut_down() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up order.
        ActionSet::register(component_a.clone(), ShutdownAction::new())
            .await
            .expect("shutdown failed");
        assert!(execution_is_shut_down(&component_a.clone()).await);
        assert!(execution_is_shut_down(&component_b.clone()).await);

        // Now delete child "a". This should cause all components to be destroyed.
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_a).await);

        // Check order of events.
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap()),
                ]
            );
        }
    }

    // Blocks an action so that we can verify the number of notifiers for the action
    // and eventually unblock it.
    pub struct MockAction<O: Send + Sync + Clone + Debug + 'static> {
        rx: Mutex<mpsc::Receiver<()>>,
        key: ActionKey,
        result: O,
    }

    impl<O: Send + Sync + Clone + Debug + 'static> MockAction<O> {
        pub fn new(key: ActionKey, result: O) -> (Self, mpsc::Sender<()>) {
            let (tx, rx) = mpsc::channel::<()>(0);
            let blocker = Self { rx: Mutex::new(rx), key, result };
            (blocker, tx)
        }
    }

    #[async_trait]
    impl<O: Send + Sync + Clone + Debug + 'static> Action for MockAction<O> {
        type Output = O;

        async fn handle(&self, _: &Arc<ComponentInstance>) -> O {
            let mut rx = self.rx.lock().await;
            rx.next().await.unwrap();
            self.result.clone()
        }

        fn key(&self) -> ActionKey {
            self.key.clone()
        }
    }

    async fn setup_destroy_waits_test_event_stream(
        test: &ActionsTest,
        event_types: Vec<EventType>,
    ) -> EventStream {
        let events: Vec<_> = event_types
            .into_iter()
            .map(|e| UseEventStreamDecl {
                source_name: e.into(),
                source: UseSource::Parent,
                scope: None,
                target_path: CapabilityPath::from_str("/svc/fuchsia.component.EventStream")
                    .unwrap(),
                filter: None,
                availability: Availability::Required,
            })
            .collect();
        let mut event_source = test
            .builtin_environment
            .lock()
            .await
            .event_source_factory
            .create_for_above_root()
            .await
            .unwrap();
        let event_stream = event_source
            .subscribe(
                events.into_iter().map(|event| EventSubscription { event_name: event }).collect(),
            )
            .await
            .expect("subscribe to event stream");
        let model = test.model.clone();
        fasync::Task::spawn(async move { model.start().await }).detach();
        event_stream
    }

    async fn run_destroy_waits_test<O>(mock_action_key: ActionKey, mock_action_result: O)
    where
        O: Send + Sync + Clone + Debug + 'static,
    {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        test.model.start().await;

        let component_root = test.model.root().clone();
        let component_a = match *component_root.lock_state().await {
            InstanceState::Resolved(ref s) => s
                .get_child(&ChildMoniker::try_from("a").unwrap())
                .expect("child a not found")
                .clone(),
            _ => panic!("not resolved"),
        };

        let (mock_action, mut mock_action_unblocker) =
            MockAction::new(mock_action_key.clone(), mock_action_result);

        // Spawn a mock action on 'a' that stalls
        {
            let mut actions = component_a.lock_actions().await;
            let _ = actions.register_no_wait(&component_a, mock_action);
        }

        // Spawn a destroy child action on root for `a`.
        // This eventually leads to a destroy action on `a`.
        let destroy_child_fut = {
            let mut actions = component_root.lock_actions().await;
            actions.register_no_wait(
                &component_root,
                DestroyChildAction::new("a".try_into().unwrap(), 0),
            )
        };

        // Check that the destroy action is waiting on the mock action.
        loop {
            let actions = component_a.lock_actions().await;
            assert!(actions.contains(&mock_action_key));

            // Check the reference count on the notifier of the mock action
            let rx = &actions.rep[&mock_action_key];
            let rx = rx
                .downcast_ref::<ActionNotifier<O>>()
                .expect("action notifier has unexpected type");
            let refcount = rx.refcount.load(Ordering::Relaxed);

            // expected reference count:
            // - 1 for the ActionSet that owns the notifier
            // - 1 for destroy action to wait on the mock action
            if refcount == 2 {
                assert!(actions.contains(&ActionKey::Destroy));
                break;
            }

            // The destroy action hasn't blocked on the mock action yet.
            // Wait for that to happen and check again.
            drop(actions);
            fasync::Timer::new(fasync::Time::after(zx::Duration::from_millis(100))).await;
        }

        // Unblock the mock action, causing destroy to complete as well
        mock_action_unblocker.try_send(()).unwrap();
        destroy_child_fut.await.unwrap();
        assert!(is_child_deleted(&component_root, &component_a).await);
    }

    #[fuchsia::test]
    async fn destroy_waits_on_discover() {
        run_destroy_waits_test(
            ActionKey::Discover,
            // The mocked action must return a result, even though the result is not used
            // by the Destroy action.
            Ok(()) as Result<(), DiscoverActionError>,
        )
        .await;
    }

    #[fuchsia::test]
    async fn destroy_waits_on_resolve() {
        run_destroy_waits_test(
            ActionKey::Resolve,
            // The mocked action must return a result, even though the result is not used
            // by the Destroy action.
            Ok(Component {
                resolved_url: "unused".to_string(),
                context_to_resolve_children: None,
                decl: ComponentDecl::default(),
                package: None,
                config: None,
                abi_revision: None,
            }) as Result<Component, ResolveActionError>,
        )
        .await;
    }

    #[fuchsia::test]
    async fn destroy_waits_on_start() {
        run_destroy_waits_test(
            ActionKey::Start,
            // The mocked action must return a result, even though the result is not used
            // by the Destroy action.
            Ok(fsys::StartResult::Started) as Result<fsys::StartResult, StartActionError>,
        )
        .await;
    }

    #[fuchsia::test]
    async fn destroy_registers_discover() {
        let components = vec![("root", ComponentDeclBuilder::new().build())];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        // This setup circumvents the registration of the Discover action on component_a.
        {
            let mut resolved_state = component_root.lock_resolved_state().await.unwrap();
            let child = cm_rust::ChildDecl {
                name: format!("a"),
                url: format!("test:///a"),
                startup: fdecl::StartupMode::Lazy,
                environment: None,
                on_terminate: None,
                config_overrides: None,
            };
            assert!(resolved_state.add_child_no_discover(&component_root, &child, None).is_ok());
        }
        let mut event_stream = setup_destroy_waits_test_event_stream(
            &test,
            vec![EventType::Discovered, EventType::Destroyed],
        )
        .await;

        // Shut down component so we can destroy it.
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_a = match *component_root.lock_state().await {
            InstanceState::Resolved(ref s) => s
                .get_child(&ChildMoniker::try_from("a").unwrap())
                .expect("child a not found")
                .clone(),
            _ => panic!("not resolved"),
        };
        ActionSet::register(component_a.clone(), ShutdownAction::new())
            .await
            .expect("shutdown failed");

        // Confirm component is still in New state.
        {
            let state = &*component_a.lock_state().await;
            assert_matches!(state, InstanceState::New);
        };

        // Register DestroyChild.
        let nf = {
            let mut actions = component_root.lock_actions().await;
            actions.register_no_wait(
                &component_root,
                DestroyChildAction::new("a".try_into().unwrap(), 0),
            )
        };

        // Wait for Discover action, which should be registered by Destroy, followed by
        // Destroyed.
        event_stream
            .wait_until(EventType::Discovered, vec!["a"].try_into().unwrap())
            .await
            .unwrap();
        event_stream.wait_until(EventType::Destroyed, vec!["a"].try_into().unwrap()).await.unwrap();
        nf.await.unwrap();
        assert!(is_child_deleted(&component_root, &component_a).await);
    }

    #[fuchsia::test]
    async fn destroy_not_resolved() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_lazy_child("c").build()),
            ("c", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        test.model
            .start_instance(&component_a.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(is_executing(&component_a).await);
        // Get component_b without resolving it.
        let component_b = match *component_a.lock_state().await {
            InstanceState::Resolved(ref s) => s
                .get_child(&ChildMoniker::try_from("b").unwrap())
                .expect("child b not found")
                .clone(),
            _ => panic!("not resolved"),
        };

        // Register destroy action on "a", and wait for it.
        ActionSet::register(component_a.clone(), ShutdownAction::new())
            .await
            .expect("shutdown failed");
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_b).await);

        // Now "a" is destroyed. Expect destroy events for "a" and "b".
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["a"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    ///  Delete "a" as child of root:
    ///
    ///  /\
    /// x  a*
    ///     \
    ///      b
    ///     / \
    ///    c   d
    #[fuchsia::test]
    async fn destroy_hierarchy() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").add_lazy_child("x").build()),
            ("a", ComponentDeclBuilder::new().add_eager_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_eager_child("c").add_eager_child("d").build()),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
            ("x", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;
        let component_x = test.look_up(vec!["x"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        test.model
            .start_instance(&component_a.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        test.model
            .start_instance(&component_x.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start x");
        assert!(is_executing(&component_a).await);
        assert!(is_executing(&component_b).await);
        assert!(is_executing(&component_c).await);
        assert!(is_executing(&component_d).await);
        assert!(is_executing(&component_x).await);

        // Register destroy action on "a", and wait for it. This should cause all components
        // in "a"'s component to be shut down and destroyed, in bottom-up order, but "x" is still
        // running.
        ActionSet::register(component_a.clone(), ShutdownAction::new())
            .await
            .expect("shutdown failed");
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect("delete child failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_c).await);
        assert!(is_destroyed(&component_d).await);
        assert!(is_executing(&component_x).await);
        {
            // Expect only "x" as child of root.
            let state = component_root.lock_state().await;
            let children: Vec<_> = match *state {
                InstanceState::Resolved(ref s) => s.children().map(|(k, _)| k.clone()).collect(),
                _ => {
                    panic!("not resolved");
                }
            };
            assert_eq!(children, vec!["x".try_into().unwrap()]);
        }
        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();

            // The leaves could be stopped in any order.
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            assert_eq!(
                first,
                vec![
                    Lifecycle::Stop(vec!["a", "b", "c"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a", "b", "d"].try_into().unwrap())
                ]
            );
            let next: Vec<_> = events.drain(0..2).collect();
            assert_eq!(
                next,
                vec![
                    Lifecycle::Stop(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a"].try_into().unwrap())
                ]
            );

            // The leaves could be destroyed in any order.
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            assert_eq!(
                first,
                vec![
                    Lifecycle::Destroy(vec!["a", "b", "c"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b", "d"].try_into().unwrap())
                ]
            );
            assert_eq!(
                events,
                vec![
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    /// Destroy `b`:
    ///  a
    ///   \
    ///    b
    ///     \
    ///      b
    ///       \
    ///      ...
    ///
    /// `b` is a child of itself, but destruction should still be able to complete.
    #[fuchsia::test]
    async fn destroy_self_referential() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_lazy_child("b").build()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_b2 = test.look_up(vec!["a", "b", "b"].try_into().unwrap()).await;

        // Start the second `b`.
        test.model
            .start_instance(&component_a.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        test.model
            .start_instance(&component_b.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        test.model
            .start_instance(&component_b2.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        assert!(is_executing(&component_a).await);
        assert!(is_executing(&component_b).await);
        assert!(is_executing(&component_b2).await);

        // Register destroy action on "a", and wait for it. This should cause all components
        // that were started to be destroyed, in bottom-up order.
        ActionSet::register(component_a.clone(), ShutdownAction::new())
            .await
            .expect("shutdown failed");
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect("delete child failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_b2).await);
        {
            let state = component_root.lock_state().await;
            let children: Vec<_> = match *state {
                InstanceState::Resolved(ref s) => s.children().map(|(k, _)| k.clone()).collect(),
                _ => panic!("not resolved"),
            };
            assert_eq!(children, Vec::<ChildMoniker>::new());
        }
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["a", "b", "b"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a"].try_into().unwrap()),
                    // This component instance is never resolved but we still invoke the Destroy
                    // hook on it.
                    Lifecycle::Destroy(vec!["a", "b", "b", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    /// Destroy `a`:
    ///
    ///    a*
    ///     \
    ///      b
    ///     / \
    ///    c   d
    ///
    /// `a` fails to destroy the first time, but succeeds the second time.
    #[fuchsia::test]
    async fn destroy_error() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_eager_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_eager_child("c").add_eager_child("d").build()),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        test.model
            .start_instance(&component_a.abs_moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(is_executing(&component_a).await);
        assert!(is_executing(&component_b).await);
        assert!(is_executing(&component_c).await);
        assert!(is_executing(&component_d).await);

        // Mock a failure to delete "d".
        {
            let mut actions = component_d.lock_actions().await;
            actions.mock_result(
                ActionKey::Destroy,
                Err(DestroyActionError::InstanceNotFound {
                    moniker: component_d.abs_moniker.clone(),
                }) as Result<(), DestroyActionError>,
            );
        }

        ActionSet::register(
            component_b.clone(),
            DestroyChildAction::new("d".try_into().unwrap(), 0),
        )
        .await
        .expect_err("d's destroy succeeded unexpectedly");

        // Register delete action on "a", and wait for it. but "d"
        // returns an error so the delete action on "a" does not succeed.
        //
        // In this state, "d" is marked destroyed but hasn't been removed from the
        // children list of "b". "c" is destroyed and has been removed from the children
        // list of "b".
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect_err("destroy succeeded unexpectedly");
        assert!(has_child(&component_root, "a").await);
        assert!(has_child(&component_a, "b").await);
        assert!(!has_child(&component_b, "c").await);
        assert!(has_child(&component_b, "d").await);
        assert!(!is_destroyed(&component_a).await);
        assert!(!is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_c).await);
        assert!(!is_destroyed(&component_d).await);
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            let expected: Vec<_> =
                vec![Lifecycle::Destroy(vec!["a", "b", "c"].try_into().unwrap())];
            assert_eq!(events, expected);
        }

        // Remove the mock from "d"
        {
            let mut actions = component_d.lock_actions().await;
            actions.remove_notifier(ActionKey::Destroy);
        }

        // Register destroy action on "a" again. "d"'s delete succeeds, and "a" is deleted
        // this time.
        ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("a".try_into().unwrap(), 0),
        )
        .await
        .expect("destroy failed");
        assert!(!has_child(&component_root, "a").await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_c).await);
        assert!(is_destroyed(&component_d).await);
        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            // The leaves could be stopped in any order.
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            let expected: Vec<_> = vec![
                Lifecycle::Destroy(vec!["a", "b", "c"].try_into().unwrap()),
                Lifecycle::Destroy(vec!["a", "b", "d"].try_into().unwrap()),
            ];
            assert_eq!(first, expected);
            assert_eq!(
                events,
                vec![
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    #[fuchsia::test]
    async fn destroy_runs_after_new_instance_created() {
        // We want to demonstrate that running two destroy child actions for the same child
        // instance, which should be idempotent, works correctly if a new instance of the child
        // under the same name is created between them.
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_transient_collection("coll").build()),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, Some(AbsoluteMoniker::root())).await;

        // Create dynamic instance in "coll".
        test.create_dynamic_child("coll", "a").await;

        // Start the component so we can witness it getting stopped.
        test.start(vec!["coll:a"].try_into().unwrap()).await;

        // We're going to run the destroy action for `a` twice. One after the other finishes, so
        // the actions semantics don't dedup them to the same work item.
        let component_root = test.look_up(AbsoluteMoniker::root()).await;
        let destroy_fut_1 = ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("coll:a".try_into().unwrap(), 1),
        );
        let destroy_fut_2 = ActionSet::register(
            component_root.clone(),
            DestroyChildAction::new("coll:a".try_into().unwrap(), 1),
        );

        let component_a = test.look_up(vec!["coll:a"].try_into().unwrap()).await;
        assert!(!is_child_deleted(&component_root, &component_a).await);

        destroy_fut_1.await.expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);

        // Now recreate `a`
        test.create_dynamic_child("coll", "a").await;
        test.start(vec!["coll:a"].try_into().unwrap()).await;

        // Run the second destroy fut, it should leave the newly created `a` alone
        destroy_fut_2.await.expect("destroy failed");
        let component_a = test.look_up(vec!["coll:a"].try_into().unwrap()).await;
        assert_eq!(get_incarnation_id(&component_root, "coll:a").await, 2);
        assert!(!is_child_deleted(&component_root, &component_a).await);

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["coll:a"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["coll:a"].try_into().unwrap()),
                ],
            );
        }
    }
}
