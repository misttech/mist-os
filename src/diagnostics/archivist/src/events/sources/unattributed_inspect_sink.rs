// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    events::{
        router::{Dispatcher, EventProducer},
        types::{Event, EventPayload, InspectSinkRequestedPayload},
    },
    identity::ComponentIdentity,
};
use fidl_fuchsia_inspect as finspect;
use fuchsia_zircon as zx;
use std::sync::Arc;

#[derive(Default)]
pub struct UnattributedInspectSinkSource {
    dispatcher: Dispatcher,
}

impl UnattributedInspectSinkSource {
    pub fn new_connection(&mut self, request_stream: finspect::InspectSinkRequestStream) {
        self.dispatcher
            .emit(Event {
                timestamp: zx::Time::get_monotonic(),
                payload: EventPayload::InspectSinkRequested(InspectSinkRequestedPayload {
                    component: Arc::new(ComponentIdentity::unknown()),
                    request_stream,
                }),
            })
            .ok();
    }
}

impl EventProducer for UnattributedInspectSinkSource {
    fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
        self.dispatcher = dispatcher;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::types::*;
    use fidl_fuchsia_inspect::InspectSinkMarker;
    use flyweights::FlyStr;
    use fuchsia_async as fasync;
    use futures::StreamExt;
    use std::collections::BTreeSet;

    #[fuchsia::test]
    async fn events_have_unknown_identity() {
        let events = BTreeSet::from([EventType::InspectSinkRequested]);
        let (mut event_stream, dispatcher) = Dispatcher::new_for_test(events);
        let mut source = UnattributedInspectSinkSource::default();
        source.set_dispatcher(dispatcher);
        let (_, inspect_sink_stream) =
            fidl::endpoints::create_proxy_and_stream::<InspectSinkMarker>().unwrap();

        let _task = fasync::Task::spawn(async move {
            source.new_connection(inspect_sink_stream);
        });

        let event = event_stream.next().await.expect("received event");
        let expected_identity = ComponentIdentity {
            url: FlyStr::new("fuchsia-pkg://UNKNOWN"),
            instance_id: Some("0".to_string().into_boxed_str()),
            relative_moniker: vec!["UNKNOWN"].into(),
        };
        match event.payload {
            EventPayload::InspectSinkRequested(InspectSinkRequestedPayload {
                component,
                request_stream: _,
            }) => {
                assert_eq!(*component, expected_identity);
            }
            payload => unreachable!("{:?} never gets here", payload),
        }
    }
}
