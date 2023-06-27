// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::events::router::{Dispatcher, EventProducer};
use anyhow::Error;
use fcomponent::EventStreamProxy;
use fidl_fuchsia_component as fcomponent;
use fuchsia_component::client::connect_to_protocol_at_path;
use tracing::warn;

pub struct EventSource {
    dispatcher: Dispatcher,
    event_stream: EventStreamProxy,
}

impl EventSource {
    pub async fn new(event_stream_path: &str) -> Result<Self, Error> {
        let event_stream =
            connect_to_protocol_at_path::<fcomponent::EventStreamMarker>(event_stream_path)?;
        let _ = event_stream.wait_for_ready().await;
        Ok(Self { event_stream, dispatcher: Dispatcher::default() })
    }

    #[cfg(test)]
    async fn new_for_test(event_stream: EventStreamProxy) -> Self {
        // Connect to /events/event_stream which contains our newer FIDL protocol
        Self { event_stream, dispatcher: Dispatcher::default() }
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
        let mut source = EventSource::new_for_test(stream_server).await;
        source.set_dispatcher(dispatcher);
        let _task = fasync::Task::spawn(async move { source.spawn().await });

        // Send a `DirectoryReady` event for diagnostics.
        let (node, _) = fidl::endpoints::create_request_stream::<fio::NodeMarker>().unwrap();
        sender
            .unbounded_send(fcomponent::Event {
                header: Some(fcomponent::EventHeader {
                    event_type: Some(fcomponent::EventType::DirectoryReady),
                    moniker: Some("./foo/bar".to_string()),
                    component_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/bar.cm".to_string()),
                    timestamp: Some(zx::Time::get_monotonic().into_nanos()),
                    ..Default::default()
                }),
                payload: Some(fcomponent::EventPayload::DirectoryReady(
                    fcomponent::DirectoryReadyPayload {
                        name: Some("diagnostics".to_string()),
                        node: Some(node),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })
            .expect("send diagnostics ready event ok");

        // Send a `LogSinkRequested` event.
        sender
            .unbounded_send(fcomponent::Event {
                header: Some(fcomponent::EventHeader {
                    event_type: Some(fcomponent::EventType::CapabilityRequested),
                    moniker: Some("./foo/bar".to_string()),
                    component_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/bar.cm".to_string()),
                    timestamp: Some(zx::Time::get_monotonic().into_nanos()),
                    ..Default::default()
                }),
                payload: Some(fcomponent::EventPayload::CapabilityRequested(
                    fcomponent::CapabilityRequestedPayload {
                        name: Some("fuchsia.logger.LogSink".to_string()),
                        capability: Some(zx::Channel::create().0),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })
            .expect("send diagnostics ready event ok");

        let expected_component_id = ComponentIdentifier::parse_from_moniker("./foo/bar").unwrap();
        let expected_identity = ComponentIdentity::from_identifier_and_url(
            expected_component_id,
            "fuchsia-pkg://fuchsia.com/foo#meta/bar.cm",
        );

        // Assert the third received event was a DirectoryReady event for diagnostics.
        let event = event_stream.next().await.unwrap();
        match event.payload {
            EventPayload::DiagnosticsReady(DiagnosticsReadyPayload { component, directory: _ }) => {
                assert_eq!(*component, expected_identity)
            }
            other => panic!("unexpected event payload: {other:?}"),
        }

        // Assert the last received event was a LogSinkRequested event.
        let event = event_stream.next().await.unwrap();
        match event.payload {
            EventPayload::LogSinkRequested(LogSinkRequestedPayload { component, .. }) => {
                assert_eq!(*component, expected_identity)
            }
            other => panic!("unexpected event payload: {other:?}"),
        }
    }

    fn spawn_fake_event_stream(
    ) -> (fcomponent::EventStreamProxy, fasync::Task<()>, UnboundedSender<fcomponent::Event>) {
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<fcomponent::Event>();
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fcomponent::EventStreamMarker>().unwrap();
        let task = fasync::Task::spawn(async move {
            let mut request_stream = server_end.into_stream().unwrap();
            loop {
                if let Some(Ok(request)) = request_stream.next().await {
                    match request {
                        fcomponent::EventStreamRequest::GetNext { responder } => {
                            if let Some(event) = receiver.next().await {
                                responder.send(vec![event]).unwrap();
                            } else {
                                break;
                            }
                        }
                        fcomponent::EventStreamRequest::WaitForReady { responder } => {
                            responder.send().unwrap();
                        }
                    }
                }
            }
        });
        (proxy, task, sender)
    }
}
