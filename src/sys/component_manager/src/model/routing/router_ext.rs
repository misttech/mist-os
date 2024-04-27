// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability::CapabilitySource;
use crate::model::component::ComponentInstance;
use crate::model::component::WeakComponentInstance;
use ::routing::{error::RoutingError, policy::GlobalPolicyChecker};
use async_trait::async_trait;
use bedrock_error::{BedrockError, Explain};
use cm_types::Availability;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use futures::future::BoxFuture;
use futures::FutureExt;
use routing::error::ComponentInstanceError;
use sandbox::Capability;
use sandbox::Dict;
use sandbox::Open;
use sandbox::Request;
use sandbox::Routable;
use sandbox::Router;
use sandbox::WeakComponentToken;
use std::sync::Arc;
use vfs::directory::entry::{self, DirectoryEntry, DirectoryEntryAsync, EntryInfo};
use vfs::execution_scope::ExecutionScope;

/// A trait to add functions to Router that know about the component manager
/// types.
pub trait RouterExt: Send + Sync {
    /// Returns a router that ensures the capability request is allowed by the
    /// policy in [`GlobalPolicyChecker`].
    fn with_policy_check(
        self,
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
    ) -> Self;

    /// Returns a router that ensures the capability request has an availability
    /// strength that is at least the provided `availability`.
    fn with_availability(self, availability: Availability) -> Router;

    /// Returns a [Dict] equivalent to `dict`, but with all [Router]s replaced with [Open].
    ///
    /// This is an alternative to [Dict::try_into_open] when the [Dict] contains [Router]s, since
    /// [Router] is not currently a type defined by the sandbox library.
    fn dict_routers_to_open(
        weak_component: &WeakComponentToken,
        scope: &ExecutionScope,
        dict: &Dict,
    ) -> Dict;

    /// Converts the [Router] capability into DirectoryEntry such that open requests
    /// will be fulfilled via the specified `request` on the router.
    ///
    /// `entry_type` is the type of the entry when the DirectoryEntry is accessed through a `fuchsia.io`
    /// connection.
    ///
    /// Routing and open tasks are spawned on `scope`.
    ///
    /// When routing failed while exercising the returned DirectoryEntry, errors will be
    /// sent to `errors_fn`.
    fn into_directory_entry<F>(
        self,
        request: Request,
        entry_type: fio::DirentType,
        scope: ExecutionScope,
        errors_fn: F,
    ) -> Arc<dyn DirectoryEntry>
    where
        for<'a> F: Fn(&'a BedrockError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static;
}

impl RouterExt for Router {
    fn with_policy_check(
        self,
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
    ) -> Self {
        Router::new(PolicyCheckRouter::new(capability_source, policy_checker, self))
    }

    fn with_availability(self, availability: Availability) -> Router {
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

    fn dict_routers_to_open(
        weak_component: &WeakComponentToken,
        scope: &ExecutionScope,
        dict: &Dict,
    ) -> Dict {
        let entries = dict.lock_entries();
        let out = Dict::new();
        let mut out_entries = out.lock_entries();
        for (key, value) in entries.iter() {
            let value = match value {
                Capability::Dictionary(dict) => {
                    Capability::Dictionary(Self::dict_routers_to_open(weak_component, scope, dict))
                }
                Capability::Router(r) => {
                    let router = r.clone();
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
                        scope.clone(),
                        |_| None,
                    )))
                }
                other => other.clone(),
            };
            out_entries.insert(key.clone(), value.clone()).ok();
        }
        drop(out_entries);
        out
    }

    fn into_directory_entry<F>(
        self,
        request: Request,
        entry_type: fio::DirentType,
        scope: ExecutionScope,
        errors_fn: F,
    ) -> Arc<dyn DirectoryEntry>
    where
        for<'a> F: Fn(&'a BedrockError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
    {
        struct RouterEntry<F> {
            router: Router,
            request: Request,
            entry_type: fio::DirentType,
            scope: ExecutionScope,
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
                request.set_scope(self.scope.clone());
                request.spawn(self);
                Ok(())
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
                                Router::dict_routers_to_open(&self.request.target, &self.scope, &d)
                                    .into()
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

        Arc::new(RouterEntry { router: self.clone(), request, entry_type, scope, errors_fn })
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
            .can_route_capability(&self.capability_source, &request.target.moniker())
        {
            Ok(()) => self.router.route(request).await,
            Err(policy_error) => Err(RoutingError::PolicyError(policy_error).into()),
        }
    }
}

/// A trait to add functions WeakComponentInstancethat know about the component
/// manager types.
pub trait WeakComponentTokenExt {
    /// Create a new token.
    fn new(instance: WeakComponentInstance) -> WeakComponentToken;

    /// Upgrade this token to the underlying instance.
    fn to_instance(self) -> WeakComponentInstance;

    /// Get a reference to the underlying instance.
    fn as_ref(&self) -> &WeakComponentInstance;

    /// Get a strong reference to the underlying instance.
    fn upgrade(&self) -> Result<Arc<ComponentInstance>, ComponentInstanceError>;

    /// Get the moniker for this component.
    fn moniker(&self) -> moniker::Moniker;

    #[cfg(test)]
    fn invalid() -> WeakComponentToken {
        WeakComponentToken::new(WeakComponentInstance::invalid())
    }
}

// We need this extra struct because WeakComponentInstance isn't defined in this
// crate so we can't implement WeakComponentTokenAny for it.
#[derive(Debug)]
struct WeakComponentInstanceExt {
    inner: WeakComponentInstance,
}
impl sandbox::WeakComponentTokenAny for WeakComponentInstanceExt {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl WeakComponentTokenExt for WeakComponentToken {
    fn new(instance: WeakComponentInstance) -> WeakComponentToken {
        WeakComponentToken { inner: Arc::new(WeakComponentInstanceExt { inner: instance }) }
    }

    fn to_instance(self) -> WeakComponentInstance {
        self.as_ref().clone()
    }

    fn as_ref(&self) -> &WeakComponentInstance {
        match self.inner.as_any().downcast_ref::<WeakComponentInstanceExt>() {
            Some(instance) => &instance.inner,
            None => panic!(),
        }
    }

    fn upgrade(&self) -> Result<Arc<ComponentInstance>, ComponentInstanceError> {
        self.as_ref().upgrade()
    }

    fn moniker(&self) -> moniker::Moniker {
        self.as_ref().moniker.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use bedrock_error::DowncastErrorForTest;
    use sandbox::Data;

    #[derive(Debug)]
    struct FakeComponentToken {}

    impl FakeComponentToken {
        fn new() -> WeakComponentToken {
            WeakComponentToken { inner: Arc::new(FakeComponentToken {}) }
        }
    }

    impl sandbox::WeakComponentTokenAny for FakeComponentToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[fuchsia::test]
    async fn availability_good() {
        let source: Capability = Data::String("hello".to_string()).into();
        let base = Router::new(source);
        let proxy = base.with_availability(Availability::Optional);
        let capability = proxy
            .route(Request {
                availability: Availability::Optional,
                target: FakeComponentToken::new(),
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
                target: FakeComponentToken::new(),
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
}
