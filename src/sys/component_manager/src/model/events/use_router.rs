// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ComponentInstance, ExtendedInstance, WeakComponentInstance};
use crate::model::events::hook_observer::HookObserver;
use crate::model::events::{forward_capability_requested_events, names_from_filter};
use async_trait::async_trait;
use cm_types::Name;
use cm_util::TaskGroup;
use fidl::endpoints::{DiscoverableProtocolMarker, RequestStream};
use fidl_fuchsia_inspect::InspectSinkMarker;
use fuchsia_inspect::Inspector;
use futures::channel::mpsc;
use futures::future::FutureExt;
use futures::stream::{FuturesUnordered, Peekable, StreamExt};
use hooks::EventType;
use inspect_runtime::{publish, PublishOptions};
use measure_tape_for_events::Measurable;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use routing::bedrock::request_metadata::{event_stream_metadata, Metadata};
use routing::bedrock::sandbox_construction::{EventStreamFilter, EventStreamSourceRouter};
use routing::component_instance::ComponentInstanceInterface;
use routing::WeakInstanceTokenExt;
use sandbox::{
    Capability, Connector, Data, Dict, Receiver, Request, Routable, Router, RouterResponse,
};
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::Poll;
use zx::sys::{ZX_CHANNEL_MAX_MSG_BYTES, ZX_CHANNEL_MAX_MSG_HANDLES};
use {fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_internal as finternal};

/// Event stream routes return dictionaries of data, detailing which event streams the client may
/// connect to and the scopes of those streams that was discovered during routing. An
/// `EventStreamUseRouter` is a connector router, which will route all of the backing dictionaries
/// and then present a single unified event stream over the connector it returns based on the data
/// found in the routed dictionaries.
pub struct EventStreamUseRouter {
    sources: Vec<EventStreamSourceRouter>,
}

#[async_trait]
impl Routable<Connector> for EventStreamUseRouter {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<Connector>, RouterError> {
        let request = request.expect("missing request");
        if debug {
            return self.route_debug(request).await;
        }

        let mut routing_tasks = FuturesUnordered::new();
        for source_route in self.sources.iter() {
            let metadata = event_stream_metadata(
                source_route.availability,
                source_route.route_metadata.clone(),
            );
            let request = Request { metadata, target: request.target.clone() };
            let filter = source_route.filter.clone();
            routing_tasks.push(
                source_route.router.route(Some(request), false).map(move |res| (res, filter)),
            );
        }
        let mut routed_dictionaries = vec![];
        while let Some((result, filter)) = routing_tasks.next().await {
            match result? {
                RouterResponse::Capability(dictionary) => {
                    routed_dictionaries.push((dictionary, filter))
                }
                RouterResponse::Unavailable => continue,
                RouterResponse::Debug(_) => {
                    panic!("non-debug route returned debug info, which is disallowed")
                }
            }
        }
        let Ok(ExtendedInstance::Component(target_component)) = request.target.upgrade() else {
            return Err(RouterError::Internal);
        };
        let (connector, use_receiver) =
            EventStreamUseReceiver::new(routed_dictionaries, target_component.clone());
        target_component.nonblocking_task_group().spawn(use_receiver.run());
        log::warn!("new connection to event stream from {}", &target_component.moniker);
        Ok(RouterResponse::Capability(connector))
    }
}

impl EventStreamUseRouter {
    pub fn new(sources: Vec<EventStreamSourceRouter>) -> Router<Connector> {
        Router::new(Self { sources })
    }

    async fn route_debug(
        &self,
        request: Request,
    ) -> Result<RouterResponse<Connector>, RouterError> {
        let mut routing_tasks = FuturesUnordered::new();
        for source_route in self.sources.iter() {
            let metadata = event_stream_metadata(
                source_route.availability,
                source_route.route_metadata.clone(),
            );
            let request = Request { metadata, target: request.target.clone() };
            routing_tasks.push(source_route.router.route(Some(request), true));
        }
        // Our router might route multiple capabilities to perform its job. In this case the
        // current API isn't a great fit, because we'll get multiple capability sources, but we can
        // only return one. Let's ensure that all of our routes complete successfully, and then
        // return the result from one of them. The only possible source for an event stream is a
        // built-in capability anyway.
        let mut any_result = None;
        while let Some(result) = routing_tasks.next().await {
            match result {
                Ok(result) => any_result = Some(result),
                Err(e) => return Err(e),
            }
        }
        match any_result {
            Some(RouterResponse::Debug(data)) => Ok(RouterResponse::Debug(data)),
            Some(RouterResponse::Unavailable) => Ok(RouterResponse::Unavailable),
            Some(RouterResponse::Capability(_)) => {
                panic!("debug route returned capability, which is disallowed")
            }
            None => panic!("no result produced, is sources empty?"),
        }
    }
}

/// An `EventStreamUseReceiver` implements the server side of an event stream. Given the routed
/// dictionaries of data found by an `EventStreamUseRouter`, it will implement the server end of
/// `fuchsia.component.EventStream` on any channels that are delivered to its `receiver`.
struct EventStreamUseReceiver {
    routed_dictionaries: Vec<(Dict, EventStreamFilter)>,
    weak_target_component: WeakComponentInstance,
    receiver: Receiver,
}

impl EventStreamUseReceiver {
    fn new(
        routed_dictionaries: Vec<(Dict, EventStreamFilter)>,
        target_component: Arc<ComponentInstance>,
    ) -> (Connector, Self) {
        let (receiver, connector) = Connector::new();
        let weak_target_component = target_component.as_weak();
        (connector, Self { routed_dictionaries, weak_target_component, receiver })
    }

    async fn run(self) {
        while let Some(message) = self.receiver.receive().await {
            let Ok(target_component) = self.weak_target_component.upgrade() else {
                return;
            };
            let (hooks, mpsc_receiver) = self.set_up_hooks(&target_component).await;
            target_component.nonblocking_task_group().spawn(Self::handle_stream(
                message.channel,
                mpsc_receiver,
                hooks,
            ))
        }
    }

    /// Creates and registers the necessary hooks for us to observe the events that the client
    /// wants to see.
    async fn set_up_hooks(
        &self,
        target_component: &Arc<ComponentInstance>,
    ) -> (Vec<Arc<HookObserver>>, mpsc::UnboundedReceiver<fcomponent::Event>) {
        let mut hooks = vec![];
        let (mpsc_sender, mpsc_receiver) = mpsc::unbounded();
        for (dictionary, filter) in &self.routed_dictionaries {
            let route_metadata: finternal::EventStreamRouteMetadata =
                dictionary.get_metadata().expect("missing route metadata");
            let capability_name = match dictionary.get(&Name::new("event_stream_name").unwrap()) {
                Ok(Some(Capability::Data(Data::String(name)))) => name,
                other_value => {
                    panic!("missing or unexpected value for event_stream_name: {:?}", other_value)
                }
            };
            let event_type = EventType::try_from(capability_name).expect("invalid event type");
            if event_type == EventType::CapabilityRequested {
                let Ok(resolved_state) = target_component.lock_resolved_state().await else {
                    // The component must have been shut down between when we got this connection
                    // and this moment. Since we're in a task group tied to the component, we'll
                    // surely stop execution any moment now. Let's just ignore this event, as
                    // that's the easiest thing to do here.
                    continue;
                };
                let Some(names) = names_from_filter(filter) else {
                    panic!("missing filter?");
                };
                let (_hook, capability_requested_receiver_lock) = resolved_state
                    .capability_requested_receivers
                    .clone()
                    .get(&names)
                    .expect("missing capability requested receiver")
                    .clone();
                Self::maybe_send_component_manager_inspect_event(
                    target_component.nonblocking_task_group(),
                    &mpsc_sender,
                    target_component.context.inspector(),
                    &route_metadata,
                    filter,
                );
                target_component.nonblocking_task_group().spawn(
                    forward_capability_requested_events(
                        mpsc_sender.clone(),
                        capability_requested_receiver_lock,
                    ),
                );
            } else {
                let hook_observer = Arc::new(HookObserver {
                    event_type,
                    subscriber: target_component.moniker.clone(),
                    route_metadata,
                    sender: mpsc_sender.clone(),
                    weak_task_group: target_component.nonblocking_task_group().as_weak(),
                    filter: filter.clone(),
                });
                target_component.hooks.install(hook_observer.hooks()).await;
                hooks.push(hook_observer);
            }
        }
        (hooks, mpsc_receiver)
    }

    async fn handle_stream(
        channel: zx::Channel,
        receiver: mpsc::UnboundedReceiver<fcomponent::Event>,
        _hooks: Vec<Arc<HookObserver>>,
    ) {
        let mut stream = fcomponent::EventStreamRequestStream::from_channel(
            fidl::AsyncChannel::from_channel(channel),
        );
        let mut receiver = pin!(receiver.peekable());
        while let Some(result) = stream.next().await {
            match result {
                Err(_err) => return,
                Ok(fcomponent::EventStreamRequest::WaitForReady { responder, .. }) => {
                    let _ = responder.send();
                }
                Ok(fcomponent::EventStreamRequest::GetNext { responder, .. }) => {
                    let Some(events) = Self::get_next_events(&mut receiver).await else {
                        // If we fail to get more events, then all of our senders have been closed.
                        // This is likely due to the target component shutting down.
                        return;
                    };
                    let _ = responder.send(events);
                }
            }
        }
    }

    /// Returns the next set of events from `receiver`. The returned vector will have at least one
    /// event, and not be too large to fit into a channel message. Once the first event is found,
    /// it will keep pulling events from `receiver` until it is empty (or our vector is full) and
    /// then return.
    ///
    /// Returns `None` if `receiver` is closed.
    async fn get_next_events(
        receiver: &mut Pin<&mut Peekable<mpsc::UnboundedReceiver<fcomponent::Event>>>,
    ) -> Option<Vec<fcomponent::Event>> {
        let mut bytes_used = 0;
        let mut handles_used = 0;
        let mut events = vec![];
        loop {
            let next_event = receiver.as_mut().peek().await?;
            let measure_tape = next_event.measure();
            bytes_used += measure_tape.num_bytes;
            handles_used += measure_tape.num_handles;
            if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize
                || handles_used > ZX_CHANNEL_MAX_MSG_HANDLES as usize
            {
                return Some(events);
            }
            events.push(receiver.next().await.expect("peek() said that the next item exists"));
            match futures::poll!(receiver.as_mut().peek()) {
                Poll::Pending => {
                    // There are no more events queued up in our receiver right now, and it's
                    // better to get events to the client quickly than to wait for us to fill our
                    // buffer. Let's return what we have.
                    return Some(events);
                }
                Poll::Ready(_) => {
                    // There's another event ready for us! Let's loop so we can process it.
                }
            }
        }
    }

    /// When a component is connecting to a capability requested event stream that has the inspect
    /// sink within its filter, we want to provide that component with component manager's inspect
    /// sink. This function will check to see if these conditions apply, and then generate that
    /// event if so.
    fn maybe_send_component_manager_inspect_event(
        task_group: TaskGroup,
        sender: &mpsc::UnboundedSender<fcomponent::Event>,
        inspector: &Inspector,
        route_metadata: &finternal::EventStreamRouteMetadata,
        filter: &EventStreamFilter,
    ) {
        if route_metadata.scope_moniker.is_some() {
            // We're not in scope, so let's not send an event
            return;
        }
        match names_from_filter(filter) {
            Some(names) if names.contains(&InspectSinkMarker::PROTOCOL_NAME.into()) => (),
            // The inspect sink is not in this filter, so let's not send an event
            _ => return,
        }
        let (client, server) = fidl::endpoints::create_endpoints();
        let Some(server_task) =
            publish(inspector, PublishOptions::default().on_inspect_sink_client(client))
        else {
            log::warn!("failed to publish component manager inspect tree");
            return;
        };
        task_group.spawn(server_task);

        let event = fcomponent::Event {
            header: Some(fcomponent::EventHeader {
                event_type: Some(EventType::CapabilityRequested.into()),
                moniker: Some(ExtendedMoniker::ComponentManager.to_string()),
                component_url: Some("file:///bin/component_manager".to_string()),
                timestamp: Some(zx::BootInstant::get()),
                ..Default::default()
            }),
            payload: Some(fcomponent::EventPayload::CapabilityRequested(
                fcomponent::CapabilityRequestedPayload {
                    name: Some(InspectSinkMarker::PROTOCOL_NAME.to_string()),
                    capability: Some(server.into_channel()),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let _ = sender.unbounded_send(event);
    }
}
