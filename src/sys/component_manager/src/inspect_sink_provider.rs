// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::{
    events::synthesizer::{EventSynthesisProvider, ExtendedComponent},
    hooks::{CapabilityReceiver, Event, EventPayload},
};
use async_trait::async_trait;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_inspect::InspectSinkMarker;
use fuchsia_async::TaskGroup;
use fuchsia_inspect::Inspector;
use futures::lock::Mutex;
use inspect_runtime::{publish, PublishOptions};
use moniker::Moniker;
use routing::event::EventFilter;
use sandbox::Message;

/// A struct for providing CapabilityRequested events carrying a channel for
/// `fuchsia.inspect.InspectSink`.
pub struct InspectSinkProvider {
    /// This keeps track of living servers for `fuchsia.inspect.Tree`. If archivist restarts,
    /// the existing servers will eventually die and a new one will be inserted here when
    /// `EventSynthesisProvider::provide` is triggered on reconnect.
    inspect_tree_server_tasks: Mutex<TaskGroup>,
    inspector: Inspector,
}

impl InspectSinkProvider {
    pub fn new(inspector: Inspector) -> Self {
        Self { inspect_tree_server_tasks: Mutex::new(TaskGroup::new()), inspector }
    }

    pub fn inspector(&self) -> &Inspector {
        &self.inspector
    }
}

#[async_trait]
impl EventSynthesisProvider for InspectSinkProvider {
    async fn provide(&self, component: ExtendedComponent, filter: &EventFilter) -> Vec<Event> {
        if !filter.contains("name", vec![InspectSinkMarker::PROTOCOL_NAME.into()]) {
            return vec![];
        }

        if !matches!(component, ExtendedComponent::ComponentManager) {
            return vec![];
        }

        let (client, server) = fidl::endpoints::create_endpoints();

        if let Some(server_task) =
            publish(&self.inspector, PublishOptions::default().on_inspect_sink_client(client))
        {
            self.inspect_tree_server_tasks.lock().await.add(server_task);

            // this value is irrelevant, archivist won't do anything with it but it is part of
            // the protocol
            let Ok(source_moniker) = Moniker::try_from("parent") else {
                return vec![];
            };

            let (receiver, sender) = CapabilityReceiver::new();
            let _ = sender.send(Message {
                payload: fsandbox::ProtocolPayload { channel: server.into_channel() },
            });
            vec![Event::new_builtin(EventPayload::CapabilityRequested {
                source_moniker,
                name: InspectSinkMarker::PROTOCOL_NAME.into(),
                receiver,
            })]
        } else {
            vec![]
        }
    }
}
