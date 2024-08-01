// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability_source::CapabilitySource;
use crate::component_instance::{ComponentInstanceInterface, WeakExtendedInstanceInterface};
use crate::error::{ComponentInstanceError, RoutingError};
use crate::policy::GlobalPolicyChecker;
use async_trait::async_trait;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{Capability, Request, Routable, Router};

/// If the metadata for a route contains a Data::Uint64 value under this key with a value greater
/// than 0, then no policy checks will be performed. This behavior is limited to non-fuchsia
/// builds, and is exclusively used when performing routes from an offer declaration. This is
/// necessary because we don't know the ultimate target of the route, and thus routes that are
/// otherwise valid could fail due to policy checks.
///
/// Consider a policy that allows a component `/core/session_manager/session:session/my_cool_app`
/// to access `fuchsia.kernel.VmexResource`. If we attempt to validate that route from the offer
/// placed on `session_manager`, we'd have to fill in `session_manager` for the target of the route
/// in the route request and follow the route to its source from there. If this policy check were
/// applied on this route it would fail the route, as `session` manager is not allowed to access
/// `fuchsia.kernel.VmexResource`. The route is valid though, because the offer on
/// `session_manager` doesn't grant the session manager program access to the restricted
/// capability.
///
/// To be able to properly support this scenario, we need to selectively disable policy checks when
/// routing from offer declarations.
pub const SKIP_POLICY_CHECKS: &'static str = "skip_policy_checks";

pub trait WithPolicyCheck {
    /// Returns a router that ensures the capability request is allowed by the
    /// policy in [`GlobalPolicyChecker`].
    fn with_policy_check<C: ComponentInstanceInterface + 'static>(
        self,
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
    ) -> Self;
}

impl WithPolicyCheck for Router {
    fn with_policy_check<C: ComponentInstanceInterface + 'static>(
        self,
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
    ) -> Self {
        Router::new(PolicyCheckRouter::<C>::new(capability_source, policy_checker, self))
    }
}

pub struct PolicyCheckRouter<C: ComponentInstanceInterface + 'static> {
    capability_source: CapabilitySource,
    policy_checker: GlobalPolicyChecker,
    router: Router,
    _phantom_data: std::marker::PhantomData<C>,
}

impl<C: ComponentInstanceInterface + 'static> PolicyCheckRouter<C> {
    pub fn new(
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
        router: Router,
    ) -> Self {
        PolicyCheckRouter {
            capability_source,
            policy_checker,
            router,
            _phantom_data: std::marker::PhantomData::<C>,
        }
    }
}

#[async_trait]
impl<C: ComponentInstanceInterface + 'static> Routable for PolicyCheckRouter<C> {
    async fn route(&self, request: Request) -> Result<Capability, RouterError> {
        #[cfg(not(target_os = "fuchsia"))]
        if let Ok(Some(Capability::Data(sandbox::Data::Uint64(num)))) =
            request.metadata.get(&cm_types::Name::new(SKIP_POLICY_CHECKS).unwrap())
        {
            if num > 0 {
                return self.router.route(request).await;
            }
        }
        let target = request
            .target
            .inner
            .as_any()
            .downcast_ref::<WeakExtendedInstanceInterface<C>>()
            .ok_or(RouterError::Unknown)?;
        let ExtendedMoniker::ComponentInstance(moniker) = target.extended_moniker() else {
            return Err(RoutingError::from(
                ComponentInstanceError::ComponentManagerInstanceUnexpected {},
            )
            .into());
        };
        match self.policy_checker.can_route_capability(&self.capability_source, &moniker) {
            Ok(()) => self.router.route(request).await,
            Err(policy_error) => Err(RoutingError::PolicyError(policy_error).into()),
        }
    }
}
