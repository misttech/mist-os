// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilitySource;
use crate::model::component::WeakComponentInstance;
use crate::sandbox_util::walk_dict_resolve_routers;
use ::routing::{error::RoutingError, policy::GlobalPolicyChecker};
use async_trait::async_trait;
use bedrock_error::{BedrockError, Explain};
use cm_types::Availability;
use cm_util::TaskGroup;
use fidl::epitaph::ChannelEpitaphExt;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use futures::future::BoxFuture;
use futures::FutureExt;
use sandbox::{AnyCapability, Capability, CapabilityTrait, Dict, Open, Path};
use std::{fmt, sync::Arc};
use vfs::execution_scope::ExecutionScope;
use zx::HandleBased;

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
    pub target: WeakComponentInstance,
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

    /// Returns an router that requests capabilities from the specified `path` relative
    /// to the base router, i.e. attenuates the base router to the subset of capabilities
    /// that live under `path`.
    pub fn with_path<'a>(self, path: impl DoubleEndedIterator<Item = &'a str>) -> Router {
        let segments: Vec<_> = path.map(|s| s.to_string()).collect();
        if segments.is_empty() {
            return self;
        }
        let route_fn = move |request: Request| {
            let router = self.clone();
            let segments = segments.clone();
            async move {
                match router.route(request.clone()).await? {
                    Capability::Dictionary(dict) => {
                        match walk_dict_resolve_routers(&dict, segments.clone(), request.clone())
                            .await
                        {
                            Some(cap) => cap.route(request).await,
                            None => {
                                return Err(RoutingError::BedrockNotPresentInDictionary {
                                    name: segments.join("/"),
                                }
                                .into());
                            }
                        }
                    }
                    Capability::Open(open) => {
                        Ok(open.downscope_path(Path::new(&segments.join("/")).into()).into())
                    }
                    _ => Err(RoutingError::BedrockUnsupportedCapability.into()),
                }
            }
            .boxed()
        };
        Router::new(route_fn)
    }

    /// Returns a router that ensures the capability request has an availability
    /// strength that is at least the provided `availability`.
    pub fn with_availability(self, availability: Availability) -> Router {
        let route_fn = move |mut request: Request| {
            let router = self.clone();
            async move {
                // The availability of the request must be compatible with the
                // availability of this step of the route.
                match ::routing::availability::advance(request.availability, availability) {
                    Ok(updated) => {
                        request.availability = updated;
                        // Everything checks out, forward the request.
                        let res = router.route(request).await;
                        res
                    }
                    Err(e) => Err(RoutingError::from(e).into()),
                }
            }
            .boxed()
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
        Router::new(PolicyCheckRouter::new(capability_source, policy_checker, self))
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
                    let result = router.route(request).await;
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
                            match super::capability_into_open(capability.clone()) {
                                Ok(open) => open.open(scope, flags, relative_path, server_end),
                                Err(error) => errors_fn(ErrorCapsule {
                                    error: error.into(),
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
    error: BedrockError,
    open_request: OpenRequest,
}

impl ErrorCapsule {
    /// Destructures the [`ErrorCapsule`] into the error and the open request
    /// sent by the client when they connect to the requested capability. It is
    /// provided here such that the endpoint may be closed with an appropriate
    /// epitaph, which is now the responsibility of the caller.
    pub fn manually_handle(mut self) -> (BedrockError, OpenRequest) {
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
impl Routable for BedrockError {
    async fn route(&self, _: Request) -> Result<Capability, BedrockError> {
        Err(self.clone())
    }
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

#[async_trait]
impl Routable for PolicyCheckRouter {
    async fn route(&self, request: Request) -> Result<Capability, BedrockError> {
        match self
            .policy_checker
            .can_route_capability(&self.capability_source, &request.target.moniker)
        {
            Ok(()) => self.router.route(request).await,
            Err(policy_error) => Err(RoutingError::PolicyError(policy_error).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use bedrock_error::DowncastErrorForTest;
    use sandbox::{Data, Message, Receiver};
    use std::iter;

    #[fuchsia::test]
    async fn availability_good() {
        let source: Capability = Data::String("hello".to_string()).into();
        let base = Router::new(source);
        let proxy = base.with_availability(Availability::Optional);
        let capability = proxy
            .route(Request {
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            })
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
        let base = Router::new(source);
        let proxy = base.with_availability(Availability::Optional);
        let error = proxy
            .route(Request {
                availability: Availability::Required,
                target: WeakComponentInstance::invalid(),
            })
            .await
            .unwrap_err();
        use ::routing::error::AvailabilityRoutingError;
        assert_matches!(
            error,
            BedrockError::RoutingError(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::AvailabilityRoutingError(
                    AvailabilityRoutingError::TargetHasStrongerAvailability
                )
            )
        );
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
                target: WeakComponentInstance::invalid(),
            })
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

        let base_router = Router::new_ok(dict1);
        let downscoped_router = base_router.with_path(iter::once("source"));

        let capability = downscoped_router
            .route(Request {
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            })
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

        let base_router = Router::new_ok(dict4);
        let downscoped_router =
            base_router.with_path(vec!["dict3", "dict2", "dict1", "source"].into_iter());

        let capability = downscoped_router
            .route(Request {
                availability: Availability::Optional,
                target: WeakComponentInstance::invalid(),
            })
            .await
            .unwrap();
        let capability = match capability {
            Capability::Data(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }
}
