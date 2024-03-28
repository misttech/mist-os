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
    crate::model::{component::ComponentInstance, error::ActionError},
    async_trait::async_trait,
    cm_util::AbortHandle,
    fuchsia_async as fasync,
    futures::{
        channel::oneshot,
        future::{join_all, pending, BoxFuture, FutureExt, Shared},
        task::{Context, Poll},
        Future,
    },
    std::collections::{HashMap, HashSet},
    std::fmt::Debug,
    std::hash::Hash,
    std::pin::Pin,
    std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

/// A action on a component that must eventually be fulfilled.
#[async_trait]
pub trait Action: Send + Sync + 'static {
    /// Run the action.
    async fn handle(self, component: &Arc<ComponentInstance>) -> Result<(), ActionError>;

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

/// A set of actions on a component that must be completed.
///
/// Each action is mapped to a future that returns when the action is complete.
pub struct ActionSet {
    rep: HashMap<ActionKey, ActionNotifier>,
    history: HashSet<ActionKey>,
    passive_waiters: HashMap<ActionKey, Vec<oneshot::Sender<()>>>,
}

/// A future bound to a particular action that completes when that action completes.
///
/// Cloning this type will not duplicate the action, but generate another future that waits on the
/// same action.
#[derive(Debug)]
pub struct ActionNotifier {
    /// The inner future.
    fut: Shared<BoxFuture<'static, Result<(), ActionError>>>,
    /// How many clones of this ActionNotifier are live, useful for testing.
    refcount: Arc<AtomicUsize>,
    /// If supported, a handle to abort the action.
    abort_handle: Option<AbortHandle>,
}

impl ActionNotifier {
    /// Instantiate an `ActionNotifier` wrapping `fut`.
    pub fn new(
        fut: BoxFuture<'static, Result<(), ActionError>>,
        abort_handle: Option<AbortHandle>,
    ) -> Self {
        Self { fut: fut.shared(), refcount: Arc::new(AtomicUsize::new(1)), abort_handle }
    }
}

impl Clone for ActionNotifier {
    fn clone(&self) -> Self {
        self.refcount.fetch_add(1, Ordering::Relaxed);
        Self {
            fut: self.fut.clone(),
            refcount: self.refcount.clone(),
            abort_handle: self.abort_handle.clone(),
        }
    }
}

impl Drop for ActionNotifier {
    fn drop(&mut self) {
        self.refcount.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Future for ActionNotifier {
    type Output = Result<(), ActionError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fut = Pin::new(&mut self.fut);
        fut.poll(cx)
    }
}

/// Represents a task that implements an action.
pub(crate) struct ActionTask {
    tx: oneshot::Sender<Result<(), ActionError>>,
    fut: BoxFuture<'static, Result<(), ActionError>>,
}

impl ActionTask {
    fn new(
        tx: oneshot::Sender<Result<(), ActionError>>,
        fut: BoxFuture<'static, Result<(), ActionError>>,
    ) -> Self {
        Self { tx, fut }
    }

    /// Runs the action in a separate task and signals the `ActionNotifier` when it completes.
    pub fn spawn(self) {
        fasync::Task::spawn(async move {
            self.tx.send(self.fut.await).unwrap_or(()); // Ignore closed receiver.
        })
        .detach();
    }
}

impl ActionSet {
    pub fn new() -> Self {
        ActionSet { rep: HashMap::new(), history: HashSet::new(), passive_waiters: HashMap::new() }
    }

    pub fn contains(&self, key: &ActionKey) -> bool {
        self.rep.contains_key(key)
    }

    #[cfg(test)]
    pub fn mock_result(&mut self, key: ActionKey, result: Result<(), ActionError>) {
        let notifier = ActionNotifier::new(async move { result }.boxed(), None);
        self.rep.insert(key, notifier);
    }

    #[cfg(test)]
    pub fn remove_notifier(&mut self, key: ActionKey) {
        self.rep.remove(&key).expect("No notifier found with that key");
    }

    /// Returns a oneshot receiver that will receive a message once the component has finished
    /// performing an action with the given key. The oneshot will receive a message immediately if
    /// the component has ever finished such an action. Does not cause any new actions to be
    /// started.
    pub fn wait_for_action(&mut self, action_key: ActionKey) -> oneshot::Receiver<()> {
        let (sender, receiver) = oneshot::channel();
        if self.history.contains(&action_key) {
            sender.send(()).unwrap();
            receiver
        } else {
            self.passive_waiters.entry(action_key).or_insert(vec![]).push(sender);
            receiver
        }
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
            actions.register_no_wait(&component, action)
        };
        rx.await
    }

    /// Registers an action in the set, but does not wait for it to complete, instead returning a
    /// future that can be used to wait on the task. This function is a no-op if the task is
    /// already registered.
    ///
    /// REQUIRES: `self` is the `ActionSet` contained in `component`.
    pub fn register_no_wait<A>(
        &mut self,
        component: &Arc<ComponentInstance>,
        action: A,
    ) -> impl Future<Output = Result<(), ActionError>>
    where
        A: Action,
    {
        let (task, rx) = self.register_inner(component, action);
        if let Some(task) = task {
            task.spawn();
        }
        rx
    }

    /// Returns a future that waits for the given action to complete, if one exists.
    pub fn wait<A>(&self, action: A) -> Option<impl Future<Output = Result<(), ActionError>>>
    where
        A: Action,
    {
        let key = action.key();
        self.rep.get(&key).cloned()
    }

    /// Removes an action from the set, completing it.
    async fn finish<'a>(component: &Arc<ComponentInstance>, key: &'a ActionKey) {
        let mut action_set = component.lock_actions().await;
        action_set.rep.remove(key);
        action_set.history.insert(key.clone());
        for sender in action_set.passive_waiters.entry(key.clone()).or_insert(vec![]).drain(..) {
            let _ = sender.send(());
        }
    }

    /// Registers, but does not execute, an action.
    ///
    /// Returns:
    /// - An object that implements the action if it was scheduled for the first time. The caller
    ///   should call spawn() on it.
    /// - A future to listen on the completion of the action.
    #[must_use]
    pub(crate) fn register_inner<'a, A>(
        &'a mut self,
        component: &Arc<ComponentInstance>,
        action: A,
    ) -> (Option<ActionTask>, ActionNotifier)
    where
        A: Action,
    {
        let key = action.key();
        // If this Action is already running, just subscribe to the result
        if let Some(rx) = self.rep.get(&key) {
            return (None, rx.clone());
        }

        // Otherwise we spin up the new Action
        let maybe_abort_handle = action.abort_handle();
        let prereqs = self.get_prereq_action(action.key());
        let abort_handles = self.get_abort_action(action.key());

        let component = component.clone();

        let action_fut = async move {
            for abort in abort_handles {
                abort.abort();
            }
            _ = join_all(prereqs).await;
            let key = action.key();
            let res = action.handle(&component).await;
            Self::finish(&component, &key).await;
            res
        }
        .boxed();

        let (tx, rx) = oneshot::channel();
        let task = ActionTask::new(tx, action_fut);
        let notifier: ActionNotifier = ActionNotifier::new(
            async move {
                match rx.await {
                    Ok(res) => res,
                    Err(_) => {
                        // Normally we won't get here but this can happen if the sender's task
                        // is cancelled because, for example, component manager exited and the
                        // executor was torn down.
                        let () = pending().await;
                        unreachable!();
                    }
                }
            }
            .boxed(),
            maybe_abort_handle,
        );
        self.rep.insert(key.clone(), notifier.clone());
        (Some(task), notifier)
    }

    /// Return futures that waits for any Action that must be waited on before
    /// executing the target Action. If none is required the returned vector is
    /// empty.
    fn get_prereq_action(&self, key: ActionKey) -> Vec<ActionNotifier> {
        // Start, Stop, and Shutdown are all serialized with respect to one another.
        match key {
            ActionKey::Shutdown => vec![
                self.rep.get(&ActionKey::Stop).cloned(),
                self.rep.get(&ActionKey::Start).cloned(),
            ],
            ActionKey::Stop => vec![
                self.rep.get(&ActionKey::Shutdown).cloned(),
                self.rep.get(&ActionKey::Start).cloned(),
            ],
            ActionKey::Start => vec![
                self.rep.get(&ActionKey::Stop).cloned(),
                self.rep.get(&ActionKey::Shutdown).cloned(),
            ],
            _ => vec![],
        }
        .into_iter()
        .flatten()
        .collect()
    }

    /// Return abort handles for any Action that may be canceled by the target Action.
    ///
    /// This is useful for stopping unnecessary work e.g. if Stop is requested while
    /// Start is running.
    fn get_abort_action(&self, key: ActionKey) -> Vec<AbortHandle> {
        // Stop and Shutdown will attempt to cancel an in-progress Start.
        match key {
            ActionKey::Shutdown | ActionKey::Stop => {
                vec![self.rep.get(&ActionKey::Start).map(|notifier| notifier.abort_handle.clone())]
            }
            _ => vec![],
        }
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }
}

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::model::{error::StopActionError, testing::test_helpers::ActionsTest},
        assert_matches::assert_matches,
        fuchsia_async as fasync,
    };

    async fn register_action_in_new_task<A>(
        action: A,
        component: Arc<ComponentInstance>,
        responder: oneshot::Sender<Result<(), ActionError>>,
        res: Result<(), ActionError>,
    ) where
        A: Action,
    {
        let (starter_tx, starter_rx) = oneshot::channel();
        fasync::Task::spawn(async move {
            let mut action_set = component.lock_actions().await;

            // Register action, and get the future. Use `register_inner` so that we can control
            // when to notify the listener.
            let (task, rx) = action_set.register_inner(&component, action);

            // Signal to test that action is registered.
            starter_tx.send(()).unwrap();

            // Drop `action_set` to release the lock.
            drop(action_set);

            if let Some(task) = task {
                // Notify the listeners, but don't actually run the action since this test tests
                // action registration and not the actions themselves.
                task.tx.send(res).unwrap();
            }
            let res = rx.await;

            // If the future completed successfully then we will get to this point.
            responder.send(res).expect("failed to send response");
        })
        .detach();
        starter_rx.await.expect("Unable to receive start signal");
    }

    #[fuchsia::test]
    async fn action_set() {
        let test = ActionsTest::new("root", vec![], None).await;
        let component = test.model.root().clone();

        let (tx1, rx1) = oneshot::channel();
        register_action_in_new_task(DestroyAction::new(), component.clone(), tx1, Ok(())).await;
        let (tx2, rx2) = oneshot::channel();
        register_action_in_new_task(
            ShutdownAction::new(ShutdownType::Instance),
            component.clone(),
            tx2,
            Err(ActionError::StopError { err: StopActionError::GetParentFailed }), // Some random error.
        )
        .await;
        let (tx3, rx3) = oneshot::channel();
        register_action_in_new_task(DestroyAction::new(), component.clone(), tx3, Ok(())).await;

        // Complete actions, while checking notifications.
        ActionSet::finish(&component, &ActionKey::Destroy).await;
        assert_matches!(rx1.await.expect("Unable to receive result of Notification"), Ok(()));
        assert_matches!(rx3.await.expect("Unable to receive result of Notification"), Ok(()));

        ActionSet::finish(&component, &ActionKey::Shutdown).await;
        assert_matches!(
            rx2.await.expect("Unable to receive result of Notification"),
            Err(ActionError::StopError { err: StopActionError::GetParentFailed })
        );
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use {
        crate::model::component::{ComponentInstance, InstanceState},
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
        let parent_resolved_state = match *parent_state {
            InstanceState::Resolved(ref s) => s,
            _ => panic!("not resolved"),
        };

        let child_state = child.lock_state().await;
        let child_execution = child.lock_execution();
        let found_child = parent_resolved_state.get_child(child.child_moniker().unwrap());

        found_child.is_none()
            && matches!(*child_state, InstanceState::Destroyed)
            && child_execution.runtime.is_none()
            && child_execution.is_shut_down()
    }

    pub async fn is_stopped(component: &ComponentInstance, moniker: &ChildName) -> bool {
        match *component.lock_state().await {
            InstanceState::Resolved(ref s) => match s.get_child(moniker) {
                Some(child) => !child.is_started(),
                None => false,
            },
            InstanceState::Destroyed => false,
            InstanceState::New | InstanceState::Unresolved(_) => {
                panic!("not resolved")
            }
        }
    }

    pub async fn is_destroyed(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        let execution = component.lock_execution();
        matches!(*state, InstanceState::Destroyed)
            && execution.runtime.is_none()
            && execution.is_shut_down()
    }

    pub async fn is_resolved(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Resolved(_))
    }

    pub async fn is_discovered(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Unresolved(_))
    }

    pub async fn is_unresolved(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        let execution = component.lock_execution();
        execution.runtime.is_none()
            && matches!(*state, InstanceState::New | InstanceState::Unresolved(_))
    }
}
