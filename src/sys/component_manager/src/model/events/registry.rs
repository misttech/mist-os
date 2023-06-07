// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, ExtendedInstance, InstanceState, WeakExtendedInstance},
            error::ModelError,
            events::{
                dispatcher::{EventDispatcher, EventDispatcherScope},
                error::EventsError,
                stream::EventStream,
                synthesizer::{EventSynthesisProvider, EventSynthesizer},
            },
            hooks::{Event as ComponentEvent, EventType, HasEventType, Hook, HooksRegistration},
            model::Model,
            routing::RouteSource,
        },
    },
    ::routing::{
        capability_source::InternalCapability, component_instance::ComponentInstanceInterface,
        event::EventFilter, mapper::NoopRouteMapper, route_event_stream,
    },
    async_trait::async_trait,
    cm_rust::{ChildRef, EventScope, UseDecl, UseEventStreamDecl},
    cm_types::Name,
    flyweights::FlyStr,
    futures::lock::Mutex,
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ChildMonikerBase, ExtendedMoniker},
    std::{
        collections::{HashMap, HashSet},
        sync::{Arc, Weak},
    },
};

pub type EventSubscription = ::routing::event::EventSubscription<UseEventStreamDecl>;

#[derive(Debug)]
pub struct RoutedEvent {
    pub source_name: Name,
    pub scopes: Vec<EventDispatcherScope>,
    pub route: Vec<ComponentEventRoute>,
}

#[derive(Debug)]
pub struct RequestedEventState {
    pub scopes: Vec<EventDispatcherScope>,
    pub route: Vec<ComponentEventRoute>,
}

impl RequestedEventState {
    pub fn new(route: Vec<ComponentEventRoute>) -> Self {
        Self { scopes: Vec::new(), route }
    }
}

#[derive(Debug)]
pub struct RouteEventsResult {
    /// Maps from source name to a mode and set of scope monikers.
    mapping: HashMap<Name, RequestedEventState>,
}

impl RouteEventsResult {
    fn new() -> Self {
        Self { mapping: HashMap::new() }
    }

    fn insert(
        &mut self,
        source_name: Name,
        scope: EventDispatcherScope,
        route: Vec<ComponentEventRoute>,
    ) {
        let event_state =
            self.mapping.entry(source_name).or_insert(RequestedEventState::new(route));
        if !event_state.scopes.contains(&scope) {
            event_state.scopes.push(scope);
        }
    }

    pub fn contains_event(&self, event_name: &Name) -> bool {
        self.mapping.contains_key(event_name)
    }

    pub fn to_vec(self) -> Vec<RoutedEvent> {
        self.mapping
            .into_iter()
            .map(|(source_name, state)| RoutedEvent {
                source_name,
                scopes: state.scopes,
                route: state.route,
            })
            .collect()
    }
}

/// Subscribes to events from multiple tasks and sends events to all of them.
pub struct EventRegistry {
    model: Weak<Model>,
    dispatcher_map: Arc<Mutex<HashMap<Name, Vec<Weak<EventDispatcher>>>>>,
    event_synthesizer: EventSynthesizer,
}

/// Contains routing information about an event.
/// This is used to downscope the moniker for the event
/// and filter events to only allowed components.
#[derive(Debug, Clone)]
pub struct ComponentEventRoute {
    /// Component child reference. We don't yet
    /// know a stronger type during routing, and the type of
    /// the object could change during runtime in the case of dynamic
    /// collections. Examples of things this could be:
    /// * <component_manager> -- refers to component manager if AboveRoot
    /// filtering is performed.
    /// * The name of a component relative to its parent
    /// * The name of a collection relative to its parent
    pub component: ChildRef,
    /// A list of scopes that this route applies to
    pub scope: Option<Vec<EventScope>>,
}

impl EventRegistry {
    pub fn new(model: Weak<Model>) -> Self {
        let event_synthesizer = EventSynthesizer::new(model.clone());
        Self { model, dispatcher_map: Arc::new(Mutex::new(HashMap::new())), event_synthesizer }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        let mut event_types = EventType::values();

        // Do not subscribe to CapabilityRouted. That is a component-framework-internal event.
        event_types.retain(|t| t != &EventType::CapabilityRouted);

        vec![
            // This hook must be registered with all events.
            // However, a task will only receive events to which it subscribed.
            HooksRegistration::new(
                "EventRegistry",
                event_types,
                Arc::downgrade(self) as Weak<dyn Hook>,
            ),
        ]
    }

    /// Register a provider for an synthesized event.
    pub fn register_synthesis_provider(
        &mut self,
        event: EventType,
        provider: Arc<dyn EventSynthesisProvider>,
    ) {
        self.event_synthesizer.register_provider(event, provider);
    }

    /// Subscribes to events of a provided set of EventTypes.
    pub async fn subscribe(
        &self,
        subscriber: &WeakExtendedInstance,
        subscriptions: Vec<EventSubscription>,
    ) -> Result<EventStream, ModelError> {
        // Register event capabilities if any. It identifies the sources of these events (might be
        // the parent or this component itself). It constructs an "allow-list tree" of events and
        // component instances.
        let mut event_names = HashSet::new();
        for subscription in subscriptions {
            if !event_names.insert(subscription.event_name.clone()) {
                return Err(
                    EventsError::duplicate_event(subscription.event_name.source_name).into()
                );
            }
        }

        let events = match subscriber.extended_moniker() {
            ExtendedMoniker::ComponentManager => event_names
                .iter()
                .map(|source_name| RoutedEvent {
                    source_name: source_name.source_name.clone(),
                    scopes: vec![
                        EventDispatcherScope::new(AbsoluteMoniker::root().into()).for_debug()
                    ],
                    route: vec![],
                })
                .collect(),
            ExtendedMoniker::ComponentInstance(target_moniker) => {
                let route_result = self.route_events(&target_moniker, &event_names).await?;
                // Each target name that we routed, will have an associated scope. The number of
                // scopes must be equal to the number of target names.
                let total_scopes: usize =
                    route_result.mapping.values().map(|state| state.scopes.len()).sum();
                if total_scopes != event_names.len() {
                    let names = event_names
                        .into_iter()
                        .filter(|event_name| !route_result.contains_event(&event_name.source_name))
                        .map(|name| name.source_name)
                        .collect();
                    return Err(EventsError::not_available(names).into());
                }
                route_result.to_vec()
            }
        };

        self.subscribe_with_routed_events(&subscriber, events).await
    }

    pub async fn subscribe_with_routed_events(
        &self,
        subscriber: &WeakExtendedInstance,
        events: Vec<RoutedEvent>,
    ) -> Result<EventStream, ModelError> {
        // TODO(fxbug.dev/48510): get rid of this channel and use FIDL directly.
        let mut event_stream = EventStream::new();

        let mut dispatcher_map = self.dispatcher_map.lock().await;
        for event in &events {
            let dispatchers = dispatcher_map.entry(event.source_name.clone()).or_insert(vec![]);
            let dispatcher = event_stream.create_dispatcher(
                subscriber.extended_moniker(),
                event.scopes.clone(),
                event.route.clone(),
            );
            dispatchers.push(dispatcher);
        }
        // This function will be called once per event. We need to preserve the routing information
        // from that event in order to determine which events should and shouldn't be routed to the
        // listener.
        if events.len() == 1 {
            event_stream.route = events[0].route.clone();
        }

        let task_scope = match subscriber.upgrade()? {
            ExtendedInstance::Component(subscriber) => subscriber.nonblocking_task_scope(),
            ExtendedInstance::AboveRoot(subscriber) => subscriber.task_scope(),
        };
        let events = events.into_iter().map(|event| (event.source_name, event.scopes)).collect();
        self.event_synthesizer.spawn_synthesis(event_stream.sender(), events, &task_scope).await;

        Ok(event_stream)
    }

    // TODO(fxbug.dev/48510): get rid of this
    /// Sends the event to all dispatchers and waits to be unblocked by all
    async fn dispatch(&self, event: &ComponentEvent) {
        // Copy the senders so we don't hold onto the sender map lock
        // If we didn't do this, it is possible to deadlock while holding onto this lock.
        // For example,
        // Task A : call dispatch(event1) -> lock on sender map -> send -> wait for responders
        // Task B : call dispatch(event2) -> lock on sender map
        // If task B was required to respond to event1, then this is a deadlock.
        // Neither task can make progress.
        let dispatchers = {
            let mut dispatcher_map = self.dispatcher_map.lock().await;
            if let Some(dispatchers) = dispatcher_map.get_mut(&event.event_type().into()) {
                let mut strong_dispatchers = vec![];
                dispatchers.retain(|dispatcher| {
                    if let Some(dispatcher) = dispatcher.upgrade() {
                        strong_dispatchers.push(dispatcher);
                        true
                    } else {
                        false
                    }
                });
                strong_dispatchers
            } else {
                // There were no senders for this event. Do nothing.
                return;
            }
        };

        for dispatcher in &dispatchers {
            // A send can fail if the EventStream was dropped. We don't
            // crash the system when this happens. It is perfectly
            // valid for a EventStream to be dropped. That simply means
            // that the EventStream is no longer interested in future
            // events.
            let _ = dispatcher.dispatch(event).await;
        }
    }

    /// Routes a list of events to a specified moniker.
    /// Returns the event_stream capabilities.
    pub async fn route_events(
        &self,
        target_moniker: &AbsoluteMoniker,
        events: &HashSet<UseEventStreamDecl>,
    ) -> Result<RouteEventsResult, ModelError> {
        let model = self.model.upgrade().ok_or(EventsError::ModelNotAvailable)?;
        let component = model.look_up(&target_moniker).await?;
        let decl = {
            let state = component.lock_state().await;
            match *state {
                InstanceState::New | InstanceState::Unresolved => {
                    // This should never happen. By this point,
                    // we've validated that the instance state should
                    // be resolved because we're routing events to it.
                    // If not, this is an internal error from which we can't recover,
                    // and indicates a bug in component manager.
                    unreachable!("route_events: not resolved");
                }
                InstanceState::Resolved(ref s) => s.decl().clone(),
                InstanceState::Destroyed => {
                    return Err(ModelError::EventsError { err: EventsError::InstanceDestroyed });
                }
            }
        };

        let mut result = RouteEventsResult::new();
        for use_decl in decl.uses {
            match use_decl {
                UseDecl::EventStream(event_decl) => {
                    if events.iter().find(|value| *value == &event_decl).is_some() {
                        let (source_name, scope_moniker, route) =
                            Self::route_single_event(event_decl.clone(), &component).await?;
                        let mut scope = EventDispatcherScope::new(scope_moniker);
                        if let Some(filter) = event_decl.filter {
                            scope = scope.with_filter(EventFilter::new(Some(filter)));
                        }
                        result.insert(source_name, scope, route);
                    }
                }
                _ => {}
            }
        }

        Ok(result)
    }

    /// Routes an event and returns its source name and scope on success.
    async fn route_single_event(
        event_decl: UseEventStreamDecl,
        component: &Arc<ComponentInstance>,
    ) -> Result<(Name, ExtendedMoniker, Vec<ComponentEventRoute>), ModelError> {
        let mut components = vec![];
        let mut route = vec![];
        let route_source = route_event_stream(
            event_decl.clone(),
            component,
            &mut NoopRouteMapper,
            &mut components,
        )
        .await?;
        // Handle scope in "use" clause

        let mut search_name: Name = event_decl.source_name;
        if let Some(moniker) = component.child_moniker() {
            route.push(ComponentEventRoute {
                component: ChildRef {
                    name: FlyStr::new(moniker.name()),
                    collection: moniker.collection().map(|value| FlyStr::new(value)),
                },
                scope: event_decl.scope,
            });
        }
        for component in components {
            let mut component_route = ComponentEventRoute {
                component: if let Some(moniker) = component.component.child_moniker() {
                    ChildRef {
                        name: FlyStr::new(moniker.name.to_string()),
                        collection: moniker
                            .collection
                            .as_ref()
                            .map(|value| FlyStr::new(value.as_str())),
                    }
                } else {
                    ChildRef { name: FlyStr::new("<root>"), collection: None }
                },
                scope: None,
            };
            if let Some(stream) = component.offer {
                if stream.target_name == search_name {
                    search_name = stream.source_name;
                    component_route.scope = stream.scope;
                }
            }
            route.push(component_route);
        }
        match route_source {
            RouteSource {
                source:
                    CapabilitySource::Framework {
                        capability: InternalCapability::EventStream(source_name),
                        component,
                    },
                relative_path: _,
            } => Ok((source_name, component.abs_moniker.into(), route)),
            RouteSource {
                source:
                    CapabilitySource::Builtin {
                        capability: InternalCapability::EventStream(source_name),
                        ..
                    },
                relative_path: _,
            } => Ok((source_name, ExtendedMoniker::ComponentManager, route)),
            _ => unreachable!(),
        }
    }

    #[cfg(test)]
    async fn dispatchers_per_event_type(&self, event_type: EventType) -> usize {
        let dispatcher_map = self.dispatcher_map.lock().await;
        dispatcher_map
            .get(&event_type.into())
            .map(|dispatchers| dispatchers.len())
            .unwrap_or_default()
    }
}

#[async_trait]
impl Hook for EventRegistry {
    async fn on(self: Arc<Self>, event: &ComponentEvent) -> Result<(), ModelError> {
        self.dispatch(event).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            hooks::{Event as ComponentEvent, EventPayload},
            testing::test_helpers::{TestModelResult, *},
        },
        assert_matches::assert_matches,
        cm_rust::{Availability, UseSource},
        fuchsia_zircon as zx,
        futures::StreamExt,
        moniker::AbsoluteMoniker,
        std::str::FromStr,
    };

    async fn dispatch_capability_requested_event(registry: &EventRegistry) {
        let (_, capability_server_end) = zx::Channel::create();
        let capability_server_end = Arc::new(Mutex::new(Some(capability_server_end)));
        let event = ComponentEvent::new_for_test(
            AbsoluteMoniker::root(),
            "fuchsia-pkg://root",
            EventPayload::CapabilityRequested {
                source_moniker: AbsoluteMoniker::root(),
                name: "foo".to_string(),
                capability: capability_server_end,
            },
        );
        registry.dispatch(&event).await;
    }

    async fn dispatch_fake_event(registry: &EventRegistry) {
        let event = ComponentEvent::new_for_test(
            AbsoluteMoniker::root(),
            "fuchsia-pkg://root",
            EventPayload::Discovered,
        );
        registry.dispatch(&event).await;
    }

    #[fuchsia::test]
    async fn drop_dispatcher_when_event_stream_dropped() {
        let TestModelResult { model, .. } = TestEnvironmentBuilder::new().build().await;
        let event_registry = EventRegistry::new(Arc::downgrade(&model));

        assert_eq!(0, event_registry.dispatchers_per_event_type(EventType::Discovered).await);
        let mut event_stream_a = event_registry
            .subscribe(
                &WeakExtendedInstance::AboveRoot(Arc::downgrade(model.top_instance())),
                vec![EventSubscription::new(UseEventStreamDecl {
                    source_name: EventType::Discovered.into(),
                    source: UseSource::Parent,
                    scope: None,
                    target_path: cm_types::Path::from_str("/svc/fuchsia.component.EventStream")
                        .unwrap(),
                    filter: None,
                    availability: Availability::Required,
                })],
            )
            .await
            .expect("subscribe succeeds");

        assert_eq!(1, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        let mut event_stream_b = event_registry
            .subscribe(
                &WeakExtendedInstance::AboveRoot(Arc::downgrade(model.top_instance())),
                vec![EventSubscription::new(UseEventStreamDecl {
                    source_name: EventType::Discovered.into(),
                    source: UseSource::Parent,
                    scope: None,
                    target_path: cm_types::Path::from_str("/svc/fuchsia.component.EventStream")
                        .unwrap(),
                    filter: None,
                    availability: Availability::Required,
                })],
            )
            .await
            .expect("subscribe succeeds");

        assert_eq!(2, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        dispatch_fake_event(&event_registry).await;

        // Verify that both EventStreams receive the event.
        assert!(event_stream_a.next().await.is_some());
        assert!(event_stream_b.next().await.is_some());
        assert_eq!(2, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        drop(event_stream_a);

        // EventRegistry won't drop EventDispatchers until an event is dispatched.
        assert_eq!(2, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        dispatch_fake_event(&event_registry).await;

        assert!(event_stream_b.next().await.is_some());
        assert_eq!(1, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        drop(event_stream_b);

        dispatch_fake_event(&event_registry).await;
        assert_eq!(0, event_registry.dispatchers_per_event_type(EventType::Discovered).await);
    }

    #[fuchsia::test]
    async fn capability_requested_over_two_event_streams() {
        let TestModelResult { model, .. } = TestEnvironmentBuilder::new().build().await;
        let event_registry = EventRegistry::new(Arc::downgrade(&model));

        assert_eq!(
            0,
            event_registry.dispatchers_per_event_type(EventType::CapabilityRequested).await
        );

        let mut event_stream_a = event_registry
            .subscribe(
                &WeakExtendedInstance::AboveRoot(Arc::downgrade(model.top_instance())),
                vec![EventSubscription::new(UseEventStreamDecl {
                    source_name: EventType::CapabilityRequested.into(),
                    source: UseSource::Parent,
                    scope: None,
                    target_path: cm_types::Path::from_str("/svc/fuchsia.component.EventStream")
                        .unwrap(),
                    filter: None,
                    availability: Availability::Required,
                })],
            )
            .await
            .expect("subscribe succeeds");

        assert_eq!(
            1,
            event_registry.dispatchers_per_event_type(EventType::CapabilityRequested).await
        );

        let mut event_stream_b = event_registry
            .subscribe(
                &WeakExtendedInstance::AboveRoot(Arc::downgrade(model.top_instance())),
                vec![EventSubscription::new(UseEventStreamDecl {
                    source_name: EventType::CapabilityRequested.into(),
                    source: UseSource::Parent,
                    scope: None,
                    target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                    filter: None,
                    availability: Availability::Required,
                })],
            )
            .await
            .expect("subscribe succeeds");

        assert_eq!(
            2,
            event_registry.dispatchers_per_event_type(EventType::CapabilityRequested).await
        );

        dispatch_capability_requested_event(&event_registry).await;

        let event_a = event_stream_a.next().await.map(|(event, _)| event.event).unwrap();

        // Verify that we received a valid CapabilityRequested event.
        assert_matches!(event_a.payload, EventPayload::CapabilityRequested { .. });

        let event_b = event_stream_b.next().await.map(|(event, _)| event.event).unwrap();

        // Verify that we received a valid CapabilityRequested event.
        assert_matches!(event_b.payload, EventPayload::CapabilityRequested { .. });
    }
}
