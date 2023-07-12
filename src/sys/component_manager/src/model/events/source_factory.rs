// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource},
        model::{
            error::ModelError,
            events::{
                registry::EventRegistry, source::EventSource, stream_provider::EventStreamProvider,
            },
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
            model::Model,
        },
    },
    ::routing::capability_source::InternalCapability,
    async_trait::async_trait,
    moniker::{ExtendedMoniker, Moniker},
    std::sync::{Arc, Weak},
};

/// Allows to create `EventSource`s and tracks all the created ones.
pub struct EventSourceFactory {
    model: Weak<Model>,

    /// The event registry. It subscribes to all events happening in the system and
    /// routes them to subscribers.
    // TODO(fxbug.dev/48512): instead of using a global registry integrate more with the hooks model.
    event_registry: Weak<EventRegistry>,

    // The static event stream provider.
    event_stream_provider: Weak<EventStreamProvider>,
}

impl EventSourceFactory {
    pub fn new(
        model: Weak<Model>,
        event_registry: Weak<EventRegistry>,
        event_stream_provider: Weak<EventStreamProvider>,
    ) -> Self {
        Self { model, event_registry, event_stream_provider }
    }

    /// Creates the subscription to the required events.
    /// `DirectoryReady` used to track events and associate them with the component that needs them
    /// as well as the scoped that will be allowed. Also the EventSource protocol capability.
    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![
            // This hook provides the EventSource capability to components in the tree
            HooksRegistration::new(
                "EventSourceFactory",
                vec![EventType::CapabilityRouted],
                Arc::downgrade(self) as Weak<dyn Hook>,
            ),
        ]
    }

    /// Creates a `EventSource` for the given `subscriber`.
    async fn create(&self, subscriber: Moniker) -> Result<EventSource, ModelError> {
        EventSource::new(
            ExtendedMoniker::ComponentInstance(subscriber),
            self.model.clone(),
            self.event_registry.clone(),
            self.event_stream_provider.clone(),
        )
        .await
    }

    pub async fn create_for_above_root(&self) -> Result<EventSource, ModelError> {
        EventSource::new(
            ExtendedMoniker::ComponentManager,
            self.model.clone(),
            self.event_registry.clone(),
            self.event_stream_provider.clone(),
        )
        .await
    }

    /// Returns an EventSource. An EventSource holds an InstancedMoniker that
    /// corresponds to the component in which it will receive events.
    async fn on_capability_routed_async(
        self: Arc<Self>,
        capability_decl: &InternalCapability,
        target_moniker: Moniker,
        capability: Option<Box<dyn CapabilityProvider>>,
    ) -> Result<Option<Box<dyn CapabilityProvider>>, ModelError> {
        match capability_decl {
            InternalCapability::EventStream(_) => {
                let event_source = self.create(target_moniker).await?;
                return Ok(Some(Box::new(event_source)));
            }
            _ => {}
        }
        Ok(capability)
    }
}

#[async_trait]
impl Hook for EventSourceFactory {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        let target_moniker = event
            .target_moniker
            .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)?;
        match &event.payload {
            EventPayload::CapabilityRouted {
                source: CapabilitySource::Builtin { capability, .. },
                capability_provider,
            } => {
                let mut capability_provider = capability_provider.lock().await;
                *capability_provider = self
                    .on_capability_routed_async(
                        &capability,
                        target_moniker.clone(),
                        capability_provider.take(),
                    )
                    .await?;
            }
            EventPayload::CapabilityRouted {
                source: CapabilitySource::Framework { capability, .. },
                capability_provider,
            } => {
                let mut capability_provider = capability_provider.lock().await;
                *capability_provider = self
                    .on_capability_routed_async(
                        &capability,
                        target_moniker.clone(),
                        capability_provider.take(),
                    )
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
}
