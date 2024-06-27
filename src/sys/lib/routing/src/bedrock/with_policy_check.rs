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

pub trait WithPolicyCheck {
    /// Returns a router that ensures the capability request is allowed by the
    /// policy in [`GlobalPolicyChecker`].
    fn with_policy_check<C: ComponentInstanceInterface + 'static>(
        self,
        capability_source: CapabilitySource<C>,
        policy_checker: GlobalPolicyChecker,
    ) -> Self;
}

impl WithPolicyCheck for Router {
    fn with_policy_check<C: ComponentInstanceInterface + 'static>(
        self,
        capability_source: CapabilitySource<C>,
        policy_checker: GlobalPolicyChecker,
    ) -> Self {
        Router::new(PolicyCheckRouter::new(capability_source, policy_checker, self))
    }
}

pub struct PolicyCheckRouter<C: ComponentInstanceInterface + 'static> {
    capability_source: CapabilitySource<C>,
    policy_checker: GlobalPolicyChecker,
    router: Router,
}

impl<C: ComponentInstanceInterface + 'static> PolicyCheckRouter<C> {
    pub fn new(
        capability_source: CapabilitySource<C>,
        policy_checker: GlobalPolicyChecker,
        router: Router,
    ) -> Self {
        PolicyCheckRouter { capability_source, policy_checker, router }
    }
}

#[async_trait]
impl<C: ComponentInstanceInterface + 'static> Routable for PolicyCheckRouter<C> {
    async fn route(&self, request: Request) -> Result<Capability, RouterError> {
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
