// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        actions::{Action, ActionKey},
        component::{ComponentInstance, InstanceState, UnresolvedInstanceState},
        error::{ActionError, DiscoverActionError},
        hooks::{Event, EventPayload},
    },
    async_trait::async_trait,
    sandbox::Dict,
    std::sync::Arc,
};

/// Dispatches a `Discovered` event for a component instance. This action should be registered
/// when a component instance is created.
pub struct DiscoverAction {
    /// A Dict holding the capabilities made available to this component by its parent.
    dict: Dict,
}

impl DiscoverAction {
    pub fn new(dict: Dict) -> Self {
        Self { dict }
    }
}

#[async_trait]
impl Action for DiscoverAction {
    type Output = ();
    async fn handle(self, component: &Arc<ComponentInstance>) -> Result<Self::Output, ActionError> {
        do_discover(component, self.dict).await.map_err(Into::into)
    }
    fn key(&self) -> ActionKey {
        ActionKey::Discover
    }
}

async fn do_discover(
    component: &Arc<ComponentInstance>,
    dict: Dict,
) -> Result<(), DiscoverActionError> {
    let is_discovered = {
        let state = component.lock_state().await;
        match *state {
            InstanceState::New => false,
            InstanceState::Unresolved(_) => true,
            InstanceState::Resolved(_) => true,
            InstanceState::Destroyed => {
                return Err(DiscoverActionError::InstanceDestroyed {
                    moniker: component.moniker.clone(),
                });
            }
        }
    };
    if is_discovered {
        return Ok(());
    }
    let event = Event::new(&component, EventPayload::Discovered);
    component.hooks.dispatch(&event).await;
    {
        let mut state = component.lock_state().await;
        assert!(
            matches!(*state, InstanceState::New | InstanceState::Destroyed),
            "Component in unexpected state after discover"
        );
        match *state {
            InstanceState::Destroyed => {
                // Nothing to do.
            }
            InstanceState::Unresolved(_) | InstanceState::Resolved(_) => {
                panic!(
                    "Component was marked {:?} during Discover action, which shouldn't be possible",
                    *state
                );
            }
            InstanceState::New => {
                state.set(InstanceState::Unresolved(UnresolvedInstanceState::new(dict)));
            }
        }
    }
    Ok(())
}
