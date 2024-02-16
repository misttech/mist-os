// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilitySource;
use crate::model::{component::WeakComponentInstance, error::RouteOrOpenError};
use ::routing::{error::RoutingError, policy::GlobalPolicyChecker};
use cm_types::Availability;
use cm_util::TaskGroup;
use fidl::epitaph::ChannelEpitaphExt;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot::{self, Canceled},
    Future,
};
use sandbox::{AnyCapability, Capability, CapabilityTrait, Dict, Open, Path};
use std::{fmt, sync::Arc};
use vfs::execution_scope::ExecutionScope;
use zx::HandleBased;

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
#[derive(Clone)]
pub struct Router {
    route_fn: Arc<RouteFn>,
}

/// [`RouteFn`] encapsulates arbitrary logic to fulfill a request to asynchronously
/// obtain a capability.
pub type RouteFn = dyn Fn(Request, Completer) -> () + Send + Sync;

/// [`Request`] contains metadata around how to obtain a capability.
#[derive(Debug, Clone)]
pub struct Request {
    /// If the capability supports path-style child object access and attenuation,
    /// requests to access into that path. Otherwise, the request should be rejected
    /// with an unsupported error.
    pub relative_path: Path,

    /// The minimal availability strength of the capability demanded by the requestor.
    pub availability: Availability,

    /// A reference to the requesting component.
    pub target: WeakComponentInstance,
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router").field("route_fn", &"[route function]").finish()
    }
}

// TODO(b/314343346): Complete or remove the Router implementation of sandbox::Capability
impl CapabilityTrait for Router {}

impl From<Router> for Capability {
    fn from(router: Router) -> Self {
        Capability::Router(Box::new(router))
    }
}

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
            None => completer.complete(Err(RoutingError::BedrockObjectDestroyed.into())),
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
    pub fn new_error(error: RouteOrOpenError) -> Self {
        Router {
            route_fn: Arc::new(move |_request, completer| {
                completer.complete(Err(error.clone()));
            }),
        }
    }
    pub fn from_any(any: AnyCapability) -> Router {
        *any.into_any().downcast::<Router>().unwrap()
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

    /// Returns an router that requests capabilities from the specified `path` relative
    /// to the base router, i.e. attenuates the base router to the subset of capabilities
    /// that live under `path`.
    pub fn with_path<'a>(self, path: impl DoubleEndedIterator<Item = &'a str>) -> Router {
        let segments: Vec<_> = path.map(|s| s.to_string()).rev().collect();
        if segments.is_empty() {
            return self;
        }
        let route_fn = move |mut request: Request, completer: Completer| {
            for name in &segments {
                request.relative_path.prepend(name.clone());
            }
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
            let mut state = ::routing::availability::AvailabilityState(request.availability);
            match state.advance(&availability) {
                Ok(()) => {
                    request.availability = state.0;
                    // Everything checks out, forward the request.
                    self.route(request, completer);
                }
                Err(e) => completer.complete(Err(RoutingError::from(e).into())),
            }
        };
        Router::new(route_fn)
    }

    /// Returns a router that ensures the capability request is allowed by the
    /// policy in [`GlobalPolicyChecker`].
    pub fn with_policy_check(
        self,
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
    ) -> Self {
        Router::from_routable(PolicyCheckRouter::new(capability_source, policy_checker, self))
    }

    /// Returns a [Dict] equivalent to `dict`, but with all [Router]s replaced with [Open].
    ///
    /// This is an alternative to [Dict::try_into_open] when the [Dict] contains [Router]s, since
    /// [Router] is not currently a type defined by the sandbox library.
    pub fn dict_routers_to_open(
        weak_component: &WeakComponentInstance,
        dict: &Dict,
        routing_task_group: TaskGroup,
    ) -> Dict {
        let entries = dict.lock_entries();
        let out = Dict::new();
        let mut out_entries = out.lock_entries();
        for (key, value) in &*entries {
            let value = match value {
                Capability::Dictionary(dict) => Capability::Dictionary(Self::dict_routers_to_open(
                    weak_component,
                    dict,
                    routing_task_group.clone(),
                )),
                Capability::Router(r) => {
                    let router = Router::from_any(r.clone());
                    let request = Request {
                        relative_path: sandbox::Path::default(),
                        target: weak_component.clone(),
                        // Use the weakest availability, so that it gets immediately upgraded to
                        // the availability in `router`.
                        availability: cm_types::Availability::Transitional,
                    };
                    // TODO: Should we convert the Open to a Directory here if the Router wraps a
                    // Dict?
                    Capability::Open(router.into_open(
                        request,
                        fio::DirentType::Service,
                        routing_task_group.clone(),
                        |_| {},
                    ))
                }
                other => other.clone(),
            };
            out_entries.insert(key.clone(), value);
        }
        drop(out_entries);
        out
    }

    /// Converts the [Router] capability into an [Open] capability such that open requests
    /// will be fulfilled via the specified `request` on the router.
    ///
    /// `entry_type` is the type of the entry when the `Open` is accessed through a `fuchsia.io`
    /// connection.
    ///
    /// Routing tasks are run on the `routing_task_group`.
    ///
    /// When routing failed while exercising the returned [Open] capability, errors will be
    /// sent to `errors_fn`.
    pub fn into_open(
        self,
        request: Request,
        entry_type: fio::DirentType,
        routing_task_group: TaskGroup,
        errors_fn: impl Fn(ErrorCapsule) + Send + Sync + 'static,
    ) -> Open {
        let router = self.clone();
        let errors_fn = Arc::new(errors_fn);
        Open::new(
            move |scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
                  server_end: zx::Channel| {
                let request = request.clone();
                let target = request.target.clone();
                let router = router.clone();
                let routing_task_group_clone = routing_task_group.clone();
                let errors_fn = errors_fn.clone();
                routing_task_group.spawn(async move {
                    // Request a capability from the `router`.
                    let result = route(&router, request).await;
                    match result {
                        Ok(capability) => {
                            // HACK: Dict needs special casing because [Dict::try_into_open]
                            // is unaware of [Router].
                            let capability = match capability {
                                Capability::Dictionary(d) => Self::dict_routers_to_open(
                                    &target,
                                    &d,
                                    routing_task_group_clone,
                                )
                                .into(),
                                cap => cap,
                            };
                            match super::capability_into_open(capability) {
                                Ok(open) => open.open(scope, flags, relative_path, server_end),
                                Err(error) => errors_fn(ErrorCapsule {
                                    error,
                                    open_request: OpenRequest { flags, relative_path, server_end },
                                }),
                            }
                        }
                        Err(error) => {
                            // Routing failed (e.g. broken route).
                            errors_fn(ErrorCapsule {
                                error,
                                open_request: OpenRequest { flags, relative_path, server_end },
                            });
                        }
                    }
                });
            },
            entry_type,
        )
    }
}

/// [`ErrorCapsule `] holds an error from capability routing, and closes the
/// server endpoint in the open request with an appropriate epitaph on drop.
#[derive(Debug)]
pub struct ErrorCapsule {
    error: RouteOrOpenError,
    open_request: OpenRequest,
}

impl ErrorCapsule {
    /// Destructures the [`ErrorCapsule`] into the error and the open request
    /// sent by the client when they connect to the requested capability. It is
    /// provided here such that the endpoint may be closed with an appropriate
    /// epitaph, which is now the responsibility of the caller.
    pub fn manually_handle(mut self) -> (RouteOrOpenError, OpenRequest) {
        (self.error.clone(), self.open_request.take())
    }
}

impl fmt::Display for ErrorCapsule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl Drop for ErrorCapsule {
    fn drop(&mut self) {
        self.open_request.close(self.error.as_zx_status());
    }
}

#[derive(Debug)]
pub struct OpenRequest {
    pub flags: fio::OpenFlags,
    pub relative_path: vfs::path::Path,
    pub server_end: zx::Channel,
}

impl OpenRequest {
    fn take(&mut self) -> Self {
        OpenRequest {
            flags: self.flags.clone(),
            relative_path: self.relative_path.clone(),
            server_end: cm_util::channel::take_channel(&mut self.server_end),
        }
    }

    fn close(&mut self, status: zx::Status) {
        let server_end = cm_util::channel::take_channel(&mut self.server_end);
        if !server_end.is_invalid_handle() {
            let _ = server_end.close_with_epitaph(status);
        }
    }
}

impl From<Router> for fsandbox::Capability {
    fn from(_router: Router) -> Self {
        unimplemented!("TODO(b/314343346): Complete or remove the Router implementation of sandbox::Capability")
    }
}

/// The completer pattern avoids boxing futures at each router in the chain.
/// Instead of building a chain of boxed futures during recursive route calls,
/// each router can pass the completer buck to the next router.
pub struct Completer {
    sender: oneshot::Sender<Result<Capability, RouteOrOpenError>>,
}

impl Completer {
    pub fn complete(self, result: Result<Capability, RouteOrOpenError>) {
        let _ = self.sender.send(result);
    }

    pub fn new() -> (impl Future<Output = Result<Capability, RouteOrOpenError>>, Self) {
        let (sender, receiver) = oneshot::channel();
        let fut = async move {
            let result: Result<_, Canceled> = receiver.await;
            let capability = result.map_err(|_| RoutingError::BedrockRoutingRequestCanceled)??;
            Ok(capability)
        };
        (fut, Completer { sender })
    }
}

fn try_routable_from_enum(capability: Capability) -> Option<Router> {
    match capability {
        Capability::Dictionary(c) => Some(Router::from_routable(c)),
        Capability::Open(c) => Some(Router::from_routable(c)),
        Capability::Router(c) => Some(Router::from_any(c)),
        _ => None,
    }
}

impl Routable for Capability {
    fn route(&self, request: Request, completer: Completer) {
        match try_routable_from_enum(self.clone()) {
            Some(router) => router.route(request, completer),
            None => {
                if request.relative_path.is_empty() {
                    completer.complete(Ok(self.clone()));
                } else {
                    completer.complete(Err(RoutingError::BedrockUnsupportedCapability.into()))
                }
            }
        }
    }
}

/// Dictionary supports routing requests:
/// - Check if path is empty, then resolve the completer with the current object.
/// - If not, see if there's a entry corresponding to the next path segment, and
///   - Delegate the rest of the request to that entry.
///   - If no entry found, close the completer with an error.
impl Routable for Dict {
    fn route(&self, mut request: Request, completer: Completer) {
        let Some(name) = request.relative_path.next() else {
            completer.complete(Ok(self.clone().into()));
            return;
        };
        let entries = self.lock_entries();
        let Some(capability) = entries.get(&name) else {
            completer
                .complete(Err(
                    RoutingError::BedrockNotPresentInDictionary { name: name.into() }.into()
                ));
            return;
        };
        let capability = capability.clone();
        drop(entries);
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
        completer.complete(Ok(open.into()));
    }
}

impl Routable for Router {
    fn route(&self, request: Request, completer: Completer) {
        self.route(request, completer);
    }
}

/// Obtain a capability from `router`, following the description in `request`.
pub async fn route(router: &Router, request: Request) -> Result<Capability, RouteOrOpenError> {
    let (fut, completer) = Completer::new();
    router.route(request, completer);
    fut.await
}

pub struct PolicyCheckRouter {
    capability_source: CapabilitySource,
    policy_checker: GlobalPolicyChecker,
    router: Router,
}

impl PolicyCheckRouter {
    pub fn new(
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
        router: Router,
    ) -> Self {
        PolicyCheckRouter { capability_source, policy_checker, router }
    }
}

impl Routable for PolicyCheckRouter {
    fn route(&self, request: Request, completer: Completer) {
        match self
            .policy_checker
            .can_route_capability(&self.capability_source, &request.target.moniker)
        {
            Ok(()) => self.router.route(request, completer),
            Err(policy_error) => {
                completer.complete(Err(RoutingError::PolicyError(policy_error).into()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use sandbox::{Data, Message, Receiver};
    use std::iter;

    #[fuchsia::test]
    async fn availability_good() {
        let source: Capability = Data::String("hello".to_string()).into();
        let base = Router::from_routable(source);
        let proxy = base.with_availability(Availability::Optional);
        let capability = route(
            &proxy,
            Request {
                relative_path: Path::default(),
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            },
        )
        .await
        .unwrap();
        let capability = match capability {
            Capability::Data(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn availability_bad() {
        let source: Capability = Data::String("hello".to_string()).into();
        let base = Router::from_routable(source);
        let proxy = base.with_availability(Availability::Optional);
        let error = route(
            &proxy,
            Request {
                relative_path: Path::default(),
                availability: Availability::Required,
                target: WeakComponentInstance::invalid(),
            },
        )
        .await
        .unwrap_err();
        use ::routing::error::AvailabilityRoutingError;
        assert_matches!(
            error,
            RouteOrOpenError::RoutingError(RoutingError::AvailabilityRoutingError(
                AvailabilityRoutingError::TargetHasStrongerAvailability
            ))
        );
    }

    #[fuchsia::test]
    async fn route_and_use_sender_with_dropped_receiver() {
        // We want to test vending a sender with a router, dropping the associated receiver, and
        // then using the sender. The objective is to observe an error, and not panic.
        let (receiver, sender) = Receiver::new();
        let router =
            Router::new(move |_request, completer| completer.complete(Ok(sender.clone().into())));

        let capability = route(
            &router,
            Request {
                relative_path: Path::default(),
                availability: Availability::Required,
                target: WeakComponentInstance::invalid(),
            },
        )
        .await
        .unwrap();
        let sender = match capability {
            Capability::Sender(c) => c,
            c => panic!("Bad enum {:#?}", c),
        };

        drop(receiver);
        let (ch1, _ch2) = zx::Channel::create();
        assert!(sender
            .send(Message {
                payload: fsandbox::ProtocolPayload { channel: ch1, flags: fio::OpenFlags::empty() },
            })
            .is_err());
    }

    #[fuchsia::test]
    async fn with_path() {
        let source = Capability::Data(Data::String("hello".to_string()));
        let dict1 = Dict::new();
        dict1.lock_entries().insert("source".to_owned(), source);
        let dict2 = Dict::new();
        dict2.lock_entries().insert("dict1".to_owned(), Capability::Dictionary(dict1));

        let base_router = Router::from_routable(dict2);
        let downscoped_router = base_router.with_path(iter::once("dict1"));

        let capability = route(
            &downscoped_router,
            Request {
                relative_path: Path::new("source"),
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            },
        )
        .await
        .unwrap();
        let capability = match capability {
            Capability::Data(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn with_path_deep() {
        let source = Capability::Data(Data::String("hello".to_string()));
        let dict1 = Dict::new();
        dict1.lock_entries().insert("source".to_owned(), source);
        let dict2 = Dict::new();
        dict2.lock_entries().insert("dict1".to_owned(), Capability::Dictionary(dict1));
        let dict3 = Dict::new();
        dict3.lock_entries().insert("dict2".to_owned(), Capability::Dictionary(dict2));
        let dict4 = Dict::new();
        dict4.lock_entries().insert("dict3".to_owned(), Capability::Dictionary(dict3));

        let base_router = Router::from_routable(dict4);
        let downscoped_router = base_router.with_path(vec!["dict3", "dict2"].into_iter());

        let capability = route(
            &downscoped_router,
            Request {
                relative_path: Path::new("dict1/source"),
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            },
        )
        .await
        .unwrap();
        let capability = match capability {
            Capability::Data(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }
}
