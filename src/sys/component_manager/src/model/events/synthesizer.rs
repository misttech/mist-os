// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::events::dispatcher::EventDispatcherScope;
use crate::model::events::event::Event;
use crate::model::events::registry::ComponentEventRoute;
use ::routing::event::EventFilter;
use cm_types::Name;
use cm_util::TaskGroup;
use errors::ModelError;
use futures::channel::mpsc;
use futures::future::join_all;
use futures::SinkExt;
use hooks::{Event as HookEvent, EventType};
use log::error;
use moniker::ExtendedMoniker;
use std::collections::HashMap;
use std::sync::Arc;

/// Implementors of this trait know how to synthesize an event.
pub trait ComponentManagerEventSynthesisProvider: Send + Sync {
    /// Provides a synthesized event applying the given `filter` under the given `component`.
    fn provide(&self, filter: &EventFilter) -> Option<HookEvent>;
}

/// Synthesis manager.
#[derive(Default)]
pub struct EventSynthesizer {
    /// Maps an event name to the provider for synthesis
    providers: HashMap<Name, Arc<dyn ComponentManagerEventSynthesisProvider>>,
}

impl EventSynthesizer {
    /// Registers a new provider that will be used when synthesizing events of the type `event`.
    pub fn register_provider(
        &mut self,
        event: EventType,
        provider: Arc<dyn ComponentManagerEventSynthesisProvider>,
    ) {
        self.providers.insert(event.into(), provider);
    }

    /// Spawns a synthesis task for the requested `events`. Resulting events will be sent on the
    /// `sender` channel.
    pub async fn spawn_synthesis(
        &self,
        sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
        events: HashMap<Name, Vec<EventDispatcherScope>>,
        scope: &TaskGroup,
    ) {
        SynthesisTask::new(&self, sender, events).spawn(scope).await
    }
}

/// Information about an event that will be synthesized.
struct EventSynthesisInfo {
    /// The provider of the synthesized event.
    provider: Arc<dyn ComponentManagerEventSynthesisProvider>,

    /// The scopes under which the event will be synthesized.
    scopes: Vec<EventDispatcherScope>,
}

struct SynthesisTask {
    /// The sender end of the channel where synthesized events will be sent.
    sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,

    /// Information about the events to synthesize
    event_infos: Vec<EventSynthesisInfo>,
}

impl SynthesisTask {
    /// Creates a new synthesis task from the given events. It will ignore events for which the
    /// `synthesizer` doesn't have a provider.
    pub fn new(
        synthesizer: &EventSynthesizer,
        sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
        mut events: HashMap<Name, Vec<EventDispatcherScope>>,
    ) -> Self {
        let event_infos = synthesizer
            .providers
            .iter()
            .filter_map(|(event_name, provider)| {
                events
                    .remove(event_name)
                    .map(|scopes| EventSynthesisInfo { provider: provider.clone(), scopes })
            })
            .collect();
        Self { sender, event_infos }
    }

    /// Spawns a task that will synthesize all events that were requested when creating the
    /// `SynthesisTask`
    pub async fn spawn(self, scope: &TaskGroup) {
        if self.event_infos.is_empty() {
            return;
        }
        scope.spawn(async move {
            let sender = self.sender;
            let futs = self
                .event_infos
                .into_iter()
                .map(|event_info| Self::run(sender.clone(), event_info));
            for result in join_all(futs).await {
                if let Err(error) = result {
                    error!(error:?; "Event synthesis failed");
                }
            }
        });
    }

    /// Performs a depth-first traversal of the component instance tree. It adds to the stream a
    /// `Running` event for all components that are running. In the case of overlapping scopes,
    /// events are deduped.  It also synthesizes events that were requested which are synthesizable
    /// (there's a provider for them). Those events will only be synthesized if their scope is
    /// within the scope of a Running scope.
    async fn run(
        mut sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
        info: EventSynthesisInfo,
    ) -> Result<(), ModelError> {
        for scope in &info.scopes {
            // If the scope is component manager, synthesize the builtin events first and then
            // proceed to synthesize from the root and down.
            if matches!(scope.moniker, ExtendedMoniker::ComponentManager) {
                if let Some(event) = info.provider.provide(&scope.filter) {
                    let event = Event { event, scope_moniker: scope.moniker.clone() };
                    // Ignore this error. This can occur when the event stream is closed in the
                    // middle of synthesis. We can finish synthesizing if an error happens.
                    let _ = sender.send((event, None)).await;
                }
            }
        }
        Ok(())
    }
}
