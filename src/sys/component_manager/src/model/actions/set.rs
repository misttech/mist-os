// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{Action, ActionKey, ActionNotifier},
    crate::model::component::ComponentInstance,
    cm_util::AbortHandle,
    errors::ActionError,
    fuchsia_async as fasync,
    futures::{
        channel::oneshot,
        future::{join_all, BoxFuture, FutureExt},
    },
    std::collections::{HashMap, HashSet},
    std::sync::Arc,
};

/// Represents a task that implements an action.
pub struct ActionTask {
    pub tx: oneshot::Sender<Result<(), ActionError>>,
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

struct ActionController {
    /// The notifier by which clients will be informed when the action returns.
    notifier: ActionNotifier,
    /// If supported, a handle to abort the action.
    maybe_abort_handle: Option<AbortHandle>,
}

/// A set of actions on a component that must be completed.
///
/// Each action is mapped to a future that returns when the action is complete.
pub struct ActionSet {
    rep: HashMap<ActionKey, ActionController>,
    history: HashSet<ActionKey>,
    passive_waiters: HashMap<ActionKey, Vec<oneshot::Sender<()>>>,
}

impl ActionSet {
    pub fn new() -> Self {
        ActionSet { rep: HashMap::new(), history: HashSet::new(), passive_waiters: HashMap::new() }
    }

    pub async fn contains(&self, key: ActionKey) -> bool {
        self.rep.contains_key(&key)
    }

    #[cfg(test)]
    pub fn mock_result(&mut self, key: ActionKey, result: Result<(), ActionError>) {
        let (sender, receiver) = oneshot::channel();
        sender.send(result).unwrap();
        let notifier = ActionNotifier::new(receiver);
        self.rep.insert(key, ActionController { notifier, maybe_abort_handle: None });
    }

    #[cfg(test)]
    pub fn remove_notifier(&mut self, key: ActionKey) {
        self.rep.remove(&key).expect("No notifier found with that key");
    }

    /// Returns a oneshot receiver that will receive a message once the component has finished
    /// performing an action with the given key. The oneshot will receive a message immediately if
    /// the component has ever finished such an action. Does not cause any new actions to be
    /// started.
    pub async fn wait_for_action(&mut self, action_key: ActionKey) -> oneshot::Receiver<()> {
        let (sender, receiver) = oneshot::channel();
        if self.history.contains(&action_key) {
            sender.send(()).unwrap();
            receiver
        } else {
            self.passive_waiters.entry(action_key).or_insert(vec![]).push(sender);
            receiver
        }
    }

    /// Registers an action in the set, but does not wait for it to complete, instead returning a
    /// future that can be used to wait on the task. This function is a no-op if the task is
    /// already registered.
    ///
    /// REQUIRES: `self` is the `ActionSet` contained in `component`.
    pub async fn register_no_wait<A>(
        &mut self,
        component: &Arc<ComponentInstance>,
        action: A,
    ) -> ActionNotifier
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
    pub fn wait(&self, action_key: ActionKey) -> Option<ActionNotifier> {
        self.rep.get(&action_key).map(|controller| controller.notifier.clone())
    }

    /// Removes an action from the set, completing it.
    pub(super) fn finish<'a>(&mut self, key: &'a ActionKey) {
        self.rep.remove(key);
        self.history.insert(key.clone());
        for sender in self.passive_waiters.entry(key.clone()).or_insert(vec![]).drain(..) {
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
        if let Some(action_controller) = self.rep.get(&key) {
            return (None, action_controller.notifier.clone());
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
            let res = action.handle(component.clone()).await;
            component.lock_actions().await.finish(&key);
            res
        }
        .boxed();

        let (notifier_completer, notifier_receiver) = oneshot::channel();
        let task = ActionTask::new(notifier_completer, action_fut);
        let notifier = ActionNotifier::new(notifier_receiver);
        self.rep.insert(
            key.clone(),
            ActionController { notifier: notifier.clone(), maybe_abort_handle },
        );
        (Some(task), notifier)
    }

    /// Return futures that waits for any Action that must be waited on before
    /// executing the target Action. If none is required the returned vector is
    /// empty.
    fn get_prereq_action(&self, key: ActionKey) -> Vec<ActionNotifier> {
        // Start, Stop, and Shutdown are all serialized with respect to one another.
        match key {
            ActionKey::Shutdown => vec![
                self.rep.get(&ActionKey::Stop).map(|c| c.notifier.clone()),
                self.rep.get(&ActionKey::Start).map(|c| c.notifier.clone()),
            ],
            ActionKey::Stop => vec![
                self.rep.get(&ActionKey::Shutdown).map(|c| c.notifier.clone()),
                self.rep.get(&ActionKey::Start).map(|c| c.notifier.clone()),
            ],
            ActionKey::Start => vec![
                self.rep.get(&ActionKey::Stop).map(|c| c.notifier.clone()),
                self.rep.get(&ActionKey::Shutdown).map(|c| c.notifier.clone()),
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
                vec![self
                    .rep
                    .get(&ActionKey::Start)
                    .and_then(|action_controller| action_controller.maybe_abort_handle.clone())]
            }
            _ => vec![],
        }
        .into_iter()
        .flatten()
        .collect()
    }
}

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::model::actions::{DestroyAction, ShutdownAction, ShutdownType},
        crate::model::testing::test_helpers::ActionsTest,
        assert_matches::assert_matches,
        errors::StopActionError,
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
            let (task, rx) = action_set.register_inner(action);

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
        component.lock_actions().await.finish(&ActionKey::Destroy);
        assert_matches!(rx1.await.expect("Unable to receive result of Notification"), Ok(()));
        assert_matches!(rx3.await.expect("Unable to receive result of Notification"), Ok(()));

        component.lock_actions().await.finish(&ActionKey::Shutdown);
        assert_matches!(
            rx2.await.expect("Unable to receive result of Notification"),
            Err(ActionError::StopError { err: StopActionError::GetParentFailed })
        );
    }
}
