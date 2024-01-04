// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        events::{event::Event, registry::ComponentEventRoute},
        hooks::{Event as ComponentEvent, EventPayload, TransferEvent},
    },
    ::routing::event::EventFilter,
    anyhow::{format_err, Error},
    cm_rust::DictionaryValue,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{channel::mpsc, lock::Mutex, sink::SinkExt},
    maplit::btreemap,
    moniker::{ExtendedMoniker, MonikerBase},
    std::path::Path,
    std::sync::{self, Arc},
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope},
};

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
        // TODO(https://fxbug.dev/48360): flatten scope_monikers. There might be monikers that are
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

        let should_dispatch =
            if let EventPayload::CapabilityRequested { capability, flags, relative_path, .. } =
                &event.payload
            {
                Self::handle_capability_requested_vfs(capability, *flags, relative_path).await
            } else {
                true
            };

        let scope_moniker = maybe_scope.unwrap().moniker.clone();

        if should_dispatch {
            let mut tx = self.tx.lock().await;
            tx.send((Event { event: event.transfer().await, scope_moniker }, None)).await?;
        }
        Ok(())
    }

    // Pass the channel through a vfs before sending it to the component. This ensures that
    // channels delivered through [CapabilityRequested] are [fuchsia.io] compliant.
    //
    // Returns true if dispatcher should dispatch the event. Will return false if the vfs
    // handled the request without handing control to the user provided callback (for example,
    // NODE_REFERENCE flag was set).
    async fn handle_capability_requested_vfs(
        capability_lock: &Arc<Mutex<Option<zx::Channel>>>,
        flags: fio::OpenFlags,
        relative_path: &Path,
    ) -> bool {
        let mut capability = capability_lock.lock().await;
        if capability.is_none() {
            return true;
        }
        let relative_path = match relative_path.to_string_lossy() {
            s if s.is_empty() => vfs::path::Path::dot(),
            s => match vfs::path::Path::validate_and_split(s) {
                Ok(s) => s,
                Err(_) => {
                    // Shouldn't happen, if it does just skip.
                    return false;
                }
            },
        };
        let server_end = capability.take().unwrap();

        let server_end_return = Arc::new(sync::Mutex::new(None));
        let server_end_return2 = server_end_return.clone();
        let service = vfs::service::endpoint(
            move |_scope: ExecutionScope, server_end: fuchsia_async::Channel| {
                let mut server_end_return = server_end_return2.lock().unwrap();
                *server_end_return = Some(server_end.into_zx_channel());
            },
        );
        service.open(ExecutionScope::new(), flags, relative_path, server_end.into());

        let mut server_end_return = server_end_return.lock().unwrap();
        if let Some(server_end) = server_end_return.take() {
            *capability = Some(server_end);
            true
        } else {
            false
        }
    }

    fn find_scope(&self, event: &ComponentEvent) -> Option<&EventDispatcherScope> {
        // TODO(https://fxbug.dev/48360): once flattening of monikers is done, we would expect to have a single
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
        Self { moniker, filter: EventFilter::new(None) }
    }

    pub fn with_filter(mut self, filter: EventFilter) -> Self {
        self.filter = filter;
        self
    }

    /// For the top-level EventStreams and event strems used in unit tests in the c_m codebase we
    /// don't take filters into account.
    pub fn for_debug(mut self) -> Self {
        self.filter = EventFilter::debug();
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
            EventPayload::DirectoryReady { name, .. } => Some(btreemap! {
                "name".to_string() => DictionaryValue::Str(name.into())
            }),
            _ => None,
        };
        self.filter.has_fields(&filterable_fields)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fuchsia_zircon as zx,
        futures::StreamExt,
        moniker::{Moniker, MonikerBase},
        std::{convert::TryInto, sync::Arc},
    };

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
        let capability_server_end = Arc::new(Mutex::new(Some(capability_server_end)));
        let event = ComponentEvent::new_for_test(
            Moniker::root(),
            "fuchsia-pkg://root/a/b/c",
            EventPayload::CapabilityRequested {
                source_moniker: source_moniker.clone(),
                name: "foo".to_string(),
                capability: capability_server_end,
                flags: fio::OpenFlags::empty(),
                relative_path: "".into(),
            },
        );
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
