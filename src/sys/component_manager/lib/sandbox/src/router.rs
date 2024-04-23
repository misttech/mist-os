// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{AnyCapability, Capability, CapabilityTrait, Dict};
use async_trait::async_trait;
use bedrock_error::BedrockError;
use cm_types::Availability;
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::future::BoxFuture;
use std::fmt::Debug;
use std::{any::Any, fmt, sync::Arc};

/// The trait that `WeakComponentToken` holds.
pub trait WeakComponentTokenAny: Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

/// A type representing a weak pointer to a component.
/// This is type erased because the bedrock library shouldn't depend on
/// Component Manager types.
#[derive(Clone, Debug)]
pub struct WeakComponentToken {
    pub inner: Arc<dyn WeakComponentTokenAny>,
}

/// Types that implement [`Routable`] let the holder asynchronously request
/// capabilities from them.
#[async_trait]
pub trait Routable: Send + Sync {
    async fn route(&self, request: Request) -> Result<Capability, BedrockError>;
}

/// [`Request`] contains metadata around how to obtain a capability.
#[derive(Debug, Clone)]
pub struct Request {
    /// The minimal availability strength of the capability demanded by the requestor.
    pub availability: Availability,

    /// A reference to the requesting component.
    pub target: WeakComponentToken,
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
impl CapabilityTrait for Router {}

impl From<Router> for Capability {
    fn from(router: Router) -> Self {
        Capability::Router(Box::new(router))
    }
}

/// Syntax sugar within the framework to express custom routing logic using a function
/// that takes a request and returns such future.
impl<F> Routable for F
where
    F: Fn(Request) -> BoxFuture<'static, Result<Capability, BedrockError>> + Send + Sync + 'static,
{
    // We use the desugared form of `async_trait` to avoid unnecessary boxing.
    fn route<'a, 'b>(&'a self, request: Request) -> BoxFuture<'b, Result<Capability, BedrockError>>
    where
        'a: 'b,
        Self: 'b,
    {
        self(request)
    }
}

#[async_trait]
impl Routable for Router {
    async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
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
    pub fn new_error(error: impl Into<BedrockError>) -> Self {
        let error: BedrockError = error.into();
        Router::new(error)
    }

    pub fn from_any(any: AnyCapability) -> Router {
        *any.into_any().downcast::<Router>().unwrap()
    }

    /// Obtain a capability from this router, following the description in `request`.
    pub async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
        self.routable.route(request).await
    }
}

impl From<Router> for fsandbox::Capability {
    fn from(_router: Router) -> Self {
        unimplemented!("TODO(b/314343346): Complete or remove the Router implementation of sandbox::Capability")
    }
}

#[async_trait]
impl Routable for Capability {
    async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
        match self.clone() {
            Capability::Router(router) => Router::from_any(router).route(request).await,
            capability => Ok(capability),
        }
    }
}

#[async_trait]
impl Routable for Dict {
    async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
        Capability::Dictionary(self.clone()).route(request).await
    }
}

#[async_trait]
impl Routable for BedrockError {
    async fn route(&self, _: Request) -> Result<Capability, BedrockError> {
        Err(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, Receiver};
    use fuchsia_zircon as zx;

    #[derive(Debug)]
    struct FakeComponentToken {}

    impl FakeComponentToken {
        fn new() -> WeakComponentToken {
            WeakComponentToken { inner: Arc::new(FakeComponentToken {}) }
        }
    }

    impl WeakComponentTokenAny for FakeComponentToken {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[fuchsia::test]
    async fn route_and_use_sender_with_dropped_receiver() {
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
            Capability::Sender(c) => c,
            c => panic!("Bad enum {:#?}", c),
        };

        drop(receiver);
        let (ch1, _ch2) = zx::Channel::create();
        assert!(sender.send(Message { channel: ch1 }).is_err());
    }
}
