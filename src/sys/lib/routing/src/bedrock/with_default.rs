// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use router_error::RouterError;
use sandbox::{CapabilityBound, Request, Routable, Router, RouterResponse};

pub trait WithDefault {
    /// Returns a router that exceptions a `None` request, supplying the provided default.
    fn with_default(self, request: Request) -> Self;
}

#[derive(Debug)]
struct RouterWithDefault<T: CapabilityBound> {
    router: Router<T>,
    default_request: Request,
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for RouterWithDefault<T> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        let request =
            if let Some(request) = request { request } else { self.default_request.try_clone()? };
        self.router.route(Some(request), debug).await
    }
}

impl<T: CapabilityBound> WithDefault for Router<T> {
    fn with_default(self, request: Request) -> Self {
        Self::new(RouterWithDefault { router: self, default_request: request })
    }
}
