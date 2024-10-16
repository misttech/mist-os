// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::events::event::Event;
use crate::model::events::registry::ComponentEventRoute;
use ::routing::event::EventFilter;
use anyhow::{format_err, Error};
use cm_rust::DictionaryValue;
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::sink::SinkExt;
use hooks::{Event as ComponentEvent, EventPayload, TransferEvent};
use maplit::btreemap;
use moniker::ExtendedMoniker;

/// EventDispatcher and EventStream are two ends of a channel.
///
/// EventDispatcher represents the sending end of the channel.
///
/// An EventDispatcher receives events of a particular event type,
/// and dispatches though events out to the EventStream if they fall within
/// one of the scopes associated with the dispatcher.
///
/// EventDispatchers are owned by EventStreams. If an EventStream is dropped,
/// all corresponding EventDispatchers are dropped.
///
/// An EventStream is owned by the client - usually a test harness or a
/// EventSource. It receives a Event from an EventDispatcher and propagates it
/// to the client.
pub struct EventDispatcher {
    // The moniker of the component subscribing to events.
    subscriber: ExtendedMoniker,

    /// Specifies the realms that this EventDispatcher can dispatch events from and under what
    /// conditions.
    scopes: Vec<EventDispatcherScope>,

    /// An `mpsc::Sender` used to dispatch an event. Note that this
    /// `mpsc::Sender` is wrapped in an Mutex<..> to allow it to be passed along
    /// to other tasks for dispatch. The Event is a lifecycle event that occurred,
    /// and the Option<Vec<ComponentEventRoute>> is the path that the event
    /// took (if applicable) to reach the destination. This route
    /// is used for dynamic permission checks (to filter events a component shouldn't have
    /// access to), and to rebase the moniker of the event.
    tx: Mutex<mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>>,

    /// Route information used externally for evaluating scopes
    // TODO(https://fxbug.dev/332389972): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)]
    pub route: Vec<ComponentEventRoute>,
}

impl EventDispatcher {
    #[cfg(test)]
    pub fn new(
        subscriber: ExtendedMoniker,
        scopes: Vec<EventDispatcherScope>,
        tx: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
    ) -> Self {
        Self::new_with_route(subscriber, scopes, tx, vec![])
    }

    pub fn new_with_route(
        subscriber: ExtendedMoniker,
        scopes: Vec<EventDispatcherScope>,
        tx: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
        route: Vec<ComponentEventRoute>,
    ) -> Self {
        // TODO(https://fxbug.dev/42125209): flatten scope_monikers. There might be monikers that are
        // contained within another moniker in the list.
        Self { subscriber, scopes, tx: Mutex::new(tx), route }
    }

    /// Sends the event to an event stream, if fired in the scope of `scope_moniker`. Returns
    /// a responder which can be blocked on.
    pub async fn dispatch(&self, event: &ComponentEvent) -> Result<(), Error> {
        let maybe_scope = self.find_scope(&event);
        if maybe_scope.is_none() {
            return Err(format_err!("Could not find scope for event"));
        }
        let scope_moniker = maybe_scope.unwrap().moniker.clone();
        let mut tx = self.tx.lock().await;
        tx.send((Event { event: event.transfer().await, scope_moniker }, None)).await?;
        Ok(())
    }

    fn find_scope(&self, event: &ComponentEvent) -> Option<&EventDispatcherScope> {
        // TODO(https://fxbug.dev/42125209): once flattening of monikers is done, we would expect to have a single
        // moniker here. For now taking the first one and ignoring the rest.
        // Ensure that the event is coming from a realm within the scope of this dispatcher and
        // matching the path filter if one exists.
        self.scopes.iter().filter(|scope| scope.contains(&self.subscriber, &event)).next()
    }
}

/// A scope for dispatching and filters on that scope.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EventDispatcherScope {
    /// The moniker of the realm
    pub moniker: ExtendedMoniker,

    /// Filters for an event in that realm.
    pub filter: EventFilter,
}

impl EventDispatcherScope {
    pub fn new(moniker: ExtendedMoniker) -> Self {
        Self { moniker: moniker.clone(), filter: EventFilter::new(moniker, None) }
    }

    pub fn with_filter(mut self, filter: EventFilter) -> Self {
        self.filter = filter;
        self
    }

    /// For the top-level EventStreams and event strems used in unit tests in the c_m codebase we
    /// don't take filters into account.
    pub fn for_debug(mut self) -> Self {
        self.filter = EventFilter::debug(self.moniker.clone());
        self
    }

    /// Given the subscriber, indicates whether or not the event is contained
    /// in this scope.
    pub fn contains(&self, subscriber: &ExtendedMoniker, event: &ComponentEvent) -> bool {
        let in_scope = match &event.payload {
            EventPayload::CapabilityRequested { source_moniker, .. } => match &subscriber {
                ExtendedMoniker::ComponentManager => true,
                ExtendedMoniker::ComponentInstance(target) => *source_moniker == *target,
            },
            _ => {
                let contained_in_realm = event.target_moniker.has_prefix(&self.moniker);
                let is_component_instance = matches!(
                    &event.target_moniker,
                    ExtendedMoniker::ComponentInstance(instance) if instance.is_root()
                );
                contained_in_realm || is_component_instance
            }
        };

        if !in_scope {
            return false;
        }

        // TODO(fxbug/122227): Creating hashmaps on every lookup is not ideal, but in practice this
        // likely doesn't happen too often.
        let filterable_fields = match &event.payload {
            EventPayload::CapabilityRequested { name, .. } => Some(btreemap! {
                "name".to_string() => DictionaryValue::Str(name.into())
            }),
            _ => None,
        };
        self.filter.has_fields(&filterable_fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    use futures::StreamExt;
    use hooks::CapabilityReceiver;
    use moniker::Moniker;
    use sandbox::Message;
    use std::sync::Arc;

    struct EventDispatcherFactory {
        /// The receiving end of a channel of Events.
        rx: mpsc::UnboundedReceiver<(Event, Option<Vec<ComponentEventRoute>>)>,

        /// The sending end of a channel of Events.
        tx: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
    }

    impl EventDispatcherFactory {
        fn new() -> Self {
            let (tx, rx) = mpsc::unbounded();
            Self { rx, tx }
        }

        /// Receives the next event from the sender.
        pub async fn next_event(&mut self) -> Option<ComponentEvent> {
            self.rx.next().await.map(|(e, _)| e.event)
        }

        fn create_dispatcher(&self, subscriber: ExtendedMoniker) -> Arc<EventDispatcher> {
            let scopes = vec![EventDispatcherScope::new(Moniker::root().into()).for_debug()];
            Arc::new(EventDispatcher::new(subscriber, scopes, self.tx.clone()))
        }
    }

    async fn dispatch_capability_requested_event(
        dispatcher: &EventDispatcher,
        source_moniker: &Moniker,
    ) -> Result<(), Error> {
        let (_, capability_server_end) = zx::Channel::create();
        let (receiver, sender) = CapabilityReceiver::new();
        let event = ComponentEvent {
            target_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
            component_url: "fuchsia-pkg://root".parse().unwrap(),
            payload: EventPayload::CapabilityRequested {
                source_moniker: source_moniker.clone(),
                name: "foo".to_string(),
                receiver,
            },
            timestamp: zx::BootInstant::get(),
        };
        sender.send(Message { channel: capability_server_end }).unwrap();
        dispatcher.dispatch(&event).await
    }

    // This test verifies that the CapabilityRequested event can only be sent to a source
    // that matches its source moniker.
    #[fuchsia::test]
    async fn can_send_capability_requested_to_source() {
        // Verify we can dispatch to a debug source.
        // Sync events get a responder if the message was dispatched.
        let mut factory = EventDispatcherFactory::new();
        let dispatcher = factory.create_dispatcher(ExtendedMoniker::ComponentManager);
        let source_moniker = vec!["root:0", "a:0", "b:0", "c:0"].try_into().unwrap();
        assert!(dispatch_capability_requested_event(&dispatcher, &source_moniker).await.is_ok());
        assert_matches!(
            factory.next_event().await,
            Some(ComponentEvent { payload: EventPayload::CapabilityRequested { .. }, .. })
        );

        // Verify that we cannot dispatch the CapabilityRequested event to the root component.
        let subscriber = ExtendedMoniker::ComponentInstance(vec!["root:0"].try_into().unwrap());
        let dispatcher = factory.create_dispatcher(subscriber);
        assert!(dispatch_capability_requested_event(&dispatcher, &source_moniker).await.is_err());

        // Verify that we cannot dispatch the CapabilityRequested event to the root:0/a:0 component.
        let subscriber =
            ExtendedMoniker::ComponentInstance(vec!["root:0", "a:0"].try_into().unwrap());
        let dispatcher = factory.create_dispatcher(subscriber);
        assert!(dispatch_capability_requested_event(&dispatcher, &source_moniker).await.is_err());

        // Verify that we cannot dispatch the CapabilityRequested event to the root:0/a:0/b:0 component.
        let subscriber =
            ExtendedMoniker::ComponentInstance(vec!["root:0", "a:0", "b:0"].try_into().unwrap());
        let dispatcher = factory.create_dispatcher(subscriber);
        assert!(dispatch_capability_requested_event(&dispatcher, &source_moniker).await.is_err());

        // Verify that we CAN dispatch the CapabilityRequested event to the root:0/a:0/b:0/c:0 component.
        let subscriber = ExtendedMoniker::ComponentInstance(
            vec!["root:0", "a:0", "b:0", "c:0"].try_into().unwrap(),
        );
        let dispatcher = factory.create_dispatcher(subscriber);
        assert!(dispatch_capability_requested_event(&dispatcher, &source_moniker).await.is_ok());
        assert_matches!(
            factory.next_event().await,
            Some(ComponentEvent { payload: EventPayload::CapabilityRequested { .. }, .. })
        );

        // Verify that we cannot dispatch the CapabilityRequested event to the root:0/a:0/b:0/c:0/d:0 component.
        let subscriber = ExtendedMoniker::ComponentInstance(
            vec!["root:0", "a:0", "b:0", "c:0", "d:0"].try_into().unwrap(),
        );
        let dispatcher = factory.create_dispatcher(subscriber);
        assert!(dispatch_capability_requested_event(&dispatcher, &source_moniker).await.is_err());
    }
}
