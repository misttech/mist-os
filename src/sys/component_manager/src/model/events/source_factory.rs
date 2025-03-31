// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::{BuiltinCapability, CapabilityProvider, FrameworkCapability};
use crate::model::component::manager::ComponentManagerInstance;
use crate::model::component::{WeakComponentInstance, WeakExtendedInstance};
use crate::model::events::registry::EventRegistry;
use crate::model::events::source::EventSource;
use crate::model::events::stream_provider::EventStreamProvider;
use ::routing::capability_source::InternalCapability;
use std::sync::{Arc, Weak};

/// Allows to create `EventSource`s and tracks all the created ones.
pub struct EventSourceFactory {
    top_instance: Weak<ComponentManagerInstance>,

    /// The event registry. It subscribes to all events happening in the system and
    /// routes them to subscribers.
    // TODO(https://fxbug.dev/42125378): instead of using a global registry integrate more with the hooks model.
    event_registry: Weak<EventRegistry>,

    // The static event stream provider.
    event_stream_provider: Weak<EventStreamProvider>,
}

impl EventSourceFactory {
    pub fn new(
        top_instance: Weak<ComponentManagerInstance>,
        event_registry: Weak<EventRegistry>,
        event_stream_provider: Weak<EventStreamProvider>,
    ) -> Arc<Self> {
        Arc::new(Self { top_instance, event_registry, event_stream_provider })
    }

    /// Creates a `EventSource` for the given `subscriber`.
    fn create(&self, subscriber: WeakComponentInstance) -> EventSource {
        EventSource::new(
            WeakExtendedInstance::Component(subscriber),
            self.event_registry.clone(),
            self.event_stream_provider.clone(),
        )
    }

    pub fn create_for_above_root(&self) -> EventSource {
        EventSource::new(
            WeakExtendedInstance::AboveRoot(self.top_instance.clone()),
            self.event_registry.clone(),
            self.event_stream_provider.clone(),
        )
    }

    fn matches(&self, capability: &InternalCapability) -> bool {
        matches!(capability, InternalCapability::EventStream(_))
    }

    /// Returns an EventSource. An EventSource holds the component in which it
    /// will receive events.
    fn new_provider(&self, target: WeakComponentInstance) -> Box<dyn CapabilityProvider> {
        let event_source = self.create(target);
        Box::new(event_source)
    }
}

pub struct EventSourceFactoryCapability {
    host: Arc<EventSourceFactory>,
}

impl EventSourceFactoryCapability {
    pub fn new(host: Arc<EventSourceFactory>) -> Self {
        Self { host }
    }
}

impl BuiltinCapability for EventSourceFactoryCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        self.host.matches(capability)
    }

    fn new_provider(&self, target: WeakComponentInstance) -> Box<dyn CapabilityProvider> {
        self.host.new_provider(target)
    }
}

impl FrameworkCapability for EventSourceFactoryCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        self.host.matches(capability)
    }

    fn new_provider(
        &self,
        _scope: WeakComponentInstance,
        target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        self.host.new_provider(target)
    }
}
