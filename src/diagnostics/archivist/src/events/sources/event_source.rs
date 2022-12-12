// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::events::{
    error::EventError,
    router::{Dispatcher, EventProducer},
};
use anyhow::Error;
use fidl_fuchsia_sys2 as fsys;
use fsys::EventStream2Proxy;
use fuchsia_component::client::connect_to_protocol_at_path;
use tracing::warn;

pub struct EventSource {
    dispatcher: Dispatcher,
    event_stream: EventStream2Proxy,
}

impl EventSource {
    pub async fn new(event_stream_path: &str) -> Result<Self, Error> {
        let event_stream =
            connect_to_protocol_at_path::<fsys::EventStream2Marker>(event_stream_path)?;
        let _ = event_stream.wait_for_ready().await;
        Ok(Self { event_stream, dispatcher: Dispatcher::default() })
    }

    #[cfg(test)]
    async fn new_for_test(event_stream: EventStream2Proxy) -> Result<Self, EventError> {
        // Connect to /events/event_stream which contains our newer FIDL protocol
        Ok(Self { event_stream, dispatcher: Dispatcher::default() })
    }

    pub async fn spawn(mut self) -> Result<(), Error> {
        while let Ok(events) = self.event_stream.get_next().await {
            for event in events {
                match event.try_into() {
                    Ok(event) => {
                        if let Err(err) = self.dispatcher.emit(event) {
                            if err.is_disconnected() {
                                break;
                            }
                        }
                    }
                    Err(EventError::UnknownResult(fsys::EventResult::Error(
                        fsys::EventError {
                            error_payload:
                                Some(fsys::EventErrorPayload::DirectoryReady(
                                    fsys::DirectoryReadyError { .. },
                                )),
                            ..
                        },
                    ))) => {
                        // The error was intended for cases when a component
                        // declared it exposes inspect but doesn't actually serve it. Turns out this
                        // is not very common and leads to spam in tests that end up having to
                        // include the inspect shard transitevely and correctly do not expose
                        // Inspect. Instead of logging a spammy and confusing error, ignore it, with
                        // RFC168 this error will be obsolete also.
                    }
                    Err(err) => {
                        warn!(?err, "Failed to interpret event");
                    }
                }
            }
        }
        Ok(())
    }
}

impl EventProducer for EventSource {
    fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
        self.dispatcher = dispatcher;
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::{events::types::*, identity::ComponentIdentity};
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use futures::{channel::mpsc::UnboundedSender, StreamExt};
    use std::collections::BTreeSet;

    #[fuchsia::test]
    async fn event_stream() {
        let events = BTreeSet::from([EventType::DiagnosticsReady, EventType::LogSinkRequested]);
        let (mut event_stream, dispatcher) = Dispatcher::new_for_test(events);
        let (stream_server, _server_task, sender) = spawn_fake_event_stream();
        let mut source = EventSource::new_for_test(stream_server).await.unwrap();
        source.set_dispatcher(dispatcher);
        let _task = fasync::Task::spawn(async move { source.spawn().await });

        // Send a `DirectoryReady` event for diagnostics.
        let (node, _) = fidl::endpoints::create_request_stream::<fio::NodeMarker>().unwrap();
        sender
            .unbounded_send(fsys::Event {
                header: Some(fsys::EventHeader {
                    event_type: Some(fsys::EventType::DirectoryReady),
                    moniker: Some("./foo/bar".to_string()),
                    component_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/bar.cmx".to_string()),
                    timestamp: Some(zx::Time::get_monotonic().into_nanos()),
                    ..fsys::EventHeader::EMPTY
                }),
                event_result: Some(fsys::EventResult::Payload(fsys::EventPayload::DirectoryReady(
                    fsys::DirectoryReadyPayload {
                        name: Some("diagnostics".to_string()),
                        node: Some(node),
                        ..fsys::DirectoryReadyPayload::EMPTY
                    },
                ))),
                ..fsys::Event::EMPTY
            })
            .expect("send diagnostics ready event ok");

        // Send a `LogSinkRequested` event.
        sender
            .unbounded_send(fsys::Event {
                header: Some(fsys::EventHeader {
                    event_type: Some(fsys::EventType::CapabilityRequested),
                    moniker: Some("./foo/bar".to_string()),
                    component_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/bar.cmx".to_string()),
                    timestamp: Some(zx::Time::get_monotonic().into_nanos()),
                    ..fsys::EventHeader::EMPTY
                }),
                event_result: Some(fsys::EventResult::Payload(
                    fsys::EventPayload::CapabilityRequested(fsys::CapabilityRequestedPayload {
                        name: Some("fuchsia.logger.LogSink".to_string()),
                        capability: Some(zx::Channel::create().unwrap().0),
                        ..fsys::CapabilityRequestedPayload::EMPTY
                    }),
                )),
                ..fsys::Event::EMPTY
            })
            .expect("send diagnostics ready event ok");

        let expected_component_id = ComponentIdentifier::parse_from_moniker("./foo/bar").unwrap();
        let expected_identity = ComponentIdentity::from_identifier_and_url(
            expected_component_id,
            "fuchsia-pkg://fuchsia.com/foo#meta/bar.cmx",
        );

        // Assert the third received event was a DirectoryReady event for diagnostics.
        let event = event_stream.next().await.unwrap();
        match event.payload {
            EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                component,
                directory: Some(_),
            }) => assert_eq!(component, expected_identity),
            other => panic!("unexpected event payload: {other:?}"),
        }

        // Assert the last received event was a LogSinkRequested event.
        let event = event_stream.next().await.unwrap();
        match event.payload {
            EventPayload::LogSinkRequested(LogSinkRequestedPayload { component, .. }) => {
                assert_eq!(component, expected_identity)
            }
            other => panic!("unexpected event payload: {other:?}"),
        }
    }

    fn spawn_fake_event_stream(
    ) -> (fsys::EventStream2Proxy, fasync::Task<()>, UnboundedSender<fsys::Event>) {
        let (sender, mut receiver) = futures::channel::mpsc::unbounded();
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fsys::EventStream2Marker>().unwrap();
        let task = fasync::Task::spawn(async move {
            let mut request_stream = server_end.into_stream().unwrap();
            loop {
                if let Some(Ok(request)) = request_stream.next().await {
                    match request {
                        fsys::EventStream2Request::GetNext { responder } => {
                            if let Some(event) = receiver.next().await {
                                responder.send(&mut vec![event].into_iter()).unwrap();
                            } else {
                                break;
                            }
                        }
                        fsys::EventStream2Request::WaitForReady { responder } => {
                            responder.send().unwrap();
                        }
                    }
                }
            }
        });
        (proxy, task, sender)
    }
}
