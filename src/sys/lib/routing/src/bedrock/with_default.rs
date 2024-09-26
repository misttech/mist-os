// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use router_error::RouterError;
use sandbox::{Capability, Request, Routable, Router};

pub trait WithDefault {
    /// Returns a router that exceptions a `None` request, supplying the provided default.
    fn with_default(self, request: Request) -> Self;
}

impl WithDefault for Router {
    fn with_default(self, request: Request) -> Self {
        #[derive(Debug)]
        struct RouterWithDefault {
            router: Router,
            default_request: Request,
        }

        #[async_trait]
        impl Routable for RouterWithDefault {
            async fn route(
                &self,
                request: Option<Request>,
                debug: bool,
            ) -> Result<Capability, RouterError> {
                let request = if let Some(request) = request {
                    request
                } else {
                    self.default_request.try_clone()?
                };
                self.router.route(Some(request), debug).await
            }
        }

        Self::new(RouterWithDefault { router: self, default_request: request })
    }
}
