// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        actions::{Action, ActionKey, ActionSet, ShutdownAction},
        component::{ComponentInstance, InstanceState},
        error::UnresolveActionError,
        hooks::{Event, EventPayload},
    },
    async_trait::async_trait,
    std::sync::Arc,
};

/// Returns a resolved component to the discovered state. The result is that the component can be
/// restarted, updating both the code and the manifest with destroying its resources. Unresolve can
/// only be applied to a resolved, stopped, component. This action supports the `ffx component
/// reload` command.
pub struct UnresolveAction {}

impl UnresolveAction {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Action for UnresolveAction {
    type Output = Result<(), UnresolveActionError>;
    async fn handle(&self, component: &Arc<ComponentInstance>) -> Self::Output {
        do_unresolve(component).await
    }
    fn key(&self) -> ActionKey {
        ActionKey::Unresolve
    }
}

// Implement the UnresolveAction by resetting the state from unresolved to unresolved and emitting
// an Unresolved event. Unresolve the component's resolved children if any.
async fn do_unresolve(component: &Arc<ComponentInstance>) -> Result<(), UnresolveActionError> {
    // Shut down the component, preventing new starts or resolves during the UnresolveAction.
    ActionSet::register(component.clone(), ShutdownAction::new()).await?;

    if component.lock_execution().await.runtime.is_some() {
        return Err(UnresolveActionError::InstanceRunning {
            moniker: component.abs_moniker.clone(),
        });
    }

    let children: Vec<Arc<ComponentInstance>> = {
        match *component.lock_state().await {
            InstanceState::Resolved(ref s) => s.children().map(|(_, c)| c.clone()).collect(),
            InstanceState::Destroyed => {
                return Err(UnresolveActionError::InstanceDestroyed {
                    moniker: component.abs_moniker.clone(),
                })
            }
            InstanceState::Unresolved | InstanceState::New => return Ok(()),
        }
    };

    // Unresolve the children before unresolving the component because removing the resolved
    // state removes the ChildInstanceState that contains the list of children.
    for child in children {
        ActionSet::register(child, UnresolveAction::new()).await?;
    }

    // Move the component back to the Discovered state. We can't use a DiscoverAction for this
    // change because the system allows and does call DiscoverAction on resolved components with
    // the expectation that they will return without changing the instance state to Discovered.
    // The state may have changed during the time taken for the recursions, so recheck here.
    {
        let mut state = component.lock_state().await;
        match &*state {
            InstanceState::Resolved(_) => {
                state.set(InstanceState::Unresolved);
                true
            }
            InstanceState::Destroyed => {
                return Err(UnresolveActionError::InstanceDestroyed {
                    moniker: component.abs_moniker.clone(),
                })
            }
            InstanceState::Unresolved | InstanceState::New => return Ok(()),
        }
    };

    // The component was shut down, so won't start. Re-enable it.
    component.lock_execution().await.reset_shut_down();

    let event = Event::new(&component, EventPayload::Unresolved);
    component.hooks.dispatch(&event).await;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use {
        crate::model::{
            actions::test_utils::{is_destroyed, is_discovered, is_executing, is_resolved},
            actions::{ActionSet, ShutdownAction, UnresolveAction},
            component::{ComponentInstance, StartReason},
            error::UnresolveActionError,
            events::{registry::EventSubscription, stream::EventStream},
            hooks::EventType,
            testing::test_helpers::{component_decl_with_test_runner, ActionsTest},
        },
        assert_matches::assert_matches,
        cm_rust::{Availability, UseEventStreamDecl, UseSource},
        cm_rust_testing::{CollectionDeclBuilder, ComponentDeclBuilder},
        cm_types::Name,
        fidl_fuchsia_component_decl as fdecl, fuchsia_async as fasync,
        moniker::{Moniker, MonikerBase},
        std::sync::Arc,
    };

    /// Check unresolve for _recursive_ case. The system has a root with the child `a` and `a` has
    /// descendants as shown in the diagram below.
    ///  a
    ///   \
    ///    b
    ///     \
    ///      c
    ///
    /// Also tests UnresolveAction on InstanceState::Unresolved.
    #[fuchsia::test]
    async fn unresolve_action_recursive_test() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_lazy_child("c").build()),
            ("c", component_decl_with_test_runner()),
        ];
        // Resolve components without starting them.
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(Moniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        assert!(is_resolved(&component_root).await);
        assert!(is_resolved(&component_a).await);
        assert!(is_resolved(&component_b).await);
        assert!(is_resolved(&component_c).await);

        // Unresolve, recursively.
        ActionSet::register(component_a.clone(), UnresolveAction::new())
            .await
            .expect("unresolve failed");
        assert!(is_resolved(&component_root).await);
        // Unresolved recursively, so children in Discovered state.
        assert!(is_discovered(&component_a).await);
        assert!(is_discovered(&component_b).await);
        assert!(is_discovered(&component_c).await);

        // Unresolve again, which is ok because UnresolveAction is idempotent.
        assert_matches!(
            ActionSet::register(component_a.clone(), UnresolveAction::new()).await,
            Ok(())
        );
        // Still Discovered.
        assert!(is_discovered(&component_a).await);
    }

    /// Check unresolve with recursion on eagerly-loaded peer children. The system has a root with
    /// the child `a` and `a` has descendants as shown in the diagram below.
    ///  a
    ///   \
    ///    b
    ///   / \
    ///  c   d
    #[fuchsia::test]
    async fn unresolve_action_recursive_test2() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_eager_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_eager_child("c").add_eager_child("d").build()),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;

        // Resolve each component.
        test.look_up(Moniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Unresolve, recursively.
        ActionSet::register(component_a.clone(), UnresolveAction::new())
            .await
            .expect("unresolve failed");

        // Unresolved recursively, so children in Discovered state.
        assert!(is_discovered(&component_a).await);
        assert!(is_discovered(&component_b).await);
        assert!(is_discovered(&component_c).await);
        assert!(is_discovered(&component_d).await);
    }

    async fn setup_unresolve_test_event_stream(
        test: &ActionsTest,
        event_types: Vec<EventType>,
    ) -> EventStream {
        let events: Vec<_> = event_types.into_iter().map(|e| e.into()).collect();
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
        let model = test.model.clone();
        fasync::Task::spawn(async move { model.start().await }).detach();
        event_stream
    }

    #[fuchsia::test]
    async fn unresolve_action_registers_unresolve_event_test() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").build()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        test.start(Moniker::root()).await;
        let component_a = test.start(vec!["a"].try_into().unwrap()).await;
        test.start(vec!["a", "b"].try_into().unwrap()).await;

        let mut event_stream =
            setup_unresolve_test_event_stream(&test, vec![EventType::Unresolved]).await;

        // Register the UnresolveAction.
        let nf = {
            let mut actions = component_a.lock_actions().await;
            actions.register_no_wait(&component_a, UnresolveAction::new())
        };

        // Confirm that the Unresolved events are emitted in the expected recursive order.
        event_stream
            .wait_until(EventType::Unresolved, vec!["a", "b"].try_into().unwrap())
            .await
            .unwrap();
        event_stream
            .wait_until(EventType::Unresolved, vec!["a"].try_into().unwrap())
            .await
            .unwrap();
        nf.await.unwrap();

        // Now attempt to unresolve again with another UnresolveAction.
        let nf2 = {
            let mut actions = component_a.lock_actions().await;
            actions.register_no_wait(&component_a, UnresolveAction::new())
        };
        // The component is not resolved anymore, so the unresolve will have no effect. It will not
        // emit an UnresolveFailed event.
        nf2.await.unwrap();
        assert!(is_discovered(&component_a).await);
    }

    /// Start a collection with the given durability. The system has a root with a container that
    /// has a collection containing children `a` and `b` as shown in the diagram below.
    ///    root
    ///      \
    ///    container
    ///     /     \
    ///  coll:a   coll:b
    ///
    async fn start_collection(
        durability: fdecl::Durability,
    ) -> (ActionsTest, Arc<ComponentInstance>, Arc<ComponentInstance>, Arc<ComponentInstance>) {
        let collection = CollectionDeclBuilder::new()
            .name("coll")
            .durability(durability)
            .allow_long_names(true)
            .build();

        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("container").build()),
            ("container", ComponentDeclBuilder::new().add_collection(collection).build()),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
        ];
        let test =
            ActionsTest::new("root", components, Some(vec!["container"].try_into().unwrap())).await;

        // Create dynamic instances in "coll".
        test.create_dynamic_child("coll", "a").await;
        test.create_dynamic_child("coll", "b").await;

        // Start the components. This should cause them to have an `Execution`.
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
        assert!(is_resolved(&component_a).await);
        assert!(is_resolved(&component_b).await);
        assert!(is_executing(&component_a).await);
        assert!(is_executing(&component_b).await);
        (test, component_container, component_a, component_b)
    }

    /// Test a collection with the given durability.
    /// Also tests UnresolveAction on InstanceState::Destroyed.
    async fn test_collection(durability: fdecl::Durability) {
        let (_test, component_container, component_a, component_b) =
            start_collection(durability).await;

        // Stop the collection.
        ActionSet::register(component_container.clone(), ShutdownAction::new())
            .await
            .expect("shutdown failed");
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
        ActionSet::register(component_container.clone(), UnresolveAction::new())
            .await
            .expect("unresolve failed");
        assert!(is_discovered(&component_container).await);

        // Trying to unresolve a child fails because the children of a collection are destroyed when
        // the collection is stopped. Then it's an error to unresolve a Destroyed component.
        assert_matches!(
            ActionSet::register(component_a.clone(), UnresolveAction::new()).await,
            Err(UnresolveActionError::InstanceDestroyed { .. })
        );
        // Still Destroyed.
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
    }

    /// Test a collection whose children have transient durability.
    #[fuchsia::test]
    async fn unresolve_action_on_transient_collection() {
        test_collection(fdecl::Durability::Transient).await;
    }

    /// Test a collection whose children have single-run durability.
    #[fuchsia::test]
    async fn unresolve_action_on_single_run_collection() {
        test_collection(fdecl::Durability::SingleRun).await;
    }
}
