// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;
use futures::lock::Mutex;
use std::sync::Arc;

/// Closure definition for an action that can be triggered by ActionFuse.
pub type TriggeredAction = Box<dyn FnOnce() + Send + Sync + 'static>;
/// The reference-counted handle to an ActionFuse. When all references go out of
/// scope, the action will be triggered (if not defused).
pub type ActionFuseHandle = Arc<Mutex<ActionFuse>>;

/// ActionFuseBuilder allows creation of ActionFuses. Note that all parameters
/// are completely optional to the builder. A fuse with no action or chained
/// fuse is valid.
pub(crate) struct ActionFuseBuilder {
    actions: Vec<TriggeredAction>,
}

impl ActionFuseBuilder {
    pub(crate) fn new() -> Self {
        ActionFuseBuilder { actions: vec![] }
    }

    /// Adds an action to be executed once dropped.
    pub(crate) fn add_action(mut self, action: TriggeredAction) -> Self {
        self.actions.push(action);
        self
    }

    /// Generates fuse based on parameters.
    pub(crate) fn build(self) -> ActionFuseHandle {
        ActionFuse::create(self.actions)
    }
}

/// ActionFuse is a wrapper around a triggered action (a closure with no
/// arguments and no return value). This action is invoked once the fuse goes
/// out of scope (via the Drop trait). An ActionFuse can be defused, preventing
/// the action from automatically invoking when going out of scope.
pub struct ActionFuse {
    /// An optional action that will be invoked when the ActionFuse goes out of
    /// scope.
    actions: Vec<TriggeredAction>,
}

impl ActionFuse {
    /// Returns an ActionFuse reference with the given TriggerAction.
    pub(super) fn create(actions: Vec<TriggeredAction>) -> ActionFuseHandle {
        Arc::new(Mutex::new(ActionFuse { actions }))
    }

    /// Suppresses the action from automatically executing.
    pub(crate) fn defuse(handle: ActionFuseHandle) {
        fasync::Task::spawn(async move {
            let mut fuse = handle.lock().await;
            fuse.actions.clear();
        })
        .detach();
    }
}

impl Drop for ActionFuse {
    fn drop(&mut self) {
        while let Some(action) = self.actions.pop() {
            (action)();
        }
    }
}
