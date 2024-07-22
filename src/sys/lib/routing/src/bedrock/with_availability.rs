// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::RoutingError;
use cm_types::Availability;
use futures::FutureExt;
use sandbox::{Request, Router};

pub trait WithAvailability {
    /// Returns a router that ensures the capability request has an availability
    /// strength that is at least the provided `availability`.
    fn with_availability(self, availability: Availability) -> Router;
}

impl WithAvailability for Router {
    fn with_availability(self, availability: Availability) -> Router {
        let route_fn = move |mut request: Request| {
            let router = self.clone();
            async move {
                // The availability of the request must be compatible with the
                // availability of this step of the route.
                match crate::availability::advance(request.availability, availability) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use router_error::{DowncastErrorForTest, RouterError};
    use sandbox::{Capability, Data, WeakInstanceToken};
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
        let source: Capability = Data::String("hello".to_string()).into();
        let base = Router::new(source);
        let proxy = base.with_availability(Availability::Optional);
        let capability = proxy
            .route(Request {
                availability: Availability::Optional,
                target: FakeComponentToken::new(),
                debug: false,
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
                debug: false,
            })
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::AvailabilityRoutingError(
                    crate::error::AvailabilityRoutingError::TargetHasStrongerAvailability
                )
            )
        );
    }
}
