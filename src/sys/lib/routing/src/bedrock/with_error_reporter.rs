// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use router_error::RouterError;
use sandbox::{CapabilityBound, Request, Routable, Router, RouterResponse};

use crate::error::{ErrorReporter, RouteRequestErrorInfo};

pub trait WithErrorReporter {
    /// Returns a router that reports errors to `error_reporter`.
    fn with_error_reporter(
        self,
        route_request: RouteRequestErrorInfo,
        error_reporter: impl ErrorReporter,
    ) -> Self;
}

struct RouterWithErrorReporter<T: CapabilityBound, R: ErrorReporter> {
    router: Router<T>,
    route_request: RouteRequestErrorInfo,
    error_reporter: R,
}

#[async_trait]
impl<T: CapabilityBound, R: ErrorReporter> Routable<T> for RouterWithErrorReporter<T, R> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        let target = request.as_ref().map(|r| r.target.clone());
        match self.router.route(request, debug).await {
            Ok(res) => Ok(res),
            Err(err) => {
                self.error_reporter.report(&self.route_request, &err, target).await;
                Err(err)
            }
        }
    }
}

impl<T: CapabilityBound> WithErrorReporter for Router<T> {
    fn with_error_reporter(
        self,
        route_request: RouteRequestErrorInfo,
        error_reporter: impl ErrorReporter,
    ) -> Self {
        Self::new(RouterWithErrorReporter { router: self, route_request, error_reporter })
    }
}
