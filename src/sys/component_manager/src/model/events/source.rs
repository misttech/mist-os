// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilityProvider;
use crate::model::component::{ExtendedInstance, WeakExtendedInstance};
use crate::model::events::registry::{EventRegistry, EventSubscription};
use crate::model::events::serve::serve_event_stream;
use crate::model::events::stream::EventStream;
use crate::model::events::stream_provider::EventStreamProvider;
use async_trait::async_trait;
use cm_util::TaskGroup;
use errors::{CapabilityProviderError, EventSourceError, EventsError, ModelError};
use fidl::endpoints::RequestStream;
use futures::{SinkExt, StreamExt};
use moniker::ExtendedMoniker;
use std::sync::{Mutex, Weak};
use vfs::directory::entry::OpenRequest;
use vfs::path::Path as VfsPath;
use vfs::service::endpoint;

// Event source (supporting event streams)
#[derive(Clone)]
pub struct EventSource {
    /// A shared reference to the event registry used to subscribe and dispatch events.
    registry: Weak<EventRegistry>,

    /// The static EventStreamProvider tracks all static event streams. It can be used to take the
    /// server end of the static event streams.
    stream_provider: Weak<EventStreamProvider>,

    /// The moniker of the component subscribing to events.
    subscriber: WeakExtendedInstance,
}

impl EventSource {
    pub fn new(
        subscriber: WeakExtendedInstance,
        registry: Weak<EventRegistry>,
        stream_provider: Weak<EventStreamProvider>,
    ) -> Self {
        Self { subscriber, registry, stream_provider }
    }

    /// Subscribes to events provided in the `events` vector.
    ///
    /// The client might request to subscribe to events that it's not allowed to see. Events
    /// that are allowed should have been defined in its manifest and either offered to it or
    /// requested from the current realm.
    pub async fn subscribe(
        &mut self,
        requests: Vec<EventSubscription>,
    ) -> Result<EventStream, ModelError> {
        let registry = self.registry.upgrade().ok_or(EventsError::RegistryNotFound)?;
        let mut static_streams = vec![];
        let subscriber_moniker = self.subscriber.extended_moniker();
        if let Some(stream_provider) = self.stream_provider.upgrade() {
            for request in requests {
                if let Some(res) =
                    stream_provider.take_static_event_stream(&subscriber_moniker, &request).await
                {
                    static_streams.push(res);
                } else {
                    // Subscribe to events in the registry, discarding prior events
                    // from before this subscribe call if this is the second
                    // time opening the event stream.
                    if request.event_name.source_name == "capability_requested" {
                        // Don't support creating a new capability_requested stream.
                        return Err(ModelError::EventsError {
                            err: EventsError::CapabilityRequestedStreamTaken,
                        });
                    }
                    let stream = registry.subscribe(&self.subscriber, vec![request]).await?;
                    static_streams.push(stream);
                }
            }
        }
        // Create an event stream for the given events
        let subscriptions: Vec<EventSubscription> = vec![];
        let mut stream = registry.subscribe(&self.subscriber, subscriptions).await?;
        for mut request in static_streams {
            let mut tx = stream.sender();
            stream.tasks.push(fuchsia_async::Task::spawn(async move {
                while let Some((event, _)) = request.next().await {
                    if let Err(_) = tx.send((event, Some(request.route.clone()))).await {
                        break;
                    }
                }
            }));
        }
        Ok(stream)
    }

    /// Subscribes to all applicable events in a single use statement.
    /// This method may be called once per path, and will return None
    /// if the event stream has already been consumed.
    async fn subscribe_all(
        &mut self,
        subscriber_moniker: ExtendedMoniker,
        path: String,
    ) -> Result<Option<EventStream>, ModelError> {
        if let Some(stream_provider) = self.stream_provider.upgrade() {
            if let Some(event_names) = stream_provider.take_events(subscriber_moniker, path).await {
                let subscriptions = event_names
                    .into_iter()
                    .map(|name| EventSubscription { event_name: name })
                    .collect();
                return Ok(Some(self.subscribe(subscriptions).await?));
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl CapabilityProvider for EventSource {
    async fn open(
        mut self: Box<Self>,
        _task_group: TaskGroup,
        mut open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        // Spawn the task in the component's task scope so that when the component is destroyed,
        // the task is cancelled and does not leak (similar to how framework capabilities are
        // scoped).
        let task_group = match self.subscriber.upgrade()? {
            ExtendedInstance::Component(target) => target.nonblocking_task_group(),
            ExtendedInstance::AboveRoot(target) => target.task_group(),
        };

        let moniker = self.subscriber.extended_moniker();

        // NOTE: EventSource is a protocol capability for which you'd normally expect an empty path,
        // but we are abusing the path in the open request to allow us to identify a specific
        // subscription.  Earlier in routing, the path in the open request is replaced with the
        // identifier (after checking that it is empty).

        // VFS paths don't have a leading '/', but the identiifiers we use do, so we need to add it
        // here.
        let path = format!("/{}", open_request.path().as_ref());
        let event_stream = Mutex::new(Some(
            self.subscribe_all(moniker, path)
                .await
                .map_err(|e| {
                    CapabilityProviderError::EventSourceError(EventSourceError::Model(Box::new(e)))
                })?
                .ok_or(EventSourceError::AlreadyConsumed)?,
        ));

        open_request.set_path(VfsPath::dot());
        open_request
            .open_service(endpoint(move |_scope, channel| {
                task_group.spawn(serve_event_stream(
                    event_stream.lock().unwrap().take().unwrap(),
                    RequestStream::from_channel(channel),
                ))
            }))
            .map_err(|e| CapabilityProviderError::VfsOpenError(e))
    }
}
