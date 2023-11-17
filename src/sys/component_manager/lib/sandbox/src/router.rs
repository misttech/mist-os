// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{anyhow, Context, Result};
use cm_types::Availability;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot::{self, Canceled},
    future::BoxFuture,
    Future,
};
use moniker::Moniker;
use std::{collections::VecDeque, fmt, sync::Arc};

use crate::{AnyCapability, Capability, CloneError};

/// A [`Router`] is a capability that lets the holder obtain other capabilities
/// asynchronously using a request that traverses through the component topology.
///
/// During routing, the request might pass through several routers, ending up at some
/// router that will fulfill the completer instead of forwarding it upstream.
#[derive(Capability, Clone)]
#[capability(as_trait(AsRouter))]
pub struct Router {
    route_fn: Arc<RouteFn>,
}

/// [`RouteFn`] encapsulates arbitrary logic to fulfill a request to asynchronously
/// obtain a capability.
pub type RouteFn = dyn Fn(Request, Completer) -> () + Send + Sync;

/// [`Request`] contains metadata around how to obtain a capability.
pub struct Request {
    /// If the capability supports fuchsia.io rights attenuation,
    /// requests to access an attenuated capability with the specified rights.
    /// Otherwise, the request should be rejected with an unsupported error.
    pub rights: Option<fio::OpenFlags>,

    /// If the capability supports path-style child object access and attenuation,
    /// requests to access into that path. Otherwise, the request should be rejected
    /// with an unsupported error.
    pub relative_path: Path,

    /// The moniker of the requesting component.
    pub target_moniker: Moniker,

    /// The minimal availability strength of the capability demanded by the requestor.
    pub availability: Availability,
}

/// A path type that supports efficient prepending and appending.
#[derive(Default, Debug, Clone)]
pub struct Path {
    pub segments: VecDeque<String>,
}

impl Path {
    pub fn new(path: &str) -> Path {
        Path { segments: path.split("/").map(|s| s.to_owned()).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn next(&mut self) -> Option<String> {
        self.segments.pop_front()
    }

    pub fn prepend(&mut self, segment: String) {
        self.segments.push_front(segment);
    }

    pub fn append(&mut self, segment: String) {
        self.segments.push_back(segment);
    }

    /// Returns a path that will be valid for using in a `fuchsia.io/Directory.Open` operation.
    pub fn fuchsia_io_path(&self) -> String {
        if self.is_empty() {
            ".".to_owned()
        } else {
            self.segments.iter().map(String::as_str).collect::<Vec<&str>>().join("/")
        }
    }
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router").field("route_fn", &"[route function]").finish()
    }
}

impl Capability for Router {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!("some FIDL protocol for routing")
    }

    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }
}

impl Router {
    /// Creates a router that calls the provided function to route a request.
    pub fn new<F>(route_fn: F) -> Self
    where
        F: Fn(Request, Completer) -> () + Send + Sync + 'static,
    {
        Router { route_fn: Arc::new(route_fn) }
    }

    /// Creates a router that will always fail a request with the provided error.
    pub fn new_error(error: anyhow::Error) -> Self {
        let error = Arc::new(error);
        Router {
            route_fn: Arc::new(move |_request, completer| {
                completer.complete(Err(anyhow!("router cannot be created: {}", error.clone())));
            }),
        }
    }

    pub fn route(&self, request: Request, completer: Completer) {
        (self.route_fn)(request, completer)
    }

    /// Returns a router that routes to the capability under `name`. This attenuates the
    /// router capability from anything that could be obtained from the base router to
    /// the set of capabilities located under `name`.
    pub fn get(self, name: String) -> Router {
        let route_fn = move |mut request: Request, completer: Completer| {
            request.relative_path.prepend(name.clone());
            self.route(request, completer);
        };
        Router::new(route_fn)
    }

    /// Returns a router that ensures the capability request has an availability
    /// strength that is at least the provided `availability`.
    pub fn availability(self, availability: Availability) -> Router {
        let route_fn = move |mut request: Request, completer: Completer| {
            // The availability of the request must be compatible with the
            // availability of this step of the route.
            let mut state = routing::availability::AvailabilityState(request.availability);
            match state.advance(&availability) {
                Ok(()) => {
                    request.availability = state.0;
                    // Everything checks out, forward the request.
                    self.route(request, completer);
                }
                Err(e) => completer.complete(Err(e.into())),
            }
        };
        Router::new(route_fn)
    }
}

impl From<&AnyCapability> for Router {
    fn from(capability: &AnyCapability) -> Self {
        let output: Result<&dyn AsRouter, _> = crate::try_as_trait!(AsRouter, capability);
        let capability = match capability.try_clone() {
            Ok(capability) => capability,
            Err(error) => return Router::new_error(error.into()),
        };
        match output {
            Ok(output) => output.as_router(),
            Err(_) => Router::new(move |request, completer| {
                if request.relative_path.is_empty() {
                    completer.complete(Ok(capability.try_clone().unwrap()));
                } else {
                    completer.complete(Err(anyhow!("the capability does not support AsRouter")))
                }
            }),
        }
    }
}

/// The completer pattern avoids boxing futures at each router in the chain.
/// Instead of building a chain of boxed futures during recursive route calls,
/// each router can pass the completer buck to the next router.
pub struct Completer {
    sender: oneshot::Sender<Result<AnyCapability>>,
}

impl Completer {
    pub fn complete(self, result: Result<AnyCapability>) {
        let _ = self.sender.send(result);
    }

    pub fn new() -> (impl Future<Output = Result<AnyCapability>>, Self) {
        let (sender, receiver) = oneshot::channel();
        let fut = async move {
            let result: Result<_, Canceled> = receiver.await;
            let capability = result.context("routing request was abandoned by a router")??;
            Ok(capability)
        };
        (fut, Completer { sender })
    }
}

/// Types implementing this trait can provide capabilities on-demand by vending out
/// [`Router`]s which let the holder asynchronously request capabilities from them.
/// Examples: programs, components, dictionaries.
///
/// The returned [`Router`] may outlive the provider, in which case requests might be
/// dropped with an appropriate error.
pub trait AsRouter {
    fn as_router(&self) -> Router;
}

impl AsRouter for Router {
    fn as_router(&self) -> Router {
        self.clone()
    }
}

/// Generic routing function.
pub async fn route(router: &Router, request: Request) -> Result<AnyCapability> {
    let (fut, completer) = Completer::new();
    router.route(request, completer);
    fut.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Data;
    use assert_matches::assert_matches;

    #[fuchsia::test]
    async fn availability_good() {
        let source = Box::new(Data::new("hello")) as AnyCapability;
        let base = Router::from(&source);
        let proxy = base.availability(Availability::Optional);
        let capability = route(
            &proxy,
            Request {
                rights: None,
                relative_path: Path::default(),
                target_moniker: Moniker::default(),
                availability: Availability::Optional,
            },
        )
        .await
        .unwrap();
        let capability: Data<&str> = capability.try_into().unwrap();
        assert_eq!(capability.value, "hello");
    }

    #[fuchsia::test]
    async fn availability_bad() {
        let source = Box::new(Data::new("hello")) as AnyCapability;
        let base = Router::from(&source);
        let proxy = base.availability(Availability::Optional);
        let error = route(
            &proxy,
            Request {
                rights: None,
                relative_path: Path::default(),
                target_moniker: Moniker::default(),
                availability: Availability::Required,
            },
        )
        .await
        .unwrap_err();
        use routing::error::AvailabilityRoutingError;
        let error = error.downcast_ref::<AvailabilityRoutingError>().unwrap();
        assert_matches!(error, AvailabilityRoutingError::TargetHasStrongerAvailability);
    }
}
