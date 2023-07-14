// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        events::{event::Event, registry::ComponentEventRoute, stream::EventStream},
        hooks::{EventPayload, EventType, HasEventType},
    },
    cm_rust::{ChildRef, EventScope},
    cm_types::Name,
    cm_util::io::clone_dir,
    fidl::endpoints::{Proxy, ServerEnd},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio,
    flyweights::FlyStr,
    fuchsia_zircon::{
        self as zx, sys::ZX_CHANNEL_MAX_MSG_BYTES, sys::ZX_CHANNEL_MAX_MSG_HANDLES, HandleBased,
    },
    futures::{lock::Mutex, StreamExt},
    measure_tape_for_events::Measurable,
    moniker::{ChildNameBase, ExtendedMoniker, Moniker, MonikerBase},
    std::sync::Arc,
    tracing::{error, warn},
};

// Number of bytes the header of a vector occupies in a fidl message.
// TODO(https://fxbug.dev/98653): This should be a constant in a FIDL library.
const FIDL_VECTOR_HEADER_BYTES: usize = 16;

// Number of bytes the header of a fidl message occupies.
// TODO(https://fxbug.dev/98653): This should be a constant in a FIDL library.
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

/// Determines if an event from a specified absolute moniker
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
    name: &str,
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
                name: FlyStr::new(event_part.name()),
                collection: event_part.collection().cloned(),
            };
            if child_ref != component.component {
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

async fn handle_get_next_request(
    event_stream: &mut EventStream,
    pending_event: &mut Option<fcomponent::Event>,
) -> Option<Vec<fcomponent::Event>> {
    // Handle buffered state
    // TODO(https://fxbug.dev/98653): Replace this
    // with a function to measure a Vec<fsys::Event>
    let mut bytes_used: usize = FIDL_HEADER_BYTES + FIDL_VECTOR_HEADER_BYTES;
    let mut handles_used: usize = 0;
    let mut events = vec![];

    /// Measures the size of an event, increments bytes used,
    /// and returns the event Vec if full.
    /// If there isn't enough space for even one event, logs an error
    /// and returns an empty Vec.
    macro_rules! handle_event {
        ($e: expr, $event_type: expr) => {
            let measure_tape = $e.measure();
            bytes_used += measure_tape.num_bytes;
            handles_used += measure_tape.num_handles;
            if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize
            || handles_used > ZX_CHANNEL_MAX_MSG_HANDLES as usize {
                if pending_event.is_some() {
                    unreachable!("Overflowed twice");
                }
                *pending_event = Some($e);
                if events.len() == 0 {
                    error!(
                        event_type = $event_type.as_str(),
                        "Event exceeded the maximum channel size, dropping event"
                    );
                }
                return Some(events);
            } else {
                events.push($e);
            }
        }
    }

    macro_rules! handle_and_filter_event {
        ($event: expr, $route: expr) => {
            if let Some(mut route) = $route {
                route.reverse();
                if !validate_and_filter_event(&mut $event.event.target_moniker, &route) {
                    continue;
                }
            }
            let event_type = $event.event.event_type().to_string();
            let event_fidl_object = match create_event_fidl_object($event).await {
                Ok(event_fidl_object) => event_fidl_object,
                Err(error) => {
                    warn!(?error, "Failed to create event object");
                    continue;
                }
            };
            handle_event!(event_fidl_object, event_type);
        };
    }

    // Read overflowed events from the buffer first
    if let Some(event) = pending_event.take() {
        let e_type = event
            .header
            .as_ref()
            .map(|header| format!("{:?}", header.event_type))
            .unwrap_or("unknown".to_string());
        handle_event!(event, e_type);
    }

    if events.is_empty() {
        // Block
        // If not for the macro this would be an if let
        // because the loop will only iterate once (therefore we block only 1 time)
        while let Some((mut event, route)) = event_stream.next().await {
            handle_and_filter_event!(event, route);
            break;
        }
    }
    loop {
        if let Some(Some((mut event, route))) = event_stream.next_or_none().await {
            handle_and_filter_event!(event, route);
        } else {
            break;
        }
    }
    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

/// Tries to handle the next request.
/// An error value indicates the caller should close the channel
async fn try_handle_get_next_request(
    event_stream: &mut EventStream,
    responder: fcomponent::EventStreamGetNextResponder,
    buffer: &mut Option<fcomponent::Event>,
) -> bool {
    let events = handle_get_next_request(event_stream, buffer).await;
    if let Some(events) = events {
        responder.send(events).is_ok()
    } else {
        unreachable!("Internal: The event_stream internal channel should never be closed.");
    }
}

/// Serves the event_stream protocol implemented for EventStreamRequestStream
/// This is needed because we get the request stream directly as a stream from FDIO
/// but as a ServerEnd from the hooks system.
pub async fn serve_event_stream_as_stream(
    mut event_stream: EventStream,
    mut stream: fcomponent::EventStreamRequestStream,
) {
    let mut buffer = None;
    while let Some(Ok(request)) = stream.next().await {
        match request {
            fcomponent::EventStreamRequest::GetNext { responder } => {
                if !try_handle_get_next_request(&mut event_stream, responder, &mut buffer).await {
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

/// Serves EventStream FIDL requests received over the provided stream.
pub async fn serve_event_stream(
    event_stream: EventStream,
    server_end: ServerEnd<fcomponent::EventStreamMarker>,
) {
    let stream = server_end.into_stream().unwrap();
    serve_event_stream_as_stream(event_stream, stream).await;
}

async fn create_event_result(
    event_result: &EventPayload,
) -> Result<fcomponent::EventPayload, fidl::Error> {
    match event_result {
        EventPayload::DirectoryReady { name, node, .. } => {
            Ok(create_directory_ready_payload(name.to_string(), node)?)
        }
        EventPayload::CapabilityRequested { name, capability, .. } => {
            Ok(create_capability_requested_payload(name.to_string(), capability.clone()).await)
        }
        EventPayload::CapabilityRouted { .. } => {
            // Capability routed events cannot be exposed externally. This should be unreachable.
            unreachable!("Capability routed can't be externally exposed");
        }
        EventPayload::Stopped { status } => {
            Ok(fcomponent::EventPayload::Stopped(fcomponent::StoppedPayload {
                status: Some(status.into_raw()),
                ..Default::default()
            }))
        }
        EventPayload::DebugStarted { runtime_dir, break_on_start } => {
            Ok(create_debug_started_payload(runtime_dir, break_on_start))
        }
        payload => Ok(match payload.event_type() {
            EventType::Discovered => {
                fcomponent::EventPayload::Discovered(fcomponent::DiscoveredPayload::default())
            }
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
        }),
    }
}

fn create_directory_ready_payload(
    name: String,
    node: &fio::NodeProxy,
) -> Result<fcomponent::EventPayload, fidl::Error> {
    let node = {
        let (node_clone, server_end) = fidl::endpoints::create_proxy()?;
        node.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server_end)?;
        let node_client_end = node_clone
            .into_channel()
            .expect("could not convert directory to channel")
            .into_zx_channel()
            .into();
        Some(node_client_end)
    };

    let payload =
        fcomponent::DirectoryReadyPayload { name: Some(name), node, ..Default::default() };
    Ok(fcomponent::EventPayload::DirectoryReady(payload))
}

async fn create_capability_requested_payload(
    name: String,
    capability: Arc<Mutex<Option<zx::Channel>>>,
) -> fcomponent::EventPayload {
    let capability = capability.lock().await.take();
    let payload = fcomponent::CapabilityRequestedPayload {
        name: Some(name),
        capability,
        ..Default::default()
    };
    fcomponent::EventPayload::CapabilityRequested(payload)
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

/// Creates the basic FIDL Event object
async fn create_event_fidl_object(event: Event) -> Result<fcomponent::Event, anyhow::Error> {
    let moniker_string = match (&event.event.target_moniker, &event.scope_moniker) {
        (moniker @ ExtendedMoniker::ComponentManager, _) => moniker.to_string(),
        (ExtendedMoniker::ComponentInstance(target), ExtendedMoniker::ComponentManager) => {
            Moniker::scope_down(&Moniker::root(), target)
                .expect("every component can be scoped down from the root")
                .to_string()
        }
        (ExtendedMoniker::ComponentInstance(target), ExtendedMoniker::ComponentInstance(scope)) => {
            Moniker::scope_down(scope, target)
                .expect("target must be a child of event scope")
                .to_string()
        }
    };
    let header = fcomponent::EventHeader {
        event_type: Some(event.event.event_type().try_into()?),
        moniker: Some(moniker_string),
        component_url: Some(event.event.component_url.clone()),
        timestamp: Some(event.event.timestamp.into_nanos()),
        ..Default::default()
    };
    let payload = create_event_result(&event.event.payload).await?;
    Ok(fcomponent::Event { header: Some(header), payload: Some(payload), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use crate::model::events::serve::validate_and_filter_event;
    use crate::model::events::serve::ComponentEventRoute;
    use cm_rust::ChildRef;
    use cm_rust::EventScope;
    use moniker::ChildName;
    use moniker::ExtendedMoniker;
    use moniker::Moniker;
    use moniker::MonikerBase;

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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "root".into(), collection: None },
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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "core".into(), collection: None },
                scope: Some(vec![EventScope::Collection("test_manager".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "test_manager".into(), collection: None },
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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "a".into(), collection: None },
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "b".into(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "b".into(), collection: None },
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "c".into(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "c".into(), collection: None },
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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "a".into(), collection: None },
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "b".into(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "b".into(), collection: None },
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "c".into(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "c".into(), collection: None },
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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "a".into(), collection: None },
                scope: Some(vec![EventScope::Collection("b".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "b".into(), collection: None },
                scope: Some(vec![EventScope::Collection("c".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "c".into(), collection: None },
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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "a".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "b".into(), collection: None },
                scope: Some(vec![EventScope::Collection("c".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "c".into(), collection: None },
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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "core".into(), collection: None },
                scope: Some(vec![EventScope::Collection("test_manager".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "test_manager".into(), collection: None },
                scope: Some(vec![EventScope::Collection("test_wrapper".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "test_wrapper".into(), collection: None },
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
            ComponentEventRoute {
                component: ChildRef { name: "<root>".into(), collection: None },
                scope: None,
            },
            ComponentEventRoute {
                component: ChildRef { name: "core".into(), collection: None },
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "test_manager".into(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "test_manager".into(), collection: None },
                scope: Some(vec![EventScope::Collection("tests".parse().unwrap())]),
            },
            ComponentEventRoute {
                component: ChildRef {
                    name: "auto-3fc01a79864c741".into(),
                    collection: Some("tests".parse().unwrap()),
                },
                scope: Some(vec![EventScope::Child(ChildRef {
                    name: "test_wrapper".into(),
                    collection: None,
                })]),
            },
            ComponentEventRoute {
                component: ChildRef { name: "test_wrapper".into(), collection: None },
                scope: Some(vec![
                    EventScope::Collection("test".parse().unwrap()),
                    EventScope::Child(ChildRef { name: "enclosing_env".into(), collection: None }),
                    EventScope::Child(ChildRef {
                        name: "hermetic_resolver".into(),
                        collection: None,
                    }),
                ]),
            },
            ComponentEventRoute {
                component: ChildRef {
                    name: "test_root".into(),
                    collection: Some("test".parse().unwrap()),
                },
                scope: None,
            },
        ];
        assert_eq!(super::validate_and_filter_event(&mut event, &route), false);
    }
}
