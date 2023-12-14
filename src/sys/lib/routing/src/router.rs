// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use cm_types::Availability;
use fidl::epitaph::ChannelEpitaphExt;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use futures::{
    channel::{
        mpsc::UnboundedSender,
        oneshot::{self, Canceled},
    },
    Future,
};
use sandbox::{AnyCapability, Capability, Dict, Open, Path};
use std::{fmt, sync::Arc};
use vfs::execution_scope::ExecutionScope;

use crate::{
    capability_source::CapabilitySource,
    component_instance::{AnyWeakComponentInstance, ComponentInstanceInterface},
    policy::{GlobalPolicyChecker, PolicyCheckRouter},
};

/// Types that implement [`Routable`] let the holder asynchronously request
/// capabilities from them.
pub trait Routable {
    fn route(&self, request: Request, completer: Completer);
}

/// A [`Router`] is a capability that lets the holder obtain other capabilities
/// asynchronously. [`Router`] is the object capability representation of [`Routable`].
///
/// During routing, a request usually traverses through the component topology,
/// passing through several routers, ending up at some router that will fulfill
/// the completer instead of forwarding it upstream.
#[derive(Capability, Clone)]
pub struct Router {
    route_fn: Arc<RouteFn>,
}

/// [`RouteFn`] encapsulates arbitrary logic to fulfill a request to asynchronously
/// obtain a capability.
pub type RouteFn = dyn Fn(Request, Completer) -> () + Send + Sync;

/// [`Request`] contains metadata around how to obtain a capability.
#[derive(Debug, Clone)]
pub struct Request {
    /// If the capability supports fuchsia.io rights attenuation,
    /// requests to access an attenuated capability with the specified rights.
    /// Otherwise, the request should be rejected with an unsupported error.
    pub rights: Option<fio::OpenFlags>,

    /// If the capability supports path-style child object access and attenuation,
    /// requests to access into that path. Otherwise, the request should be rejected
    /// with an unsupported error.
    pub relative_path: Path,

    /// The minimal availability strength of the capability demanded by the requestor.
    pub availability: Availability,

    /// A reference to the requesting component.
    pub target: AnyWeakComponentInstance,
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router").field("route_fn", &"[route function]").finish()
    }
}

// TODO(b/314343346): Complete or remove the Router implementation of sandbox::Capability
impl Capability for Router {}

/// If `T` is [`Routable`], then a `Weak<T>` is also [`Routable`], except the request
/// may fail if the weak pointer has expired.
impl<T> Routable for std::sync::Weak<T>
where
    T: Routable + Send + Sync + 'static,
{
    fn route(&self, request: Request, completer: Completer) {
        match self.upgrade() {
            Some(routable) => {
                routable.route(request, completer);
            }
            None => completer.complete(Err(anyhow!("object was destroyed"))),
        }
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

    /// Package a [`Routable`] object into a [`Router`].
    pub fn from_routable<T: Routable + Send + Sync + 'static>(routable: T) -> Router {
        let route_fn = move |request, completer| {
            routable.route(request, completer);
        };
        Router::new(route_fn)
    }

    /// Obtain a capability from this router, following the description in `request`.
    pub fn route(&self, request: Request, completer: Completer) {
        (self.route_fn)(request, completer)
    }

    /// Returns a router that routes to the capability under `name`. This attenuates the
    /// router capability from anything that could be obtained from the base router to
    /// the set of capabilities located under `name`.
    pub fn with_name(self, name: impl Into<String>) -> Router {
        let name: String = name.into();
        let route_fn = move |mut request: Request, completer: Completer| {
            request.relative_path.prepend(name.clone());
            self.route(request, completer);
        };
        Router::new(route_fn)
    }

    /// Returns a router that ensures the capability request has an availability
    /// strength that is at least the provided `availability`.
    pub fn with_availability(self, availability: Availability) -> Router {
        let route_fn = move |mut request: Request, completer: Completer| {
            // The availability of the request must be compatible with the
            // availability of this step of the route.
            let mut state = crate::availability::AvailabilityState(request.availability);
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

    /// Returns a router that ensures the capability request is allowed by the
    /// policy in [`GlobalPolicyChecker`].
    pub fn with_policy_check<C>(
        self,
        capability_source: CapabilitySource<C>,
        policy_checker: GlobalPolicyChecker,
    ) -> Self
    where
        C: ComponentInstanceInterface + 'static,
    {
        Router::from_routable(PolicyCheckRouter::new(capability_source, policy_checker, self))
    }

    /// Converts the [Router] capability into an [Open] capability such that open requests
    /// will be fulfilled via the specified `request` on the router.
    ///
    /// `entry_type` is the type of the entry when the `Open` is accessed through a `fuchsia.io`
    /// connection.
    ///
    /// When routing failed while exercising the returned [Open] capability, errors will be
    /// sent to `errors`.
    pub fn into_open(
        self,
        request: Request,
        entry_type: fio::DirentType,
        errors: UnboundedSender<anyhow::Error>,
    ) -> Open {
        let router = self.clone();
        let errors = Arc::new(errors);
        Open::new(
            move |scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
                  server_end: zx::Channel| {
                let request = request.clone();
                let router = router.clone();
                let scope_clone = scope.clone();
                let errors = errors.clone();
                scope.spawn(async move {
                    // Request a capability from the `router`.
                    let router = router;
                    let result = route(&router, request).await;
                    match result {
                        Ok(capability) => {
                            // Connect to the capability by converting it into `Open`, then open it.
                            let open: Result<Open, _> = capability.try_into();
                            match open {
                                Ok(open) => {
                                    open.open(scope_clone, flags, relative_path, server_end);
                                }
                                Err(error) => {
                                    // Capabilities that don't support opening, such as a Handle
                                    // capability, can fail and get logged here. We can also model
                                    // a Void source this way.
                                    let _ = errors.unbounded_send(error.into());
                                    let _ =
                                        server_end.close_with_epitaph(zx::Status::NOT_SUPPORTED);
                                }
                            }
                        }
                        Err(error) => {
                            // Routing failed (e.g. broken route).
                            let _ = errors.unbounded_send(error);
                            let _ = server_end.close_with_epitaph(zx::Status::UNAVAILABLE);
                        }
                    }
                })
            },
            entry_type,
        )
    }
}

impl From<Router> for fsandbox::Capability {
    fn from(_router: Router) -> Self {
        unimplemented!("TODO(b/314343346): Complete or remove the Router implementation of sandbox::Capability")
    }
}

impl Routable for AnyCapability {
    fn route(&self, request: Request, completer: Completer) {
        let capability = self.clone();
        match try_get_routable(self) {
            Ok(router) => router.route(request, completer),
            Err(_) => {
                if request.relative_path.is_empty() {
                    completer.complete(Ok(capability.clone()));
                } else {
                    completer.complete(Err(anyhow!("the capability does not support routing")))
                }
            }
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

/// If the capability implements [`Routable`], then get a reference to that trait. Otherwise fail.
fn try_get_routable<'a>(capability: &'a AnyCapability) -> Result<&'a dyn Routable, ()> {
    // These are all the capability types that we know implements `Routable`.
    // This approach will need to be revised once we have capability types
    // outside of the sandbox and routing crates.
    let result: Result<&Dict, _> = capability.try_into();
    if let Ok(dict) = result {
        return Ok(dict);
    }
    let result: Result<&Open, _> = capability.try_into();
    if let Ok(open) = result {
        return Ok(open);
    }
    let result: Result<&Router, _> = capability.try_into();
    if let Ok(router) = result {
        return Ok(router);
    }
    Err(())
}

/// Dictionary supports routing requests:
/// - Check if path is empty, then resolve the completer with the current object.
/// - If not, see if there's a entry corresponding to the next path segment, and
///   - Delegate the rest of the request to that entry.
///   - If no entry found, close the completer with an error.
impl Routable for Dict {
    fn route(&self, mut request: Request, completer: Completer) {
        let Some(name) = request.relative_path.next() else {
            completer.complete(Ok(Box::new(self.clone())));
            return;
        };
        let entries = self.lock_entries();
        let Some(capability) = entries.get(&name) else {
            completer.complete(Err(anyhow!("item {} is not present in dictionary", name)));
            return;
        };
        capability.route(request, completer);
    }
}

impl Routable for Open {
    /// Each request from the router will yield an [`Open`]  with rights downscoped to
    /// `request.rights` and paths relative to `request.relative_path`.
    fn route(&self, request: Request, completer: Completer) {
        let mut open = self.clone();
        if !request.relative_path.is_empty() {
            open = open.downscope_path(request.relative_path);
        }
        if let Some(rights) = request.rights {
            open = open.downscope_rights(rights)
        }
        completer.complete(Ok(Box::new(open) as AnyCapability));
    }
}

impl Routable for Router {
    fn route(&self, request: Request, completer: Completer) {
        self.route(request, completer);
    }
}

/// Obtain a capability from `router`, following the description in `request`.
pub async fn route(router: &Router, request: Request) -> Result<AnyCapability> {
    let (fut, completer) = Completer::new();
    router.route(request, completer);
    fut.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use sandbox::{Data, Message, Receiver, Sender};

    #[fuchsia::test]
    async fn availability_good() {
        let source: AnyCapability = Box::new(Data::String("hello".to_string()));
        let base = Router::from_routable(source);
        let proxy = base.with_availability(Availability::Optional);
        let capability = route(
            &proxy,
            Request {
                rights: None,
                relative_path: Path::default(),
                availability: Availability::Optional,
                target: AnyWeakComponentInstance::invalid_for_tests(),
            },
        )
        .await
        .unwrap();
        let capability: Data = capability.try_into().unwrap();
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn availability_bad() {
        let source: AnyCapability = Box::new(Data::String("hello".to_string()));
        let base = Router::from_routable(source);
        let proxy = base.with_availability(Availability::Optional);
        let error = route(
            &proxy,
            Request {
                rights: None,
                relative_path: Path::default(),
                availability: Availability::Required,
                target: AnyWeakComponentInstance::invalid_for_tests(),
            },
        )
        .await
        .unwrap_err();
        use crate::error::AvailabilityRoutingError;
        let error = error.downcast_ref::<AvailabilityRoutingError>().unwrap();
        assert_matches!(error, AvailabilityRoutingError::TargetHasStrongerAvailability);
    }

    #[fuchsia::test]
    async fn route_and_use_sender_with_dropped_receiver() {
        // We want to test vending a sender with a router, dropping the associated receiver, and
        // then using the sender. The objective is to observe an error, and not panic.
        let receiver: Receiver<()> = Receiver::new();
        let sender = receiver.new_sender();
        let router = Router::new(move |_request, completer| {
            completer.complete(Ok(Box::new(sender.clone())))
        });

        let capability = route(
            &router,
            Request {
                rights: None,
                relative_path: Path::default(),
                availability: Availability::Required,
                target: AnyWeakComponentInstance::invalid_for_tests(),
            },
        )
        .await
        .unwrap();
        let sender: Sender<()> = capability.try_into().unwrap();

        drop(receiver);
        let (ch1, _ch2) = zx::Channel::create();
        assert!(sender
            .send(Message {
                payload: fsandbox::ProtocolPayload { channel: ch1, flags: fio::OpenFlags::empty() },
                target: (),
            })
            .is_err());
    }
}
