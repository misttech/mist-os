// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::request_metadata::Metadata;
use crate::error::RoutingError;
use async_trait::async_trait;
use cm_types::Availability;
use fidl_fuchsia_component_sandbox as fsandbox;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{CapabilityBound, Request, Routable, Router, RouterResponse};

struct AvailabilityRouter<T: CapabilityBound> {
    router: Router<T>,
    availability: Availability,
    moniker: ExtendedMoniker,
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for AvailabilityRouter<T> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        let AvailabilityRouter { router, availability, moniker } = self;
        // The availability of the request must be compatible with the
        // availability of this step of the route.
        let request_availability =
            request.metadata.get_metadata().ok_or(fsandbox::RouterError::InvalidArgs).inspect_err(
                |e| {
                    log::error!(
                        "request {:?} did not have availability metadata: {e:?}",
                        request.target
                    )
                },
            )?;
        match crate::availability::advance(moniker, request_availability, *availability) {
            Ok(updated) => {
                request.metadata.set_metadata(updated);
                // Everything checks out, forward the request.
                router.route(Some(request), debug).await
            }
            Err(e) => Err(RoutingError::from(e).into()),
        }
    }
}

pub trait WithAvailability {
    /// Returns a router that ensures the capability request has an availability
    /// strength that is at least the provided `availability`.
    fn with_availability(
        self,
        moniker: impl Into<ExtendedMoniker>,
        availability: Availability,
    ) -> Self;
}

impl<T: CapabilityBound> WithAvailability for Router<T> {
    fn with_availability(
        self,
        moniker: impl Into<ExtendedMoniker>,
        availability: Availability,
    ) -> Self {
        Router::<T>::new(AvailabilityRouter { availability, router: self, moniker: moniker.into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use router_error::{DowncastErrorForTest, RouterError};
    use sandbox::{Data, Dict, WeakInstanceToken};
    use std::sync::Arc;

    #[derive(Debug)]
    struct FakeComponentToken {}

    impl FakeComponentToken {
        fn new() -> WeakInstanceToken {
            WeakInstanceToken { inner: Arc::new(FakeComponentToken {}) }
        }
    }

    impl sandbox::WeakInstanceTokenAny for FakeComponentToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[fuchsia::test]
    async fn availability_good() {
        let source = Data::String("hello".to_string());
        let base = Router::<Data>::new_ok(source);
        let proxy =
            base.with_availability(ExtendedMoniker::ComponentManager, Availability::Optional);
        let metadata = Dict::new();
        metadata.set_metadata(Availability::Optional);
        let capability = proxy
            .route(Some(Request { target: FakeComponentToken::new(), metadata }), false)
            .await
            .unwrap();
        let capability = match capability {
            RouterResponse::<Data>::Capability(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn availability_bad() {
        let source = Data::String("hello".to_string());
        let base = Router::<Data>::new_ok(source);
        let proxy =
            base.with_availability(ExtendedMoniker::ComponentManager, Availability::Optional);
        let metadata = Dict::new();
        metadata.set_metadata(Availability::Required);
        let error = proxy
            .route(Some(Request { target: FakeComponentToken::new(), metadata }), false)
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::AvailabilityRoutingError(
                    crate::error::AvailabilityRoutingError::TargetHasStrongerAvailability { moniker: ExtendedMoniker::ComponentManager}
                )
            )
        );
    }
}
