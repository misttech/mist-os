// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        actions::{Action, ActionKey},
        component::{ComponentInstance, InstanceState, UnresolvedInstanceState},
        error::{ActionError, DiscoverActionError},
        hooks::{Event, EventPayload},
        structured_dict::ComponentInput,
    },
    async_trait::async_trait,
    std::sync::Arc,
};

/// Dispatches a `Discovered` event for a component instance. This action should be registered
/// when a component instance is created.
pub struct DiscoverAction {
    /// A struct holding the capabilities made available to this component by its parent.
    component_input: ComponentInput,
}

impl DiscoverAction {
    pub fn new(component_input: ComponentInput) -> Self {
        Self { component_input }
    }
}

#[async_trait]
impl Action for DiscoverAction {
    async fn handle(self, component: &Arc<ComponentInstance>) -> Result<(), ActionError> {
        do_discover(component, self.component_input).await.map_err(Into::into)
    }
    fn key(&self) -> ActionKey {
        ActionKey::Discover
    }
}

async fn do_discover(
    component: &Arc<ComponentInstance>,
    component_input: ComponentInput,
) -> Result<(), DiscoverActionError> {
    let is_discovered = {
        let state = component.lock_state().await;
        match *state {
            InstanceState::New => false,
            InstanceState::Unresolved(_)
            | InstanceState::Resolved(_)
            | InstanceState::Started(_, _)
            | InstanceState::Shutdown(_, _) => true,
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
            InstanceState::Shutdown(_, _) | InstanceState::Destroyed => {
                // Nothing to do.
            }
            InstanceState::Unresolved(_)
            | InstanceState::Resolved(_)
            | InstanceState::Started(_, _) => {
                panic!(
                    "Component was marked {:?} during Discover action, which shouldn't be possible",
                    *state
                );
            }
            InstanceState::New => {
                state.set(InstanceState::Unresolved(UnresolvedInstanceState::new(component_input)));
            }
        }
    }
    Ok(())
}
