// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::events::event::Event;
use crate::model::events::registry::ComponentEventRoute;
use crate::model::events::stream::EventStream;
use cm_rust::{ChildRef, EventScope};
use cm_types::{LongName, Name};
use cm_util::io::clone_dir;
use fidl::endpoints::Proxy;
use futures::stream::Peekable;
use futures::{stream, Stream, StreamExt};
use hooks::{CapabilityReceiver, EventPayload, EventType, HasEventType};
use measure_tape_for_events::Measurable;
use moniker::{ExtendedMoniker, Moniker};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tracing::{error, warn};
use zx::sys::{ZX_CHANNEL_MAX_MSG_BYTES, ZX_CHANNEL_MAX_MSG_HANDLES};
use zx::{self as zx, HandleBased};
use {fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio};

// Number of bytes the header of a vector occupies in a fidl message.
// TODO(https://fxbug.dev/42181010): This should be a constant in a FIDL library.
const FIDL_VECTOR_HEADER_BYTES: usize = 16;

// Number of bytes the header of a fidl message occupies.
// TODO(https://fxbug.dev/42181010): This should be a constant in a FIDL library.
const FIDL_HEADER_BYTES: usize = 16;

/// Computes the scope length, which is the number of segments
/// up until the first scope. This is used to re-map the moniker
/// of an event before passing it off to the component that is
/// reading the event stream.
fn get_scope_length(route: &[ComponentEventRoute]) -> usize {
    // Length is the length of the scope (0 if no scope, 1 for the
    // component after <root>)
    let mut length = 0;
    // Index is the current index in route +1 (since we want to exclude
    // a component's parent from the moniker).
    let mut index = 1;
    for component in route {
        // Set length to index, this is the most recent
        // scope found in the route.
        if component.scope.is_some() {
            length = index;
        }
        index += 1;
    }
    length
}

/// Determines if an event from a specified moniker
/// may be routed to a given scope.
fn is_moniker_valid_within_scope(moniker: &ExtendedMoniker, route: &[ComponentEventRoute]) -> bool {
    match moniker {
        ExtendedMoniker::ComponentInstance(instance) => {
            validate_component_instance(instance, route.iter())
        }
        ExtendedMoniker::ComponentManager => false,
    }
}

// Returns true if the filter contains a specific Ref
fn event_filter_contains_ref(
    filter: &Option<Vec<EventScope>>,
    name: &LongName,
    collection: Option<&Name>,
) -> bool {
    filter.as_ref().map_or(true, |value| {
        value
            .iter()
            .map(|value| match value {
                EventScope::Child(ChildRef { collection: child_coll, name: child_name }) => {
                    collection == child_coll.as_ref() && child_name == name
                }
                EventScope::Collection(collection_name) => Some(collection_name) == collection,
            })
            .any(|val| val)
    })
}

/// Checks the specified instance against the specified route,
/// Returns Some(true) if the route is explicitly allowed,
/// Some(false) if the route is explicitly rejected,
/// or None if allowed because no route explicitly rejected it.
fn validate_component_instance(
    instance: &Moniker,
    mut iter: std::slice::Iter<'_, ComponentEventRoute>,
) -> bool {
    let path = instance.path();
    let mut event_iter = path.iter();
    // Component manager is an unnamed component which exists in route
    // but not in the moniker (because it's not a named component).
    // We take the first item from the iterator and get its scope
    // to determine initial scoping and ensure that route
    // and moniker are properly aligned to each other.
    let mut active_scope = iter.next().unwrap().scope.clone();
    for component in iter {
        if let Some(event_part) = event_iter.next() {
            if !event_filter_contains_ref(&active_scope, event_part.name(), event_part.collection())
            {
                // Reject due to scope mismatch
                return false;
            }
            let child_ref = ChildRef {
                name: event_part.name().clone(),
                collection: event_part.collection().cloned(),
            };
            if Some(child_ref) != component.component {
                // Reject due to path mismatch
                return false;
            }
            active_scope = component.scope.clone();
        } else {
            // Reject due to no more event parts
            return false;
        }
    }
    match (active_scope, event_iter.next()) {
        (Some(scopes), Some(event)) => {
            if !event_filter_contains_ref(&Some(scopes), event.name(), event.collection.as_ref()) {
                return false;
            }
        }
        (Some(_), None) => {
            // Reject due to no more event parts
            return false;
        }
        _ => {}
    }
    // Reached end of scope.
    true
}

/// Filters and downscopes an event by a route.
/// Returns true if the event is allowed given the specified route,
/// false otherwise.
fn filter_event(moniker: &mut ExtendedMoniker, route: &[ComponentEventRoute]) -> bool {
    let scope_length = get_scope_length(route);
    if !is_moniker_valid_within_scope(&moniker, &route[0..scope_length]) {
        return false;
    }
    // For scoped events, the apparent root (visible to the component)
    // starts at the last scope declaration which applies to this particular event.
    // Since this creates a relative rather than absolute moniker, where the base may be different
    // for each event, ambiguous component monikers are possible here.
    if let ExtendedMoniker::ComponentInstance(instance) = moniker {
        let mut path = instance.path().clone();
        path.reverse();
        for _ in 0..scope_length {
            path.pop();
        }
        path.reverse();
        *instance = Moniker::new(path);
    }
    true
}

/// Validates and filters an event, returning true if the route is allowed,
/// false otherwise. The scope of the event is filtered to the allowed route.
pub fn validate_and_filter_event(
    moniker: &mut ExtendedMoniker,
    route: &[ComponentEventRoute],
) -> bool {
    let needs_filter = route.iter().any(|component| component.scope.is_some());
    if needs_filter {
        filter_event(moniker, route)
    } else {
        true
    }
}

/// [`EventFiller`] helps build a vector of events up to the Zircon
/// channel message size limit.
///
/// TODO(https://fxbug.dev/42181010): This can be simplified given better
/// FIDL large messages support.
struct EventFiller {
    bytes_used: usize,
    handles_used: usize,
    events: Vec<fcomponent::Event>,
}

impl EventFiller {
    fn new() -> Self {
        EventFiller {
            bytes_used: FIDL_HEADER_BYTES + FIDL_VECTOR_HEADER_BYTES,
            handles_used: 0,
            events: vec![],
        }
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Measures the size of an event, increments bytes used, and returns the
    /// event Vec as an error if full.
    ///
    /// If there isn't enough space for even one event, logs an error and
    /// returns an empty Vec.
    fn add_event(
        mut self,
        event: fcomponent::Event,
        pending_event: &mut Option<fcomponent::Event>,
    ) -> Result<Self, Vec<fcomponent::Event>> {
        let event_type = event
            .header
            .as_ref()
            .map(|header| format!("{:?}", header.event_type))
            .unwrap_or_else(|| "unknown".to_string());
        let measure_tape = event.measure();
        self.bytes_used += measure_tape.num_bytes;
        self.handles_used += measure_tape.num_handles;
        if self.bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize
            || self.handles_used > ZX_CHANNEL_MAX_MSG_HANDLES as usize
        {
            if pending_event.is_some() {
                unreachable!("Overflowed twice");
            }
            *pending_event = Some(event);
            if self.events.len() == 0 {
                error!(
                    event_type = event_type.as_str(),
                    "Event exceeded the maximum channel size, dropping event"
                );
            }
            return Err(self.events);
        } else {
            self.events.push(event);
            return Ok(self);
        }
    }
}

impl From<EventFiller> for Vec<fcomponent::Event> {
    fn from(value: EventFiller) -> Self {
        value.events
    }
}

/// This function returns events via both `Ok` and `Err` such that we
/// may use the question mark operator to return early.
async fn do_handle_get_next_request(
    mut event_stream: Pin<&mut Peekable<impl Stream<Item = fcomponent::Event>>>,
    pending_event: &mut Option<fcomponent::Event>,
) -> Result<Vec<fcomponent::Event>, Vec<fcomponent::Event>> {
    let mut events = EventFiller::new();

    // Read overflowed events from the buffer first.
    if let Some(event) = pending_event.take() {
        events = events.add_event(event, pending_event)?;
    }

    if events.is_empty() {
        // Block one time, to ensure we get at least one event to return to the client.
        if let Some(event) = event_stream.next().await {
            events = events.add_event(event, pending_event)?;
        }
    }
    loop {
        // Try to add any immediately available event, stopping if there aren't any.
        let Poll::Ready(_) = futures::poll!(event_stream.as_mut().peek()) else {
            break;
        };
        let Some(event) = event_stream.next().await else {
            break;
        };
        events = events.add_event(event, pending_event)?;
    }
    if events.is_empty() {
        unreachable!("Internal: The event_stream internal channel should never be closed.");
    }
    Ok(events.into())
}

/// Obtains the next batch of events, waiting for at least one. Returns when there are no
/// more events available at the moment, or when the events are going to exceed the
/// maximum size that can be sent in a channel message.
async fn handle_get_next_request(
    event_stream: Pin<&mut Peekable<impl Stream<Item = fcomponent::Event>>>,
    pending_event: &mut Option<fcomponent::Event>,
) -> Vec<fcomponent::Event> {
    match do_handle_get_next_request(event_stream, pending_event).await {
        Ok(v) => v,
        Err(v) => v,
    }
}

/// Serves the event_stream protocol implemented for EventStreamRequestStream
/// This is needed because we get the request stream directly as a stream from FDIO
/// but as a ServerEnd from the hooks system.
pub async fn serve_event_stream(
    event_stream: EventStream,
    mut stream: fcomponent::EventStreamRequestStream,
) {
    async fn filter_event(input: (Event, Option<Vec<ComponentEventRoute>>)) -> Option<Event> {
        let (mut event, route) = input;
        if let Some(mut route) = route {
            route.reverse();
            if !validate_and_filter_event(&mut event.event.target_moniker, &route) {
                return None;
            }
        }
        Some(event)
    }
    async fn filter_log_errors(
        result: Result<fcomponent::Event, anyhow::Error>,
    ) -> Option<fcomponent::Event> {
        match result {
            Ok(event_fidl_object) => Some(event_fidl_object),
            Err(error) => {
                warn!(?error, "Failed to create event object");
                None
            }
        }
    }
    let event_stream = event_stream
        .filter_map(filter_event)
        .map(create_event_fidl_objects)
        .fuse()
        .flatten_unordered(None)
        .filter_map(filter_log_errors);
    let event_stream = event_stream.boxed().peekable();
    let mut event_stream = std::pin::pin!(event_stream);

    let mut buffer = None;
    while let Some(Ok(request)) = stream.next().await {
        match request {
            fcomponent::EventStreamRequest::GetNext { responder } => {
                let events = handle_get_next_request(event_stream.as_mut(), &mut buffer).await;
                if !responder.send(events).is_ok() {
                    // Close the channel if an error occurs while handling the request.
                    return;
                }
            }
            fcomponent::EventStreamRequest::WaitForReady { responder } => {
                let _ = responder.send();
            }
        }
    }
}

type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send + 'static>>;

fn stream_once<T: Send + 'static>(value: T) -> BoxStream<T> {
    stream::once(std::future::ready(value)).boxed()
}

fn create_event_payloads(
    event_payload: &EventPayload,
) -> BoxStream<Result<fcomponent::EventPayload, fidl::Error>> {
    match event_payload {
        EventPayload::CapabilityRequested { name, receiver, .. } => {
            create_capability_requested_payload(name.to_string(), receiver.clone())
        }
        EventPayload::Stopped { status, exit_code, .. } => {
            stream_once(Ok(fcomponent::EventPayload::Stopped(fcomponent::StoppedPayload {
                status: Some(status.into_raw()),
                exit_code: *exit_code,
                ..Default::default()
            })))
        }
        EventPayload::DebugStarted { runtime_dir, break_on_start } => {
            stream_once(Ok(create_debug_started_payload(runtime_dir, break_on_start)))
        }
        payload => stream_once(Ok(match payload.event_type() {
            EventType::Destroyed => {
                fcomponent::EventPayload::Destroyed(fcomponent::DestroyedPayload::default())
            }
            EventType::Resolved => {
                fcomponent::EventPayload::Resolved(fcomponent::ResolvedPayload::default())
            }
            EventType::Unresolved => {
                fcomponent::EventPayload::Unresolved(fcomponent::UnresolvedPayload::default())
            }
            EventType::Started => {
                fcomponent::EventPayload::Started(fcomponent::StartedPayload::default())
            }
            _ => unreachable!("Unsupported event type"),
        })),
    }
}

fn create_capability_requested_payload(
    name: String,
    receiver: CapabilityReceiver,
) -> BoxStream<Result<fcomponent::EventPayload, fidl::Error>> {
    match receiver.take() {
        // If this component has the opportunity to intercept capability requests,
        // emit a CapabilityRequested event for every request it receives.
        Some(receiver) => stream::unfold(receiver, move |receiver| {
            let name = name.clone();
            async move {
                let Some(message) = receiver.receive().await else {
                    return None;
                };
                let payload = fcomponent::CapabilityRequestedPayload {
                    name: Some(name),
                    capability: Some(message.channel),
                    ..Default::default()
                };
                Some((Ok(fcomponent::EventPayload::CapabilityRequested(payload)), receiver))
            }
        })
        .boxed(),
        // If someone else took away the opportunity to intercept capability requests,
        // emit a CapabilityRequested event with an absent capability.
        None => stream::once(std::future::ready(Ok(
            fcomponent::EventPayload::CapabilityRequested(fcomponent::CapabilityRequestedPayload {
                name: Some(name),
                capability: None,
                ..Default::default()
            }),
        )))
        .boxed(),
    }
}

fn create_debug_started_payload(
    runtime_dir: &Option<fio::DirectoryProxy>,
    break_on_start: &Arc<zx::EventPair>,
) -> fcomponent::EventPayload {
    fcomponent::EventPayload::DebugStarted(fcomponent::DebugStartedPayload {
        runtime_dir: clone_dir(runtime_dir.as_ref()).map(|dir| {
            dir.into_channel()
                .expect("could not convert directory to channel")
                .into_zx_channel()
                .into()
        }),
        break_on_start: break_on_start.duplicate_handle(zx::Rights::SAME_RIGHTS).ok(),
        ..Default::default()
    })
}

/// Creates a stream of FIDL Event objects from an Event.
fn create_event_fidl_objects(event: Event) -> BoxStream<Result<fcomponent::Event, anyhow::Error>> {
    let moniker_string = match (&event.event.target_moniker, &event.scope_moniker) {
        (moniker @ ExtendedMoniker::ComponentManager, _) => moniker.to_string(),
        (ExtendedMoniker::ComponentInstance(target), ExtendedMoniker::ComponentManager) => {
            target.to_string()
        }
        (ExtendedMoniker::ComponentInstance(target), ExtendedMoniker::ComponentInstance(scope)) => {
            target.strip_prefix(scope).expect("target must be a child of event scope").to_string()
        }
    };
    let event_type = match event.event.event_type().try_into() {
        Ok(event_type) => event_type,
        Err(error) => return stream::once(std::future::ready(Err(error))).boxed(),
    };
    let header = fcomponent::EventHeader {
        event_type: Some(event_type),
        moniker: Some(moniker_string),
        component_url: Some(event.event.component_url.to_string()),
        timestamp: Some(event.event.timestamp),
        ..Default::default()
    };
    let payload_stream = create_event_payloads(&event.event.payload);
    payload_stream
        .map(move |payload| {
            payload
                .and_then(|payload| {
                    Ok(fcomponent::Event {
                        header: Some(header.clone()),
                        payload: Some(payload),
                        ..Default::default()
                    })
                })
                .map_err(Into::into)
        })
        .boxed()
}

#[cfg(test)]
mod tests {
    use crate::model::events::serve::{validate_and_filter_event, ComponentEventRoute};
    use cm_rust::{ChildRef, EventScope};
    use moniker::{ChildName, ExtendedMoniker, Moniker};

    // Route: /root(coll)
    // Event: /root
    // Output: (rejected)
    #[test]
    fn test_validate_and_filter_event_at_root() {
        let mut moniker =
            ExtendedMoniker::ComponentInstance(Moniker::new(vec![ChildName::try_new(
                "root",
                Some("coll"),
            )
            .unwrap()]));
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "root".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Collection("coll".parse().unwrap())]),
            },
        ];
        assert!(!validate_and_filter_event(&mut moniker, &route));
    }

    // Route: /<root>/core(test_manager)/test_manager
    // Event: /
    // Output: (rejected)
    #[test]
    fn test_validate_and_filter_event_empty_moniker() {
        let mut event = ExtendedMoniker::ComponentInstance(Moniker::root());
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "core".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Collection("test_manager".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: Some(ChildRef {
                    name: "test_manager".parse().unwrap(),
                    collection: None,
                }),
                scope: None,
            },
        ];
        assert_eq!(validate_and_filter_event(&mut event, &route), false);
    }

    // Route: a(b)/b(c)/c
    // Event: a/b/c
    // Output: /
    #[test]
    fn test_validate_and_filter_event_moniker_root() {
        let mut event = ExtendedMoniker::ComponentInstance(Moniker::new(vec![
            ChildName::try_new("a", None).unwrap(),
            ChildName::try_new("b", None).unwrap(),
            ChildName::try_new("c", None).unwrap(),
        ]));
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "a".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "b".parse().unwrap(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "b".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "c".parse().unwrap(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "c".parse().unwrap(), collection: None }),
                scope: None,
            },
        ];
        assert!(super::validate_and_filter_event(&mut event, &route));
        assert_eq!(event, ExtendedMoniker::ComponentInstance(Moniker::root()));
    }

    // Route: a(b)/b(c)/c
    // Event: a/b/c/d
    // Output: d
    #[test]
    fn test_validate_and_filter_event_moniker_children_scoped() {
        let mut event = ExtendedMoniker::ComponentInstance(Moniker::new(vec![
            ChildName::try_new("a", None).unwrap(),
            ChildName::try_new("b", None).unwrap(),
            ChildName::try_new("c", None).unwrap(),
            ChildName::try_new("d", None).unwrap(),
        ]));
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "a".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "b".parse().unwrap(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "b".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "c".parse().unwrap(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "c".parse().unwrap(), collection: None }),
                scope: None,
            },
        ];
        assert!(super::validate_and_filter_event(&mut event, &route));
        assert_eq!(
            event,
            ExtendedMoniker::ComponentInstance(Moniker::new(vec![
                ChildName::try_new("d", None).unwrap()
            ]))
        );
    }

    // Route: a(b)/b(c)/c
    // Event: a
    // Output: (rejected)
    #[test]
    fn test_validate_and_filter_event_moniker_above_root_rejected() {
        let mut event =
            ExtendedMoniker::ComponentInstance(Moniker::new(vec![
                ChildName::try_new("a", None).unwrap()
            ]));
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "a".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Collection("b".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "b".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Collection("c".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "c".parse().unwrap(), collection: None }),
                scope: None,
            },
        ];
        assert!(!super::validate_and_filter_event(&mut event, &route));
        assert_eq!(
            event,
            ExtendedMoniker::ComponentInstance(Moniker::new(vec![
                ChildName::try_new("a", None).unwrap()
            ]))
        );
    }

    // Route: a/b(c)/c
    // Event: f/i
    // Output: (rejected)
    #[test]
    fn test_validate_and_filter_event_moniker_ambiguous() {
        let mut event = ExtendedMoniker::ComponentInstance(Moniker::new(vec![
            ChildName::try_new("f", None).unwrap(),
            ChildName::try_new("i", None).unwrap(),
        ]));
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "a".parse().unwrap(), collection: None }),
                scope: None,
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "b".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Collection("c".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: Some(ChildRef { name: "c".parse().unwrap(), collection: None }),
                scope: None,
            },
        ];
        assert!(!super::validate_and_filter_event(&mut event, &route));
    }

    // Route: /core(test_manager)/test_manager/test-id(test_wrapper)/test_wrapper(test_root)
    // Event: /core/feedback
    // Output: (rejected)
    #[test]
    fn test_validate_and_filter_event_moniker_root_rejected() {
        let mut event = ExtendedMoniker::ComponentInstance(Moniker::new(vec![
            ChildName::try_new("core", None).unwrap(),
            ChildName::try_new("feedback", None).unwrap(),
        ]));
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "core".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Collection("test_manager".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: Some(ChildRef {
                    name: "test_manager".parse().unwrap(),
                    collection: None,
                }),
                scope: Some(vec![EventScope::Collection("test_wrapper".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: Some(ChildRef {
                    name: "test_wrapper".parse().unwrap(),
                    collection: None,
                }),
                scope: Some(vec![EventScope::Collection("test_root".parse().unwrap())]),
            },
        ];
        assert_eq!(super::validate_and_filter_event(&mut event, &route), false);
    }

    // Route: /<root>/core(test_manager)/test_manager(col(tests))/auto-3fc01a79864c741:tests(test_wrapper)/test_wrapper(col(test),enclosing_env,hermetic_resolver)/test:test_root/archivist
    // Event: /core/test_manager/tests:auto-3fc01a79864c741/test_wrapper/archivist
    // Output: (rejected)
    #[test]
    fn test_validate_child_under_scoped_collection_is_root() {
        let mut event = ExtendedMoniker::ComponentInstance(Moniker::new(vec![
            ChildName::try_new("core", None).unwrap(),
            ChildName::try_new("test_manager", None).unwrap(),
            ChildName::try_new("auto-3fc01a79864c741", Some("tests")).unwrap(),
            ChildName::try_new("test_wrapper", None).unwrap(),
            ChildName::try_new("archivist", None).unwrap(),
        ]));
        let route = vec![
            ComponentEventRoute { component: None, scope: None },
            ComponentEventRoute {
                component: Some(ChildRef { name: "core".parse().unwrap(), collection: None }),
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "test_manager".parse().unwrap(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: Some(ChildRef {
                    name: "test_manager".parse().unwrap(),
                    collection: None,
                }),
                scope: Some(vec![EventScope::Collection("tests".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: Some(ChildRef {
                    name: "auto-3fc01a79864c741".parse().unwrap(),
                    collection: Some("tests".parse().unwrap()),
                }),
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "test_wrapper".parse().unwrap(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: Some(ChildRef {
                    name: "test_wrapper".parse().unwrap(),
                    collection: None,
                }),
                scope: Some(vec![
                    EventScope::Collection("test".parse().unwrap()),
                    EventScope::Child(ChildRef {
                        name: "enclosing_env".parse().unwrap(),
                        collection: None,
                    }),
                    EventScope::Child(ChildRef {
                        name: "hermetic_resolver".parse().unwrap(),
                        collection: None,
                    }),
                ]),
            },
            ComponentEventRoute {
                component: Some(ChildRef {
                    name: "test_root".parse().unwrap(),
                    collection: Some("test".parse().unwrap()),
                }),
                scope: None,
            },
        ];
        assert_eq!(super::validate_and_filter_event(&mut event, &route), false);
    }
}
