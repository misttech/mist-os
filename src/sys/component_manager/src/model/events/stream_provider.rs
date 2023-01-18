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
    cm_rust::{ComponentDecl, UseDecl},
    futures::lock::Mutex,
    moniker::{AbsoluteMoniker, ExtendedMoniker},
    std::{
        collections::HashMap,
        sync::{Arc, Weak},
    },
};

/// V2 variant of EventStreamAttachment
/// contains the event stream and its name.
pub struct EventStreamAttachmentV2 {
    /// The name of this event stream.
    name: String,
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
    /// A mapping from a component instance's InstancedAbsoluteMoniker, to the set of
    /// event streams and their corresponding paths in the component instance's out directory.
    streams_v2: HashMap<ExtendedMoniker, Vec<EventStreamAttachmentV2>>,

    /// Looks up subscriptions per component over a component's lifetime.
    /// This is used solely for removing subscriptions from the subscriptions HashMap when
    /// a component is purged.
    subscription_component_lookup: HashMap<ExtendedMoniker, HashMap<AbsolutePath, Vec<String>>>,
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
                streams_v2: HashMap::new(),
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
    ) -> Option<Vec<String>> {
        let state = self.state.lock().await;
        if let Some(subscriptions) = state.subscription_component_lookup.get(&target_moniker) {
            if let Some(subscriptions_by_path) =
                subscriptions.get(&AbsolutePath { path, target_moniker })
            {
                return Some(subscriptions_by_path.clone());
            }
        }
        None
    }

    /// Creates a static event stream for any static capabilities (such as capability_requested)
    /// Static capabilities must be instantiated before component initialization to prevent race
    /// conditions.
    pub async fn create_v2_static_event_stream(
        self: &Arc<Self>,
        subscriber: &WeakExtendedInstance,
        stream_name: String,
        subscription: EventSubscription,
        path: String,
    ) -> Result<(), ModelError> {
        let registry = self.registry.upgrade().ok_or(EventsError::RegistryNotFound)?;
        let event_stream = registry.subscribe(subscriber, vec![subscription]).await?;
        let subscriber_moniker = subscriber.extended_moniker();
        let absolute_path = AbsolutePath { target_moniker: subscriber_moniker.clone(), path };
        let mut state = self.state.lock().await;
        if !state.subscription_component_lookup.contains_key(&subscriber_moniker) {
            state.subscription_component_lookup.insert(subscriber_moniker.clone(), HashMap::new());
        }
        if let Some(subscriptions) =
            state.subscription_component_lookup.get_mut(&subscriber_moniker)
        {
            if !subscriptions.contains_key(&absolute_path) {
                subscriptions.insert(absolute_path.clone(), vec![]);
            }
            let path_list = subscriptions.get_mut(&absolute_path).unwrap();
            path_list.push(stream_name.clone());
            let event_streams =
                state.streams_v2.entry(subscriber_moniker.clone()).or_insert(vec![]);
            event_streams.push(EventStreamAttachmentV2 {
                name: stream_name,
                server_end: Some(event_stream),
            });
        }
        Ok(())
    }

    /// Returns the server end of the event stream with provided `name` associated with
    /// the component with the provided `target_moniker`. This method returns None if such a stream
    /// does not exist or the channel has already been taken.
    pub async fn take_static_event_stream(
        &self,
        target_moniker: &ExtendedMoniker,
        stream_name: String,
    ) -> Option<EventStream> {
        let mut state = self.state.lock().await;
        if let Some(event_streams) = state.streams_v2.get_mut(&target_moniker) {
            if let Some(attachment) =
                event_streams.iter_mut().find(|event_stream| event_stream.name == stream_name)
            {
                return attachment.server_end.take();
            }
        }
        return None;
    }

    async fn on_component_destroyed(self: &Arc<Self>, target_moniker: &AbsoluteMoniker) {
        let mut state = self.state.lock().await;
        // Remove all event streams associated with the `target_moniker` component.
        state.streams_v2.remove(&ExtendedMoniker::ComponentInstance(target_moniker.clone()));
        state
            .subscription_component_lookup
            .remove(&ExtendedMoniker::ComponentInstance(target_moniker.clone()));
    }

    async fn try_route_v2_events(
        self: &Arc<Self>,
        subscriber: &WeakExtendedInstance,
        decl: &ComponentDecl,
    ) -> Result<bool, ModelError> {
        let mut routed_v2 = false;
        for use_decl in &decl.uses {
            match use_decl {
                UseDecl::EventStream(decl) => {
                    self.create_v2_static_event_stream(
                        subscriber,
                        decl.source_name.to_string(),
                        EventSubscription { event_name: decl.source_name.clone() },
                        decl.target_path.to_string(),
                    )
                    .await?;
                    routed_v2 = true;
                }
                _ => {}
            }
        }
        Ok(routed_v2)
    }

    async fn on_component_resolved(
        self: &Arc<Self>,
        target: &WeakExtendedInstance,
        decl: &ComponentDecl,
    ) -> Result<(), ModelError> {
        // NOTE: We can't have deprecated and new events
        // active in the same component.
        // To prevent this from happening we use conditional routing.
        // First, new events are routed, and if a single new event is routed
        // successfully we don't attempt to route legacy capabilities.
        // This logic will be removed once everything is switched to
        // v2 events.
        // TODO(https://fxbug.dev/81980): Remove once fully migrated.
        // Note that a component can still use v1 and v2 events, and can get the v1
        // events as long as all the v2 events fail to route.
        let err = match self.try_route_v2_events(target, decl).await {
            Ok(_) => None,
            Err(error) => Some(error),
        };
        if let Some(error) = err {
            return Err(error);
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
