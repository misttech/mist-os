// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    Capability, CapabilityBound, Dict, SpecificRouter, SpecificRouterResponse, Unit,
    WeakInstanceToken,
};
use async_trait::async_trait;
use futures::future::BoxFuture;
use router_error::{Explain, RouterError};
use std::fmt;
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;
use zx_status::Status;

/// Types that implement [`Routable`] let the holder asynchronously request
/// capabilities from them.
#[async_trait]
pub trait Routable: Send + Sync {
    async fn route(&self, request: Option<Request>, debug: bool)
        -> Result<Capability, RouterError>;
}

/// [`Request`] contains metadata around how to obtain a capability.
#[derive(Debug)]
pub struct Request {
    /// A reference to the requesting component.
    pub target: WeakInstanceToken,

    /// Metadata associated with the request.
    pub metadata: Dict,
}

impl Request {
    /// Clones the [`Request`] where the metadata [`Dict`] is a shallow copy. As a
    /// result, the metadata [`Dict`] must not contain a nested [`Dict`] otherwise a
    /// [`RouterError::InvalidArgs`] error will be returned.
    pub fn try_clone(&self) -> Result<Self, RouterError> {
        self.metadata
            .enumerate()
            .find_map(|(_, v)| {
                match v {
                    // Since Dictionaries are shallow copied, throw an error if
                    // there is a nested Dictionary.
                    Ok(Capability::Dictionary(_)) => Some(Err::<Self, _>(RouterError::InvalidArgs)),
                    _ => None,
                }
            })
            .transpose()?;
        let metadata = self.metadata.shallow_copy().map_err(|()| RouterError::InvalidArgs)?;
        Ok(Self { target: self.target.clone(), metadata })
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

impl CapabilityBound for Router {}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO(https://fxbug.dev/329680070): Require `Debug` on `Routable` trait.
        f.debug_struct("Router").field("routable", &"[some routable object]").finish()
    }
}

/// Syntax sugar within the framework to express custom routing logic using a function
/// that takes a request and returns such future.
impl<F> Routable for F
where
    F: Fn(Option<Request>, bool) -> BoxFuture<'static, Result<Capability, RouterError>>
        + Send
        + Sync
        + 'static,
{
    // We use the desugared form of `async_trait` to avoid unnecessary boxing.
    fn route<'a, 'b>(
        &'a self,
        request: Option<Request>,
        debug: bool,
    ) -> BoxFuture<'b, Result<Capability, RouterError>>
    where
        'a: 'b,
        Self: 'b,
    {
        self(request, debug)
    }
}

#[async_trait]
impl Routable for Router {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Capability, RouterError> {
        Router::route(self, request, debug).await
    }
}

// TODO(https://fxbug.dev/360221168): Delete this once all routers are their specialized types
impl<T: CapabilityBound> From<SpecificRouter<T>> for Router {
    fn from(router: SpecificRouter<T>) -> Self {
        Router::new(InnerRouter { router })
    }
}

struct InnerRouter<T: CapabilityBound> {
    router: SpecificRouter<T>,
}
#[async_trait]
impl<T: CapabilityBound> Routable for InnerRouter<T> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Capability, RouterError> {
        let resp = self.router.route(request, debug).await?;
        Ok(match resp {
            SpecificRouterResponse::Capability(c) => c.into(),
            SpecificRouterResponse::Unavailable => Capability::Unit(Unit {}),
            SpecificRouterResponse::Debug(d) => Capability::Data(d),
        })
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
    pub async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Capability, RouterError> {
        self.routable.route(request, debug).await
    }
}

#[derive(Debug, Error, Clone)]
struct CapabilityNotCloneableError {}

impl Explain for CapabilityNotCloneableError {
    fn as_zx_status(&self) -> Status {
        Status::NOT_FOUND
    }
}

impl fmt::Display for CapabilityNotCloneableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "capability not cloneable")
    }
}

#[async_trait]
impl Routable for Capability {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Capability, RouterError> {
        match self.try_clone() {
            Ok(Capability::Router(router)) => router.route(request, debug).await,
            Ok(capability) => Ok(capability),
            Err(_) => Err(RouterError::NotFound(Arc::new(CapabilityNotCloneableError {}))),
        }
    }
}

#[async_trait]
impl Routable for Dict {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Capability, RouterError> {
        Capability::Dictionary(self.clone()).route(request, debug).await
    }
}

#[async_trait]
impl Routable for RouterError {
    async fn route(&self, _: Option<Request>, _: bool) -> Result<Capability, RouterError> {
        Err(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Connector, Data, DictKey, Message};
    use cm_types::Availability;
    use fidl::handle::Channel;

    #[fuchsia::test]
    async fn route_and_use_connector_with_dropped_receiver() {
        // We want to test vending a sender with a router, dropping the associated receiver, and
        // then using the sender. The objective is to observe an error, and not panic.
        let (receiver, sender) = Connector::new();
        let router = Router::new_ok(sender.clone());

        let metadata = Dict::new();
        metadata
            .insert(
                DictKey::new("availability").unwrap(),
                Capability::Data(Data::String(Availability::Required.to_string())),
            )
            .unwrap();
        let capability = router.route(None, false).await.unwrap();
        let sender = match capability {
            Capability::Connector(c) => c,
            c => panic!("Bad enum {:#?}", c),
        };

        drop(receiver);
        let (ch1, _ch2) = Channel::create();
        assert!(sender.send(Message { channel: ch1 }).is_err());
    }
}
