// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Capability, CapabilityTrait, Dict, WeakComponentToken};
use async_trait::async_trait;
use cm_types::Availability;
use fidl::AsHandleRef;
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_zircon as zx;
use futures::future::BoxFuture;
use futures::TryStreamExt;
use router_error::RouterError;
use std::fmt::Debug;
use std::{fmt, sync::Arc};

/// Types that implement [`Routable`] let the holder asynchronously request
/// capabilities from them.
#[async_trait]
pub trait Routable: Send + Sync {
    async fn route(&self, request: Request) -> Result<Capability, RouterError>;
}

/// [`Request`] contains metadata around how to obtain a capability.
#[derive(Debug, Clone)]
pub struct Request {
    /// The minimal availability strength of the capability demanded by the requestor.
    pub availability: Availability,

    /// A reference to the requesting component.
    pub target: WeakComponentToken,
}

impl From<Request> for fsandbox::RouteRequest {
    fn from(request: Request) -> Self {
        let availability = from_cm_type(request.availability);
        let (token, server) = zx::EventPair::create();
        request.target.register(token.get_koid().unwrap(), server);
        fsandbox::RouteRequest {
            availability: Some(availability),
            requesting: Some(fsandbox::ComponentToken { token }),
            ..Default::default()
        }
    }
}

/// A [`Router`] is a capability that lets the holder obtain other capabilities
/// asynchronously. [`Router`] is the object capability representation of [`Routable`].
///
/// During routing, a request usually traverses through the component topology,
/// passing through several routers, ending up at some router that will fulfill
/// the request instead of forwarding it upstream.
#[derive(Clone)]
pub struct Router {
    routable: Arc<dyn Routable>,
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO(https://fxbug.dev/329680070): Require `Debug` on `Routable` trait.
        f.debug_struct("Router").field("routable", &"[some routable object]").finish()
    }
}

// TODO(b/314343346): Complete or remove the Router implementation of sandbox::Capability
impl CapabilityTrait for Router {
    fn into_fidl(self) -> fsandbox::Capability {
        self.into()
    }
}

/// Syntax sugar within the framework to express custom routing logic using a function
/// that takes a request and returns such future.
impl<F> Routable for F
where
    F: Fn(Request) -> BoxFuture<'static, Result<Capability, RouterError>> + Send + Sync + 'static,
{
    // We use the desugared form of `async_trait` to avoid unnecessary boxing.
    fn route<'a, 'b>(&'a self, request: Request) -> BoxFuture<'b, Result<Capability, RouterError>>
    where
        'a: 'b,
        Self: 'b,
    {
        self(request)
    }
}

#[async_trait]
impl Routable for Router {
    async fn route(&self, request: Request) -> Result<Capability, RouterError> {
        Router::route(self, request).await
    }
}

impl Router {
    /// Package a [`Routable`] object into a [`Router`].
    pub fn new(routable: impl Routable + 'static) -> Self {
        Router { routable: Arc::new(routable) }
    }

    /// Creates a router that will always resolve with the provided capability,
    /// unless the capability is also a router, where it will recursively request
    /// from the router.
    pub fn new_ok(capability: impl Into<Capability>) -> Self {
        let capability: Capability = capability.into();
        Router::new(capability)
    }

    /// Creates a router that will always fail a request with the provided error.
    pub fn new_error(error: impl Into<RouterError>) -> Self {
        let error: RouterError = error.into();
        Router::new(error)
    }

    /// Obtain a capability from this router, following the description in `request`.
    pub async fn route(&self, request: Request) -> Result<Capability, RouterError> {
        self.routable.route(request).await
    }

    async fn serve_router(
        self,
        mut stream: fsandbox::RouterRequestStream,
    ) -> Result<(), fidl::Error> {
        async fn do_route(
            router: &Router,
            payload: fsandbox::RouteRequest,
        ) -> Result<fsandbox::Capability, fsandbox::RouterError> {
            let Some(availability) = payload.availability else {
                return Err(fsandbox::RouterError::InvalidArgs);
            };
            let Some(token) = payload.requesting else {
                return Err(fsandbox::RouterError::InvalidArgs);
            };
            let capability = crate::registry::get(token.token.as_handle_ref().get_koid().unwrap());
            let component = match capability {
                Some(crate::Capability::Component(c)) => c,
                Some(_) => return Err(fsandbox::RouterError::InvalidArgs),
                None => return Err(fsandbox::RouterError::InvalidArgs),
            };
            let request = Request { availability: to_cm_type(availability), target: component };
            let cap = router.route(request).await?;
            Ok(cap.into())
        }

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::RouterRequest::Route { payload, responder } => {
                    responder.send(do_route(&self, payload).await)?;
                }
                fsandbox::RouterRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!("Received unknown Router request with ordinal {ordinal}");
                }
            }
        }
        Ok(())
    }

    /// Serves the `fuchsia.sandbox.Router` protocol and moves ourself into the registry.
    pub fn serve_and_register(self, stream: fsandbox::RouterRequestStream, koid: zx::Koid) {
        let router = self.clone();

        // Move this capability into the registry.
        crate::registry::insert(self.into(), koid, async move {
            router.serve_router(stream).await.expect("failed to serve Router");
        });
    }
}

impl From<Router> for fsandbox::Capability {
    fn from(router: Router) -> Self {
        let (client_end, sender_stream) =
            fidl::endpoints::create_request_stream::<fsandbox::RouterMarker>().unwrap();
        router.serve_and_register(sender_stream, client_end.get_koid().unwrap());
        fsandbox::Capability::Router(client_end)
    }
}

#[async_trait]
impl Routable for Capability {
    async fn route(&self, request: Request) -> Result<Capability, RouterError> {
        match self.clone() {
            Capability::Router(router) => router.route(request).await,
            capability => Ok(capability),
        }
    }
}

#[async_trait]
impl Routable for Dict {
    async fn route(&self, request: Request) -> Result<Capability, RouterError> {
        Capability::Dictionary(self.clone()).route(request).await
    }
}

#[async_trait]
impl Routable for RouterError {
    async fn route(&self, _: Request) -> Result<Capability, RouterError> {
        Err(self.clone())
    }
}

fn to_cm_type(value: fsandbox::Availability) -> cm_types::Availability {
    match value {
        fsandbox::Availability::Required => cm_types::Availability::Required,
        fsandbox::Availability::Optional => cm_types::Availability::Optional,
        fsandbox::Availability::SameAsTarget => cm_types::Availability::SameAsTarget,
        fsandbox::Availability::Transitional => cm_types::Availability::Transitional,
    }
}

fn from_cm_type(value: cm_types::Availability) -> fsandbox::Availability {
    match value {
        cm_types::Availability::Required => fsandbox::Availability::Required,
        cm_types::Availability::Optional => fsandbox::Availability::Optional,
        cm_types::Availability::SameAsTarget => fsandbox::Availability::SameAsTarget,
        cm_types::Availability::Transitional => fsandbox::Availability::Transitional,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, Receiver};
    use assert_matches::assert_matches;
    use fuchsia_zircon as zx;

    #[derive(Debug)]
    struct FakeComponentToken {}

    impl FakeComponentToken {
        fn new() -> WeakComponentToken {
            WeakComponentToken { inner: Arc::new(FakeComponentToken {}) }
        }
    }

    impl crate::WeakComponentTokenAny for FakeComponentToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[fuchsia::test]
    async fn route_and_use_connector_with_dropped_receiver() {
        // We want to test vending a sender with a router, dropping the associated receiver, and
        // then using the sender. The objective is to observe an error, and not panic.
        let (receiver, sender) = Receiver::new();
        let router = Router::new_ok(sender.clone());

        let capability = router
            .route(Request {
                availability: Availability::Required,
                target: FakeComponentToken::new(),
            })
            .await
            .unwrap();
        let sender = match capability {
            Capability::Connector(c) => c,
            c => panic!("Bad enum {:#?}", c),
        };

        drop(receiver);
        let (ch1, _ch2) = zx::Channel::create();
        assert!(sender.send(Message { channel: ch1 }).is_err());
    }

    #[fuchsia::test]
    async fn serve_router() {
        let component = FakeComponentToken::new();
        let (component_client, server) = zx::EventPair::create();
        let koid = server.basic_info().unwrap().related_koid;
        component.register(koid, server);

        let (_, sender) = Receiver::new();
        let router = Router::new_ok(sender);
        let (client, stream) =
            fidl::endpoints::create_proxy_and_stream::<fsandbox::RouterMarker>().unwrap();
        let _stream = fuchsia_async::Task::spawn(router.serve_router(stream));

        let capability = client
            .route(fsandbox::RouteRequest {
                availability: Some(fsandbox::Availability::Required),
                requesting: Some(fsandbox::ComponentToken { token: component_client }),
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();
        assert_matches!(capability, fsandbox::Capability::Connector(_));
    }

    #[fuchsia::test]
    async fn serve_router_bad_arguments() {
        let (_, sender) = Receiver::new();
        let router = Router::new_ok(sender);
        let (client, stream) =
            fidl::endpoints::create_proxy_and_stream::<fsandbox::RouterMarker>().unwrap();
        let _stream = fuchsia_async::Task::spawn(router.serve_router(stream));

        // Check with no component token.
        let capability = client
            .route(fsandbox::RouteRequest {
                availability: Some(fsandbox::Availability::Required),
                requesting: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_matches!(capability, Err(fsandbox::RouterError::InvalidArgs));

        let component = FakeComponentToken::new();
        let (component_client, server) = zx::EventPair::create();
        let koid = server.basic_info().unwrap().related_koid;
        component.register(koid, server);

        // Check with no availability.
        let capability = client
            .route(fsandbox::RouteRequest {
                availability: None,
                requesting: Some(fsandbox::ComponentToken { token: component_client }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_matches!(capability, Err(fsandbox::RouterError::InvalidArgs));
    }

    #[fuchsia::test]
    async fn serve_router_bad_token() {
        let (_, sender) = Receiver::new();
        let router = Router::new_ok(sender);
        let (client, stream) =
            fidl::endpoints::create_proxy_and_stream::<fsandbox::RouterMarker>().unwrap();
        let _stream = fuchsia_async::Task::spawn(router.serve_router(stream));

        // Create the client but don't register it.
        let (component_client, _server) = zx::EventPair::create();

        let capability = client
            .route(fsandbox::RouteRequest {
                availability: Some(fsandbox::Availability::Required),
                requesting: Some(fsandbox::ComponentToken { token: component_client }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_matches!(capability, Err(fsandbox::RouterError::InvalidArgs));
    }
}
