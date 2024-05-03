// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The "Action" concept represents an asynchronous activity on a component that should eventually
//! complete.
//!
//! Actions decouple the "what" of what needs to happen to a component from the "how". Several
//! client APIs may induce operations on a component's state that complete asynchronously. These
//! operations could depend on each other in various ways.
//!
//! A key property of actions is idempotency. If two equal actions are registered on a component,
//! the work for that action is performed only once. This means that two distinct call sites can
//! register the same action, and be guaranteed the work is not repeated.
//!
//! Here are a couple examples:
//! - A `Shutdown` FIDL call must shut down every component instance in the tree, in
//!   dependency order. For this to happen every component must shut down, but not before its
//!   downstream dependencies have shut down.
//! - A `Realm.DestroyChild` FIDL call returns right after a child component is destroyed.
//!   However, in order to actually delete the child, a sequence of events must happen:
//!     * All instances in the component must be shut down (see above)
//!     * The component instance's persistent storage must be erased, if any.
//!     * The component's parent must remove it as a child.
//!
//! Note the interdependencies here -- destroying a component also requires shutdown, for example.
//!
//! These processes could be implemented through a chain of futures in the vicinity of the API
//! call. However, this doesn't scale well, because it requires distributed state handling and is
//! prone to races. Actions solve this problem by allowing client code to just specify the actions
//! that need to eventually be fulfilled. The actual business logic to perform the actions can be
//! implemented by the component itself in a coordinated manner.
//!
//! `DestroyChild()` is an example of how this can work. For simplicity, suppose it's called on a
//! component with no children of its own. This might cause a chain of events like the following:
//!
//! - Before it returns, the `DestroyChild` FIDL handler registers the `DeleteChild` action on the
//!   parent component for child being destroyed.
//! - This results in a call to `Action::handle` for the component. In response to
//!   `DestroyChild`, `Action::handle()` spawns a future that sets a `Destroy` action on the child.
//!   Note that `Action::handle()` is not async, it always spawns any work that might block
//!   in a future.
//! - `Action::handle()` is called on the child. In response to `Destroy`, it sets a `Shutdown`
//!   action on itself (the component instance must be stopped before it is destroyed).
//! - `Action::handle()` is called on the child again, in response to `Shutdown`. It turns out the
//!   instance is still running, so the `Shutdown` future tells the instance to stop. When this
//!   completes, the `Shutdown` action is finished.
//! - The future that was spawned for `Destroy` is notified that `Shutdown` completes, so it cleans
//!   up the instance's resources and finishes the `Destroy` action.
//! - When the work for `Destroy` completes, the future spawned for `DestroyChild` deletes the
//!   child and marks `DestroyChild` finished, which will notify the client that the action is
//!   complete.

mod destroy;
mod discover;
pub mod resolve;
mod set;
pub mod shutdown;
pub mod start;
mod stop;
mod unresolve;

// Re-export the actions
pub use {
    destroy::DestroyAction, discover::DiscoverAction, resolve::ResolveAction,
    shutdown::ShutdownAction, shutdown::ShutdownType, start::StartAction, stop::StopAction,
    unresolve::UnresolveAction,
};

use {
    crate::model::component::{ComponentInstance, WeakComponentInstance},
    async_trait::async_trait,
    cm_util::AbortHandle,
    errors::ActionError,
    futures::{
        channel::oneshot,
        future::{BoxFuture, FutureExt, Shared},
        task::{Context, Poll},
        Future,
    },
    std::fmt::Debug,
    std::hash::Hash,
    std::pin::Pin,
    std::sync::Arc,
};

/// A action on a component that must eventually be fulfilled.
#[async_trait]
pub trait Action: Send + Sync + 'static {
    /// Run the action.
    async fn handle(self, component: Arc<ComponentInstance>) -> Result<(), ActionError>;

    /// `key` identifies the action.
    fn key(&self) -> ActionKey;

    /// If the action supports cooperative cancellation, return a handle for this purpose.
    ///
    /// The action may monitor the handle and bail early when it is safe to do so.
    fn abort_handle(&self) -> Option<AbortHandle> {
        None
    }
}

/// A key that uniquely identifies an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKey {
    Discover,
    Resolve,
    Unresolve,
    Start,
    Stop,
    Shutdown,
    Destroy,
}

/// A future bound to a particular action that completes when that action completes.
///
/// Cloning this type will not duplicate the action, but generate another future that waits on the
/// same action.
#[derive(Debug, Clone)]
pub struct ActionNotifier {
    fut: Shared<BoxFuture<'static, Result<(), ActionError>>>,
}

impl ActionNotifier {
    /// Instantiate an `ActionNotifier` that will complete when a message is received on
    /// `receiver`.
    pub fn new(receiver: oneshot::Receiver<Result<(), ActionError>>) -> Self {
        Self {
            fut: receiver
                .map(|res| res.expect("ActionNotifier sender was unexpectedly closed"))
                .boxed()
                .shared(),
        }
    }

    /// Returns the number of references that exist to the shared future in this notifier. Returns
    /// None if the future has completed.
    #[cfg(test)]
    pub fn get_reference_count(&self) -> Option<usize> {
        self.fut.strong_count()
    }
}

impl Future for ActionNotifier {
    type Output = Result<(), ActionError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fut = Pin::new(&mut self.fut);
        fut.poll(cx)
    }
}

pub struct ActionsManager {
    action_set: set::ActionSet,
    component: WeakComponentInstance,
}

impl ActionsManager {
    pub fn new() -> Self {
        Self { action_set: set::ActionSet::new(), component: WeakComponentInstance::invalid() }
    }

    /// Sets the component reference against which actions on this component will be run. If an
    /// action is registered on this component before this function is called, it will panic.
    pub fn set_component_reference(&mut self, component: WeakComponentInstance) {
        self.component = component;
    }

    pub async fn contains(&self, key: ActionKey) -> bool {
        self.action_set.contains(key).await
    }

    #[cfg(test)]
    pub fn mock_result(&mut self, key: ActionKey, result: Result<(), ActionError>) {
        self.action_set.mock_result(key, result)
    }

    #[cfg(test)]
    pub fn remove_notifier(&mut self, key: ActionKey) {
        self.action_set.remove_notifier(key)
    }

    /// Returns a oneshot receiver that will receive a message once the component has finished
    /// performing an action with the given key. The oneshot will receive a message immediately if
    /// the component has ever finished such an action. Does not cause any new actions to be
    /// started.
    pub async fn wait_for_action(&mut self, action_key: ActionKey) -> oneshot::Receiver<()> {
        self.action_set.wait_for_action(action_key).await
    }

    /// Registers an action in the set, returning when the action is finished (which may represent
    /// a task that's already running for this action).
    pub async fn register<A>(
        component: Arc<ComponentInstance>,
        action: A,
    ) -> Result<(), ActionError>
    where
        A: Action,
    {
        let rx = {
            let mut actions = component.lock_actions().await;
            actions.register_no_wait(action).await
        };
        rx.await
    }

    /// Registers an action in the set, but does not wait for it to complete, instead returning a
    /// future that can be used to wait on the task. This function is a no-op if the task is
    /// already registered.
    ///
    /// REQUIRES: `self` is the `ActionSet` contained in `component`.
    pub async fn register_no_wait<A>(&mut self, action: A) -> ActionNotifier
    where
        A: Action,
    {
        let component =
            self.component.upgrade().expect("tried to register action on nonexistent component");
        self.action_set.register_no_wait(&component, action).await
    }

    /// Returns a future that waits for the given action to complete, if one exists.
    pub async fn wait<A>(&self, action: A) -> Option<ActionNotifier>
    where
        A: Action,
    {
        self.action_set.wait(action).await
    }

    fn finish<'a>(&mut self, key: &'a ActionKey) {
        self.action_set.finish(key)
    }

    /// Registers, but does not execute, an action.
    ///
    /// Returns:
    /// - An object that implements the action if it was scheduled for the first time. The caller
    ///   should call spawn() on it.
    /// - A future to listen on the completion of the action.
    #[cfg(test)]
    pub(crate) fn register_inner<'a, A>(
        &'a mut self,
        action: A,
    ) -> (Option<set::ActionTask>, ActionNotifier)
    where
        A: Action,
    {
        let component =
            self.component.upgrade().expect("tried to register action on nonexistent component");
        self.action_set.register_inner(&component, action)
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use {
        crate::model::component::instance::InstanceState,
        crate::model::component::ComponentInstance,
        moniker::{ChildName, MonikerBase},
        routing::component_instance::ComponentInstanceInterface,
    };

    /// Verifies that a child component is deleted by checking its InstanceState and verifying that
    /// it does not exist in the InstanceState of its parent. Assumes the parent is not destroyed
    /// yet.
    pub async fn is_child_deleted(parent: &ComponentInstance, child: &ComponentInstance) -> bool {
        let instanced_moniker =
            child.instanced_moniker().leaf().expect("Root component cannot be destroyed");

        // Verify the parent-child relationship
        assert_eq!(
            parent.instanced_moniker().child(instanced_moniker.clone()),
            *child.instanced_moniker()
        );

        let parent_state = parent.lock_state().await;
        let parent_resolved_state = parent_state.get_resolved_state().expect("not resolved");

        let child_state = child.lock_state().await;
        let found_child = parent_resolved_state.get_child(child.child_moniker().unwrap());

        found_child.is_none() && matches!(*child_state, InstanceState::Destroyed)
    }

    pub async fn is_stopped(component: &ComponentInstance, moniker: &ChildName) -> bool {
        if let Some(resolved_state) = component.lock_state().await.get_resolved_state() {
            if let Some(child) = resolved_state.get_child(moniker) {
                return !child.is_started().await;
            }
        }
        false
    }

    pub async fn is_destroyed(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Destroyed)
    }

    pub async fn is_resolved(component: &ComponentInstance) -> bool {
        component.lock_state().await.get_resolved_state().is_some()
    }

    pub async fn is_discovered(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Unresolved(_))
    }

    pub async fn is_shutdown(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Shutdown(_, _))
    }
}
