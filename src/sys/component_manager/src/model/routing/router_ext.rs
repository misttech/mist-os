// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::routing::router::Request;
use crate::model::routing::router::Routable;
use crate::model::routing::router::Router;

use crate::capability::CapabilitySource;
use ::routing::{error::RoutingError, policy::GlobalPolicyChecker};
use async_trait::async_trait;
use bedrock_error::BedrockError;
use sandbox::Capability;

/// A trait to add functions to Router that know about the component manager
/// types.
pub trait RouterExt {
    /// Returns a router that ensures the capability request is allowed by the
    /// policy in [`GlobalPolicyChecker`].
    fn with_policy_check(
        self,
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
    ) -> Self;
}

impl RouterExt for Router {
    fn with_policy_check(
        self,
        capability_source: CapabilitySource,
        policy_checker: GlobalPolicyChecker,
    ) -> Self {
        Router::new(PolicyCheckRouter::new(capability_source, policy_checker, self))
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
