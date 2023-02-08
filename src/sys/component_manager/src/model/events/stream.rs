// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        events::{
            dispatcher::{EventDispatcher, EventDispatcherScope},
            event::Event,
            registry::ComponentEventRoute,
        },
        hooks::EventPayload,
    },
    cm_rust::DictionaryValue,
    futures::{channel::mpsc, poll, stream::Peekable, task::Context, Stream, StreamExt},
    maplit::hashmap,
    moniker::ExtendedMoniker,
    routing::event::EventFilter,
    std::{
        pin::Pin,
        sync::{Arc, Weak},
        task::Poll,
    },
};

#[cfg(test)]
use {
    crate::model::hooks::{EventType, HasEventType},
    moniker::AbsoluteMoniker,
};

pub struct EventStream {
    /// The receiving end of a channel of Events.
    rx: Peekable<mpsc::UnboundedReceiver<(Event, Option<Vec<ComponentEventRoute>>)>>,

    /// The sending end of a channel of Events.
    tx: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,

    /// Filter for the stream. Events not matching this filter
    /// will be discarded by the stream.
    pub filter: EventFilter,

    /// Optional subscriber, required when filtering is used.
    pub subscriber: Option<ExtendedMoniker>,

    /// A vector of EventDispatchers to this EventStream.
    /// EventStream assumes ownership of the dispatchers. They are
    /// destroyed when this EventStream is destroyed.
    dispatchers: Vec<Arc<EventDispatcher>>,

    /// The route taken for this event stream.
    /// This is used for access control and namespacing during
    /// serving of the event stream.
    pub route: Vec<ComponentEventRoute>,
    /// Routing tasks associated with this event stream.
    /// Tasks associated with the stream will be terminated
    /// when the EventStream is destroyed.
    pub tasks: Vec<fuchsia_async::Task<()>>,
}

impl EventStream {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded();
        Self {
            rx: rx.peekable(),
            tx,
            dispatchers: vec![],
            route: vec![],
            tasks: vec![],
            filter: EventFilter::new(None),
            subscriber: None,
        }
    }

    pub fn create_dispatcher(
        &mut self,
        subscriber: ExtendedMoniker,
        scopes: Vec<EventDispatcherScope>,
        route: Vec<ComponentEventRoute>,
    ) -> Weak<EventDispatcher> {
        self.route = route.clone();
        let dispatcher =
            Arc::new(EventDispatcher::new_with_route(subscriber, scopes, self.tx.clone(), route));
        self.dispatchers.push(dispatcher.clone());
        Arc::downgrade(&dispatcher)
    }

    pub async fn next_or_none(
        &mut self,
    ) -> Option<Option<(Event, Option<Vec<ComponentEventRoute>>)>> {
        match poll!(Pin::new(&mut self.rx).peek()) {
            Poll::Ready(_) => Some(self.rx.next().await),
            Poll::Pending => None,
        }
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)> {
        self.tx.clone()
    }

    fn matches_filter(&self, event: &Event) -> bool {
        let filterable_fields = match &event.event.payload {
            EventPayload::CapabilityRequested { name, .. } => Some(hashmap! {
                "name".to_string() => DictionaryValue::Str(name.into())
            }),
            EventPayload::DirectoryReady { name, .. } => Some(hashmap! {
                "name".to_string() => DictionaryValue::Str(name.into())
            }),
            _ => None,
        };

        self.filter.has_fields(&filterable_fields)
    }

    /// Waits for an event with a particular EventType against a component with a
    /// particular moniker. Ignores all other events.
    #[cfg(test)]
    pub async fn wait_until(
        &mut self,
        expected_event_type: EventType,
        expected_moniker: AbsoluteMoniker,
    ) -> Option<Event> {
        let expected_moniker = ExtendedMoniker::ComponentInstance(expected_moniker);
        while let Some((event, _)) = self.next().await {
            let actual_event_type = event.event.event_type();
            if expected_moniker == event.event.target_moniker
                && expected_event_type == actual_event_type
            {
                return Some(event);
            }
        }
        None
    }
}

impl Stream for EventStream {
    type Item = (Event, Option<Vec<ComponentEventRoute>>);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Filter by filter. At the Stream layer we are aware of the correct UseEventStreamDecl,
        // whereas in the registry and dispatcher layers we (likely) receive incorrect
        // decls from routing in the case of duplicate capability_requested event streams.
        // Since the names are the same the routing layer has no way to differentiate the decls,
        // so at that layer we ignore the "filter" attribute and apply the filter at the stream
        // layer where we have full context.
        loop {
            let value = Pin::new(&mut self.rx).poll_next(cx);
            if let (Poll::Ready(Some((event, _))), Some(_moniker)) = (&value, &self.subscriber) {
                if !self.matches_filter(&event) {
                    continue;
                }
            }
            return value;
        }
    }
}
