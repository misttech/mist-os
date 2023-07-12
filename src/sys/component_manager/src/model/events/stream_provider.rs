// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::WeakExtendedInstance,
        error::ModelError,
        events::{
            error::EventsError,
            registry::{EventRegistry, EventSubscription},
            stream::EventStream,
        },
        hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
    },
    async_trait::async_trait,
    cm_rust::{ComponentDecl, UseDecl, UseEventStreamDecl},
    futures::lock::Mutex,
    moniker::{ExtendedMoniker, Moniker},
    std::{
        collections::HashMap,
        sync::{Arc, Weak},
    },
};

/// Contains the event stream and its name.
pub struct EventStreamAttachment {
    /// The name of this event stream.
    name: UseEventStreamDecl,
    /// The server end of a component's event stream.
    server_end: Option<EventStream>,
}

/// An absolute path to a directory within a specified component.
#[derive(Eq, Hash, PartialEq, Clone)]
struct AbsolutePath {
    /// The path where the event stream will be installed
    /// in target_moniker.
    path: String,
    /// The absolute path to the component that this path refers to.
    target_moniker: ExtendedMoniker,
}

/// Mutable event stream state, guarded by a mutex in the
/// EventStreamProvider which allows for mutation.
struct StreamState {
    /// A mapping from a component instance's InstancedMoniker, to the set of
    /// event streams and their corresponding paths in the component instance's out directory.
    streams: HashMap<ExtendedMoniker, Vec<EventStreamAttachment>>,

    /// Looks up subscriptions per component over a component's lifetime.
    /// This is used solely for removing subscriptions from the subscriptions HashMap when
    /// a component is purged.
    subscription_component_lookup:
        HashMap<ExtendedMoniker, HashMap<AbsolutePath, Vec<UseEventStreamDecl>>>,
}

/// Creates EventStreams on component resolution according to statically declared
/// event_streams, and passes them along to components on start.
pub struct EventStreamProvider {
    /// A shared reference to the event registry used to subscribe and dispatch events.
    registry: Weak<EventRegistry>,

    state: Arc<Mutex<StreamState>>,
}

impl EventStreamProvider {
    pub fn new(registry: Weak<EventRegistry>) -> Self {
        Self {
            registry,
            state: Arc::new(Mutex::new(StreamState {
                streams: HashMap::new(),
                subscription_component_lookup: HashMap::new(),
            })),
        }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "EventStreamProvider",
            vec![EventType::Destroyed, EventType::Resolved],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    pub async fn take_events(
        self: &Arc<Self>,
        target_moniker: ExtendedMoniker,
        path: String,
    ) -> Option<Vec<UseEventStreamDecl>> {
        let state = self.state.lock().await;
        state
            .subscription_component_lookup
            .get(&target_moniker)
            .and_then(|subscriptions| subscriptions.get(&AbsolutePath { path, target_moniker }))
            .map(|subscriptions_by_path| subscriptions_by_path.clone())
    }

    /// Creates a static event stream for any static capabilities (such as capability_requested)
    /// Static capabilities must be instantiated before component initialization to prevent race
    /// conditions.
    pub async fn create_static_event_stream(
        self: &Arc<Self>,
        subscriber: &WeakExtendedInstance,
        subscription: EventSubscription,
        path: String,
    ) -> Result<(), ModelError> {
        let registry = self.registry.upgrade().ok_or(EventsError::RegistryNotFound)?;
        let event_stream = registry.subscribe(subscriber, vec![subscription.clone()]).await?;
        let subscriber_moniker = subscriber.extended_moniker();
        let absolute_path = AbsolutePath { target_moniker: subscriber_moniker.clone(), path };
        let mut state = self.state.lock().await;
        let subscriptions =
            state.subscription_component_lookup.entry(subscriber_moniker.clone()).or_default();
        let path_list = subscriptions.entry(absolute_path).or_default();
        path_list.push(subscription.event_name.clone());
        let event_streams = state.streams.entry(subscriber_moniker.clone()).or_default();
        event_streams.push(EventStreamAttachment {
            name: subscription.event_name,
            server_end: Some(event_stream),
        });
        Ok(())
    }

    /// Returns the server end of the event stream with provided `name` associated with
    /// the component with the provided `target_moniker`. This method returns None if such a stream
    /// does not exist or the channel has already been taken.
    pub async fn take_static_event_stream(
        &self,
        target_moniker: &ExtendedMoniker,
        stream_name: &EventSubscription,
    ) -> Option<EventStream> {
        let mut state = self.state.lock().await;
        state
            .streams
            .get_mut(&target_moniker)
            .and_then(|event_streams| {
                event_streams
                    .iter_mut()
                    .find(|event_stream| event_stream.name == stream_name.event_name)
            })
            .and_then(|attachment| attachment.server_end.take())
    }

    async fn on_component_destroyed(self: &Arc<Self>, target_moniker: &Moniker) {
        let mut state = self.state.lock().await;
        // Remove all event streams associated with the `target_moniker` component.
        state.streams.remove(&ExtendedMoniker::ComponentInstance(target_moniker.clone()));
        state
            .subscription_component_lookup
            .remove(&ExtendedMoniker::ComponentInstance(target_moniker.clone()));
    }

    async fn on_component_resolved(
        self: &Arc<Self>,
        target: &WeakExtendedInstance,
        decl: &ComponentDecl,
    ) -> Result<(), ModelError> {
        for use_decl in &decl.uses {
            match use_decl {
                UseDecl::EventStream(decl) => {
                    self.create_static_event_stream(
                        target,
                        EventSubscription::new(decl.clone()),
                        decl.target_path.to_string(),
                    )
                    .await?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Hook for EventStreamProvider {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        let target_moniker = event
            .target_moniker
            .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)?;
        match &event.payload {
            EventPayload::Destroyed => {
                self.on_component_destroyed(target_moniker).await;
            }
            EventPayload::Resolved { decl, component, .. } => {
                self.on_component_resolved(
                    &WeakExtendedInstance::Component(component.clone()),
                    decl,
                )
                .await?;
            }
            _ => {}
        }
        Ok(())
    }
}
