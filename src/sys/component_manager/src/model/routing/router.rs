// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::WeakComponentInstance;
use crate::{capability::CapabilitySource, sandbox_util::DictExt};
use ::routing::{error::RoutingError, policy::GlobalPolicyChecker};
use async_trait::async_trait;
use bedrock_error::{BedrockError, Explain};
use cm_types::{Availability, IterablePath, RelativePath};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use futures::future::BoxFuture;
use futures::FutureExt;
use sandbox::{AnyCapability, Capability, CapabilityTrait, Dict, Open};
use std::{fmt, sync::Arc};
use vfs::directory::entry::{self, DirectoryEntry, DirectoryEntryAsync, EntryInfo};

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

    /// Returns an router that requests capabilities from the specified `path` relative
    /// to the base router, i.e. attenuates the base router to the subset of capabilities
    /// that live under `path`.
    pub fn with_path(self, path: RelativePath) -> Router {
        if path.is_dot() {
            return self;
        }
        let route_fn = move |request: Request| {
            let router = self.clone();
            let path = path.clone();
            async move {
                match router.route(request.clone()).await? {
                    Capability::Dictionary(dict) => {
                        match dict.get_with_request(&path, request.clone()).await? {
                            Some(cap) => cap.route(request).await,
                            None => Err(RoutingError::BedrockNotPresentInDictionary {
                                name: path
                                    .iter_segments()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join("/"),
                            }
                            .into()),
                        }
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
    pub fn dict_routers_to_open(weak_component: &WeakComponentInstance, dict: &Dict) -> Dict {
        let entries = dict.lock_entries();
        let out = Dict::new();
        let mut out_entries = out.lock_entries();
        for (key, value) in &*entries {
            let value = match value {
                Capability::Dictionary(dict) => {
                    Capability::Dictionary(Self::dict_routers_to_open(weak_component, dict))
                }
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
                    Capability::Open(Open::new(router.into_directory_entry(
                        request,
                        fio::DirentType::Service,
                        |_| None,
                    )))
                }
                other => other.clone(),
            };
            out_entries.insert(key.clone(), value);
        }
        drop(out_entries);
        out
    }

    /// Converts the [Router] capability into DirectoryEntry such that open requests
    /// will be fulfilled via the specified `request` on the router.
    ///
    /// `entry_type` is the type of the entry when the DirectoryEntry is accessed through a `fuchsia.io`
    /// connection.
    ///
    /// Tasks are spawned on the component's execution scope.
    ///
    /// When routing failed while exercising the returned DirectoryEntry, errors will be
    /// sent to `errors_fn`.
    pub fn into_directory_entry<F>(
        self,
        request: Request,
        entry_type: fio::DirentType,
        errors_fn: F,
    ) -> Arc<dyn DirectoryEntry>
    where
        for<'a> F: Fn(&'a BedrockError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
    {
        struct RouterEntry<F> {
            router: Router,
            request: Request,
            entry_type: fio::DirentType,
            errors_fn: F,
        }

        impl<F> DirectoryEntry for RouterEntry<F>
        where
            for<'a> F: Fn(&'a BedrockError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
        {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
            }

            fn open_entry(
                self: Arc<Self>,
                mut request: entry::OpenRequest<'_>,
            ) -> Result<(), zx::Status> {
                if let Ok(target) = self.request.target.upgrade() {
                    // Spawn this request on the component's execution scope so that it doesn't
                    // block the namespace.
                    request.set_scope(target.execution_scope.clone());
                    request.spawn(self);
                    Ok(())
                } else {
                    Err(zx::Status::NOT_FOUND)
                }
            }
        }

        impl<F> DirectoryEntryAsync for RouterEntry<F>
        where
            for<'a> F: Fn(&'a BedrockError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
        {
            async fn open_entry_async(
                self: Arc<Self>,
                open_request: entry::OpenRequest<'_>,
            ) -> Result<(), zx::Status> {
                // Hold a guard to prevent this task from being dropped during component
                // destruction.  This task is tied to the target component.
                let _guard = open_request.scope().active_guard();

                // Request a capability from the `router`.
                let result = self.router.route(self.request.clone()).await;
                let error = match result {
                    Ok(capability) => {
                        // HACK: Dict needs special casing because [Dict::try_into_open]
                        // is unaware of [Router].
                        let capability = match capability {
                            Capability::Dictionary(d) => {
                                Router::dict_routers_to_open(&self.request.target, &d).into()
                            }
                            cap => cap,
                        };
                        match super::capability_into_open(capability.clone()) {
                            Ok(open) => return open.open_entry(open_request),
                            Err(error) => error,
                        }
                    }
                    Err(error) => error, // Routing failed (e.g. broken route).
                };
                if let Some(fut) = (self.errors_fn)(&error) {
                    fut.await;
                }
                Err(error.as_zx_status())
            }
        }

        Arc::new(RouterEntry { router: self.clone(), request, entry_type, errors_fn })
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
        assert!(sender.send(Message { channel: ch1 }).is_err());
    }

    #[fuchsia::test]
    async fn with_path() {
        let source = Capability::Data(Data::String("hello".to_string()));
        let dict1 = Dict::new();
        dict1.lock_entries().insert("source".parse().unwrap(), source);

        let base_router = Router::new_ok(dict1);
        let downscoped_router = base_router.with_path(RelativePath::new("source").unwrap());

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
        dict1.lock_entries().insert("source".parse().unwrap(), source);
        let dict2 = Dict::new();
        dict2.lock_entries().insert("dict1".parse().unwrap(), Capability::Dictionary(dict1));
        let dict3 = Dict::new();
        dict3.lock_entries().insert("dict2".parse().unwrap(), Capability::Dictionary(dict2));
        let dict4 = Dict::new();
        dict4.lock_entries().insert("dict3".parse().unwrap(), Capability::Dictionary(dict3));

        let base_router = Router::new_ok(dict4);
        let downscoped_router =
            base_router.with_path(RelativePath::new("dict3/dict2/dict1/source").unwrap());

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
