// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ControlHandle;
use fidl::Error::ClientChannelClosed;
use fidl_fuchsia_memory_attribution as fattribution;
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use fuchsia_zircon::AsHandleRef;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tracing::error;

/// Function of this type returns a vector of attribution updates, and is used
/// as the type of the callback in [AttributionServer::new].
type GetAttributionFn = dyn Fn() -> Vec<fattribution::AttributionUpdate> + Send;

/// Error types that may be used by the async hanging-get server.
#[derive(Error, Debug)]
pub enum AttributionServerObservationError {
    #[error("multiple pending observations for the same Observer")]
    GetUpdateAlreadyPending,
}

/// Error types that may be used when sending an attribution update.
#[derive(Error, Debug)]
pub enum AttributionServerPublishError {
    #[error("the update is malformed")]
    MalformedUpdate,
}

/// Main structure for the memory attribution hanging get server.
///
/// Components that wish to expose attribution information should create a
/// single [AttributionServer] object.
/// Each inbound fuchsia.attribution.Provider connection should get its own
/// [Observer] object using [AttributionServer::new_observer].
/// [Publisher]s, created using [AttributionServer::new_publisher], should be
/// used to push attribution changes.
#[derive(Clone)]
pub struct AttributionServerHandle {
    inner: Arc<Mutex<AttributionServer>>,
}

impl AttributionServerHandle {
    /// Create a new [Observer] that represents a single client.
    ///
    /// Each FIDL client connection should get its own [Observer] object.
    pub fn new_observer(&self, control_handle: fattribution::ProviderControlHandle) -> Observer {
        AttributionServer::register(&self.inner, control_handle)
    }

    /// Create a new [Publisher] that can push updates to observers.
    pub fn new_publisher(&self) -> Publisher {
        Publisher { inner: self.inner.clone() }
    }
}

/// An `Observer` can be used to register observation requests, corresponding to individual hanging
/// get calls. These will be notified when the state changes or immediately the first time
/// an `Observer` registers an observation.
pub struct Observer {
    inner: Arc<Mutex<AttributionServer>>,
}

impl Observer {
    /// Register a new observation request.
    ///
    /// A newly-created observer will first receive the current state. After
    /// the first call, the observer will be notified only if the state changes.
    ///
    /// Errors occur when an Observer attempts to wait for an update when there
    /// is a update request already pending.
    pub fn next(&self, responder: fattribution::ProviderGetResponder) {
        self.inner.lock().next(responder)
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.inner.lock().unregister();
    }
}

/// A [Publisher] should be used to send updates to [Observer]s.
pub struct Publisher {
    inner: Arc<Mutex<AttributionServer>>,
}

impl Publisher {
    /// Registers an update to the state observed.
    ///
    /// `partial_state` is a function that returns the update.
    pub fn on_update(
        &self,
        updates: Vec<fattribution::AttributionUpdate>,
    ) -> Result<(), AttributionServerPublishError> {
        // [update_generator] is a `Fn` and not an `FnOnce` in order to be called multiple times,
        // once for each [Observer].
        self.inner.lock().on_update(updates)
    }
}

pub struct AttributionServer {
    state: Box<GetAttributionFn>,
    consumer: Option<AttributionConsumer>,
}

impl AttributionServer {
    /// Create a new memory attribution server.
    ///
    /// `state` is a function returning the complete attribution state (not partial updates).
    pub fn new(state: Box<GetAttributionFn>) -> AttributionServerHandle {
        AttributionServerHandle {
            inner: Arc::new(Mutex::new(AttributionServer { state, consumer: None })),
        }
    }

    pub fn on_update(
        &mut self,
        updates: Vec<fattribution::AttributionUpdate>,
    ) -> Result<(), AttributionServerPublishError> {
        if let Some(consumer) = &mut self.consumer {
            return consumer.update_and_notify(updates);
        }
        Ok(())
    }

    /// Get the next attribution state.
    pub fn next(&mut self, responder: fattribution::ProviderGetResponder) {
        let entry = self.consumer.as_mut().unwrap();
        entry.get_update(responder, self.state.as_ref());
    }

    pub fn register(
        inner: &Arc<Mutex<Self>>,
        control_handle: fattribution::ProviderControlHandle,
    ) -> Observer {
        let mut locked_inner = inner.lock();

        if let Some(consumer) = &locked_inner.consumer {
            tracing::error!("Multiple connection requests to AttributionProvider");
            consumer.observer_control_handle.shutdown_with_epitaph(zx::Status::CANCELED);
        }

        locked_inner.consumer = Some(AttributionConsumer::new(control_handle));
        Observer { inner: inner.clone() }
    }

    /// Deregister the current observer. No observer can be registered as long
    /// as another observer is already registered.
    pub fn unregister(&mut self) {
        self.consumer = None;
    }
}

/// Principal holds the identification for a principal.
#[derive(PartialEq, Eq, Hash, Clone)]
enum Principal {
    Self_(),
    Koid(zx::Koid),
    Part(String),
}

impl TryFrom<&fattribution::AttributionUpdate> for Principal {
    type Error = AttributionServerPublishError;

    fn try_from(value: &fattribution::AttributionUpdate) -> Result<Self, Self::Error> {
        let identifier = match value {
            fattribution::AttributionUpdate::Add(u) => u.identifier.as_ref().unwrap(),
            fattribution::AttributionUpdate::Update(u) => u.identifier.as_ref().unwrap(),
            fattribution::AttributionUpdate::Remove(u) => u,
            fattribution::AttributionUpdateUnknown!() => {
                return Err(AttributionServerPublishError::MalformedUpdate);
            }
        };
        match identifier {
            fattribution::Identifier::Self_(_) => Ok(Principal::Self_()),
            fattribution::Identifier::Component(component) => component
                .get_koid()
                .or(Err(AttributionServerPublishError::MalformedUpdate))
                .map(|k| Principal::Koid(k)),
            fattribution::Identifier::Part(part) => Ok(Principal::Part(part.to_owned())),
            fattribution::IdentifierUnknown!() => {
                return Err(AttributionServerPublishError::MalformedUpdate);
            }
        }
    }
}

/// CoalescedUpdate contains all the pending updates for a given principal.
#[derive(Default)]
struct CoalescedUpdate {
    add: Option<fattribution::AttributionUpdate>,
    update: Option<fattribution::AttributionUpdate>,
    remove: Option<fattribution::AttributionUpdate>,
}

/// Should the update be kept, or can it be discarded.
#[derive(PartialEq)]
enum ShouldKeepUpdate {
    KEEP,
    DISCARD,
}

impl CoalescedUpdate {
    /// Merges updates of a given Principal, discarding the ones that become irrelevant.
    pub fn update(&mut self, u: fattribution::AttributionUpdate) -> ShouldKeepUpdate {
        match u {
            fattribution::AttributionUpdate::Add(u) => {
                self.add = Some(fattribution::AttributionUpdate::Add(u));
                self.update = None;
                self.remove = None;
            }
            fattribution::AttributionUpdate::Update(u) => {
                self.update = Some(fattribution::AttributionUpdate::Update(u));
            }
            fattribution::AttributionUpdate::Remove(u) => {
                if self.add.is_some() {
                    // We both added and removed the principal, so it is a no-op.
                    return ShouldKeepUpdate::DISCARD;
                }
                self.remove = Some(fattribution::AttributionUpdate::Remove(u));
            }
            fattribution::AttributionUpdateUnknown!() => {
                error!("Unknown attribution update type");
            }
        };
        ShouldKeepUpdate::KEEP
    }

    pub fn get_updates(self) -> Vec<fattribution::AttributionUpdate> {
        let mut result = Vec::new();
        if let Some(u) = self.add {
            result.push(u);
        }
        if let Some(u) = self.update {
            result.push(u);
        }
        if let Some(u) = self.remove {
            result.push(u);
        }
        result
    }
}

/// AttributionConsumer tracks pending updates and observation requests for a given id.
struct AttributionConsumer {
    /// Whether we sent the first full state, or not.
    first: bool,

    /// Pending updates waiting to be sent.
    pending: HashMap<Principal, CoalescedUpdate>,

    /// Control handle for the FIDL connection.
    observer_control_handle: fattribution::ProviderControlHandle,

    /// FIDL responder for a pending hanging get call.
    responder: Option<fattribution::ProviderGetResponder>,
}

impl AttributionConsumer {
    /// Create a new [AttributionConsumer] without an `observer` and an initial `dirty`
    /// value of `true`.
    pub fn new(observer_control_handle: fattribution::ProviderControlHandle) -> Self {
        AttributionConsumer {
            first: true,
            pending: HashMap::new(),
            observer_control_handle: observer_control_handle,
            responder: None,
        }
    }

    /// Register a new observation request. The observer will be notified immediately if
    /// the [AttributionConsumer] has pending updates, or hasn't sent anything yet. The
    /// request will be stored for future notification if the [AttributionConsumer] does
    /// not have anything to send yet.
    pub fn get_update(
        &mut self,
        responder: fattribution::ProviderGetResponder,
        gen_state: &GetAttributionFn,
    ) {
        if self.responder.is_some() {
            self.observer_control_handle.shutdown_with_epitaph(zx::Status::BAD_STATE);
            return;
        }
        if self.first {
            self.first = false;
            self.pending.clear();
            Self::send_update(gen_state(), responder);
            return;
        }
        self.responder = Some(responder);
        self.maybe_notify();
    }

    /// Take in new memory attribution updates.
    pub fn update_and_notify(
        &mut self,
        updated_state: Vec<fattribution::AttributionUpdate>,
    ) -> Result<(), AttributionServerPublishError> {
        for update in updated_state {
            let principal: Principal = (&update).try_into()?;
            if self.pending.entry(principal.clone()).or_insert(Default::default()).update(update)
                == ShouldKeepUpdate::DISCARD
            {
                self.pending.remove(&principal);
            }
        }
        self.maybe_notify();
        Ok(())
    }

    /// Notify of the pending updates if a responder is available.
    fn maybe_notify(&mut self) {
        if self.pending.is_empty() {
            return;
        }

        match self.responder.take() {
            Some(observer) => Self::send_update(
                self.pending.drain().map(|(_, v)| v.get_updates()).flatten().collect(),
                observer,
            ),
            None => {}
        }
    }

    /// Sends the attribution update to the provided responder.
    fn send_update(
        state: Vec<fattribution::AttributionUpdate>,
        responder: fattribution::ProviderGetResponder,
    ) {
        match responder.send(Ok(fattribution::ProviderGetResponse {
            attributions: Some(state),
            ..Default::default()
        })) {
            Ok(()) => {} // indicates that the observer was successfully updated
            Err(e) => {
                // `send()` ensures that the channel is shut down in case of error.
                if let ClientChannelClosed { status: zx::Status::PEER_CLOSED, .. } = e {
                    // Skip if this is simply our client closing the channel.
                    return;
                }
                error!("Failed to send memory state to observer: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;
    use fidl::endpoints::RequestStream;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use futures::TryStreamExt;

    /// Tests that the ELF runner can tell us about the resources used by the component it runs.
    #[test]
    fn test_attribute_memory() {
        let mut exec = fasync::TestExecutor::new();
        let server = AttributionServer::new(Box::new(|| {
            let new_principal = fattribution::NewPrincipal {
                identifier: Some(fattribution::Identifier::Self_(fattribution::Self_)),
                type_: Some(fattribution::Type::Runnable),
                ..Default::default()
            };
            vec![fattribution::AttributionUpdate::Add(new_principal)]
        }));
        let (snapshot_provider, snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fattribution::ProviderMarker>().unwrap();

        let observer = server.new_observer(snapshot_request_stream.control_handle());
        fasync::Task::spawn(async move {
            serve(observer, snapshot_request_stream).await.unwrap();
        })
        .detach();

        let attributions =
            exec.run_singlethreaded(snapshot_provider.get()).unwrap().unwrap().attributions;
        assert!(attributions.is_some());

        let attributions_vec = attributions.unwrap();
        // It should contain one component, the one we just launched.
        assert_eq!(attributions_vec.len(), 1);
        let new_attrib = attributions_vec.get(0).unwrap();
        let fattribution::AttributionUpdate::Add(added_principal) = new_attrib else {
            panic!("Not a new principal");
        };
        assert_eq!(
            added_principal.identifier,
            Some(fattribution::Identifier::Self_(fattribution::Self_))
        );
        assert_eq!(added_principal.type_, Some(fattribution::Type::Runnable));

        server
            .new_publisher()
            .on_update(vec![fattribution::AttributionUpdate::Update(
                fattribution::UpdatedPrincipal {
                    identifier: Some(fattribution::Identifier::Self_(fattribution::Self_)),
                    ..Default::default()
                },
            )])
            .expect("Error sending the update");
        let attributions =
            exec.run_singlethreaded(snapshot_provider.get()).unwrap().unwrap().attributions;
        assert!(attributions.is_some());

        let attributions_vec = attributions.unwrap();
        // It should contain one component, the one we just launched.
        assert_eq!(attributions_vec.len(), 1);
        let updated_attrib = attributions_vec.get(0).unwrap();
        let fattribution::AttributionUpdate::Update(updated_principal) = updated_attrib else {
            panic!("Not an updated principal");
        };
        assert_eq!(
            updated_principal.identifier,
            Some(fattribution::Identifier::Self_(fattribution::Self_))
        );
    }

    pub async fn serve(
        observer: Observer,
        mut stream: fattribution::ProviderRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fattribution::ProviderRequest::Get { responder } => {
                    observer.next(responder);
                }
                fattribution::ProviderRequest::_UnknownMethod { .. } => {
                    assert!(false);
                }
            }
        }
        Ok(())
    }

    /// Tests that a new Provider connection cancels a previous one.
    #[test]
    fn test_disconnect_on_new_connection() {
        let mut exec = fasync::TestExecutor::new();
        let server = AttributionServer::new(Box::new(|| vec![]));
        let (snapshot_provider, snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fattribution::ProviderMarker>().unwrap();

        let _observer = server.new_observer(snapshot_request_stream.control_handle());

        let (_new_snapshot_provider, new_snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fattribution::ProviderMarker>().unwrap();

        let _new_observer = server.new_observer(new_snapshot_request_stream.control_handle());

        let result = exec.run_singlethreaded(snapshot_provider.get());
        assert_matches!(result, Err(ClientChannelClosed { status: zx::Status::CANCELED, .. }));
    }

    /// Tests that a new [Provider::get] call while another call is still pending
    /// generates an error.
    #[test]
    fn test_disconnect_on_two_pending_gets() {
        let mut exec = fasync::TestExecutor::new();
        let server = AttributionServer::new(Box::new(|| vec![]));
        let (snapshot_provider, snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fattribution::ProviderMarker>().unwrap();

        let observer = server.new_observer(snapshot_request_stream.control_handle());
        fasync::Task::spawn(async move {
            serve(observer, snapshot_request_stream).await.unwrap();
        })
        .detach();

        // The first call should succeed right away.
        exec.run_singlethreaded(snapshot_provider.get())
            .expect("Connection dropped")
            .expect("Get call failed");

        // The next call should block until an update is pushed on the provider side.
        let mut future = snapshot_provider.get();

        let _ = exec.run_until_stalled(&mut future);

        // The second parallel get() call should fail.
        let result = exec.run_singlethreaded(snapshot_provider.get());

        let result2 = exec.run_singlethreaded(future);

        assert_matches!(result2, Err(ClientChannelClosed { status: zx::Status::BAD_STATE, .. }));
        assert_matches!(result, Err(ClientChannelClosed { status: zx::Status::BAD_STATE, .. }));
    }

    /// Tests that the first get call returns the full state, not updates.
    #[test]
    fn test_no_update_on_first_call() {
        let mut exec = fasync::TestExecutor::new();
        let server = AttributionServer::new(Box::new(|| {
            let new_principal = fattribution::NewPrincipal {
                identifier: Some(fattribution::Identifier::Self_(fattribution::Self_)),
                type_: Some(fattribution::Type::Runnable),
                ..Default::default()
            };
            vec![fattribution::AttributionUpdate::Add(new_principal)]
        }));
        let (snapshot_provider, snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fattribution::ProviderMarker>().unwrap();

        let observer = server.new_observer(snapshot_request_stream.control_handle());
        fasync::Task::spawn(async move {
            serve(observer, snapshot_request_stream).await.unwrap();
        })
        .detach();

        server
            .new_publisher()
            .on_update(vec![fattribution::AttributionUpdate::Update(
                fattribution::UpdatedPrincipal {
                    identifier: Some(fattribution::Identifier::Self_(fattribution::Self_)),
                    ..Default::default()
                },
            )])
            .expect("Error sending the update");

        // As this is the first call, we should get the full state, not the update.
        let attributions =
            exec.run_singlethreaded(snapshot_provider.get()).unwrap().unwrap().attributions;
        assert!(attributions.is_some());

        let attributions_vec = attributions.unwrap();
        // It should contain one component, the one we just launched.
        assert_eq!(attributions_vec.len(), 1);
        let new_attrib = attributions_vec.get(0).unwrap();
        let fattribution::AttributionUpdate::Add(added_principal) = new_attrib else {
            panic!("Not a new principal");
        };
        assert_eq!(
            added_principal.identifier,
            Some(fattribution::Identifier::Self_(fattribution::Self_))
        );
        assert_eq!(added_principal.type_, Some(fattribution::Type::Runnable));
    }
}
