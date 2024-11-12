// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Capability, CapabilityBound, Data, Dict, WeakInstanceToken};
use async_trait::async_trait;
use futures::future::BoxFuture;
use router_error::RouterError;
use std::fmt;
use std::fmt::Debug;
use std::sync::Arc;

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

/// Types that implement [`Routable`] let the holder asynchronously request capabilities
/// from them.
#[async_trait]
pub trait Routable<T>: Send + Sync
where
    T: CapabilityBound,
{
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError>;
}

/// Response of a [Router] request.
#[derive(Debug)]
pub enum RouterResponse<T: CapabilityBound> {
    /// Routing succeeded and returned this capability.
    Capability(T),

    /// Routing succeeded, but the capability was marked unavailable.
    Unavailable,

    /// Routing succeeded in debug mode, `Data` contains the debug data.
    Debug(Data),
}

/// A [`Router`] is a capability that lets the holder obtain other capabilities
/// asynchronously. [`Router`] is the object capability representation of
/// [`Routable`].
///
/// During routing, a request usually traverses through the component topology,
/// passing through several routers, ending up at some router that will fulfill
/// the request instead of forwarding it upstream.
///
/// [`Router`] differs from [`Router`] in that it is parameterized on the capability
/// type `T`. Instead of a [`Capability`], [`Router`] returns a [`RouterResponse`].
/// [`Router`] will supersede [`Router`].
#[derive(Clone)]
pub struct Router<T: CapabilityBound> {
    routable: Arc<dyn Routable<T>>,
}

impl CapabilityBound for Router<crate::Connector> {
    fn debug_typename() -> &'static str {
        "ConnectorRouter"
    }
}
impl CapabilityBound for Router<crate::Data> {
    fn debug_typename() -> &'static str {
        "DataRouter"
    }
}
impl CapabilityBound for Router<crate::DirEntry> {
    fn debug_typename() -> &'static str {
        "DirEntryRouter"
    }
}
impl CapabilityBound for Router<crate::Dict> {
    fn debug_typename() -> &'static str {
        "DictionaryRouter"
    }
}

impl CapabilityBound for Router<crate::DirConnector> {
    fn debug_typename() -> &'static str {
        "DirConnectorRouter"
    }
}

impl<T: CapabilityBound> fmt::Debug for Router<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO(https://fxbug.dev/329680070): Require `Debug` on `Routable` trait.
        f.debug_struct("Router").field("routable", &"[some routable object]").finish()
    }
}

/// Syntax sugar within the framework to express custom routing logic using a function
/// that takes a request and returns such future.
impl<T: CapabilityBound, F> Routable<T> for F
where
    F: Fn(Option<Request>, bool) -> BoxFuture<'static, Result<RouterResponse<T>, RouterError>>
        + Send
        + Sync
        + 'static,
{
    // We use the desugared form of `async_trait` to avoid unnecessary boxing.
    fn route<'a, 'b>(
        &'a self,
        request: Option<Request>,
        debug: bool,
    ) -> BoxFuture<'b, Result<RouterResponse<T>, RouterError>>
    where
        'a: 'b,
        Self: 'b,
    {
        self(request, debug)
    }
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for Router<T> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        Router::route(self, request, debug).await
    }
}

impl<T: CapabilityBound> Router<T> {
    /// Package a [`Routable`] object into a [`Router`].
    pub fn new(routable: impl Routable<T> + 'static) -> Self {
        Self { routable: Arc::new(routable) }
    }

    /// Creates a router that will always fail a request with the provided error.
    pub fn new_error(error: impl Into<RouterError>) -> Self {
        let v: RouterError = error.into();
        Self::new(ErrRouter { v })
    }

    /// Creates a router that will always return the given debug info.
    pub fn new_debug(data: impl Into<Data>) -> Self {
        let v: Data = data.into();
        Self::new(DebugRouter { v })
    }

    /// Obtain a capability from this router, following the description in `request`.
    pub async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        self.routable.route(request, debug).await
    }
}

impl<T: Clone + CapabilityBound> Router<T> {
    /// Creates a router that will always resolve with the provided capability.
    // TODO: Should this require debug info?
    pub fn new_ok(c: impl Into<T>) -> Self {
        let v: T = c.into();
        Self::new(OkRouter { v })
    }
}

#[derive(Clone)]
struct OkRouter<T: Clone + CapabilityBound> {
    v: T,
}

#[async_trait]
impl<T: Clone + CapabilityBound> Routable<T> for OkRouter<T> {
    async fn route(
        &self,
        _request: Option<Request>,
        _debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        Ok(RouterResponse::Capability(self.v.clone()))
    }
}

#[derive(Clone)]
struct DebugRouter {
    v: Data,
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for DebugRouter {
    async fn route(
        &self,
        _request: Option<Request>,
        _debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        Ok(RouterResponse::Debug(self.v.clone()))
    }
}

#[derive(Clone)]
struct ErrRouter {
    v: RouterError,
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for ErrRouter {
    async fn route(
        &self,
        _request: Option<Request>,
        _debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        Err(self.v.clone())
    }
}
