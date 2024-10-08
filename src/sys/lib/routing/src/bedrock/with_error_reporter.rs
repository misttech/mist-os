// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use router_error::RouterError;
use sandbox::{Capability, Request, Routable, Router};

use crate::error::{ErrorReporter, RouteRequestErrorInfo};

pub trait WithErrorReporter {
    /// Returns a router that reports errors to `error_reporter`.
    fn with_error_reporter(
        self,
        route_request: RouteRequestErrorInfo,
        error_reporter: impl ErrorReporter,
    ) -> Self;
}

impl WithErrorReporter for Router {
    fn with_error_reporter(
        self,
        route_request: RouteRequestErrorInfo,
        error_reporter: impl ErrorReporter,
    ) -> Self {
        struct RouterWithErrorReporter<R> {
            router: Router,
            route_request: RouteRequestErrorInfo,
            error_reporter: R,
        }

        #[async_trait]
        impl<R: ErrorReporter> Routable for RouterWithErrorReporter<R> {
            async fn route(
                &self,
                request: Option<Request>,
                debug: bool,
            ) -> Result<Capability, RouterError> {
                match self.router.route(request, debug).await {
                    Ok(res) => Ok(res),
                    Err(err) => {
                        self.error_reporter.report(&self.route_request, &err).await;
                        Err(err)
                    }
                }
            }
        }

        Self::new(RouterWithErrorReporter { router: self, route_request, error_reporter })
    }
}
