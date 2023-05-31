// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilityProvider,
        model::{
            component::{ExtendedInstance, WeakExtendedInstance},
            error::{CapabilityProviderError, ModelError},
            events::{
                error::EventsError,
                registry::{EventRegistry, EventSubscription},
                serve::serve_event_stream,
                stream::EventStream,
                stream_provider::EventStreamProvider,
            },
            model::Model,
        },
    },
    async_trait::async_trait,
    cm_task_scope::TaskScope,
    cm_util::channel,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{SinkExt, StreamExt},
    moniker::ExtendedMoniker,
    routing::component_instance::ComponentInstanceInterface,
    std::{
        path::PathBuf,
        sync::{Arc, Weak},
    },
};

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
    pub async fn new(
        subscriber: ExtendedMoniker,
        model: Weak<Model>,
        registry: Weak<EventRegistry>,
        stream_provider: Weak<EventStreamProvider>,
    ) -> Result<Self, ModelError> {
        let subscriber = {
            let model = model.upgrade().ok_or(EventsError::ModelNotAvailable)?;
            match &subscriber {
                ExtendedMoniker::ComponentInstance(m) => {
                    WeakExtendedInstance::Component(model.look_up(&m).await?.as_weak())
                }
                ExtendedMoniker::ComponentManager => {
                    WeakExtendedInstance::AboveRoot(Arc::downgrade(model.top_instance()))
                }
            }
        };
        Ok(Self { subscriber, registry, stream_provider })
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
                    if request.event_name.source_name.to_string() == "capability_requested" {
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
        _task_scope: TaskScope,
        _flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        // Spawn the task in the component's task scope so that when the component is destroyed,
        // the task is cancelled and does not leak (similar to how framework capabilities are
        // scoped).
        let task_scope = match self
            .subscriber
            .upgrade()
            .map_err(|err| CapabilityProviderError::EventSourceError { err })?
        {
            ExtendedInstance::Component(target) => target.nonblocking_task_scope(),
            ExtendedInstance::AboveRoot(target) => target.task_scope(),
        };
        let server_end = channel::take_channel(server_end);
        let stream = ServerEnd::<fcomponent::EventStreamMarker>::new(server_end);
        task_scope
            .add_task(async move {
                let moniker = self.subscriber.extended_moniker();
                if let Ok(Some(event_stream)) = self
                    .subscribe_all(moniker, relative_path.into_os_string().into_string().unwrap())
                    .await
                {
                    serve_event_stream(event_stream, stream).await;
                }
            })
            .await;
        Ok(())
    }
}
