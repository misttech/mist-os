// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{DictExt, RoutingError},
    async_trait::async_trait,
    cm_types::IterablePath,
    router_error::RouterError,
    sandbox::{Capability, Request, Routable, Router},
    std::fmt::Debug,
};

/// Implements the `lazy_get` function for [`Routable`] objects.
pub trait LazyGet: Routable {
    /// Returns a router that requests capabilities from the specified `path` relative to
    /// the base routable or fails the request with `not_found_error` if the member is not
    /// found. The base routable should resolve with a dictionary capability.
    fn lazy_get<P>(self, path: P, not_found_error: impl Into<RouterError>) -> Router
    where
        P: IterablePath + Debug + 'static;
}

impl<T: Routable + 'static> LazyGet for T {
    fn lazy_get<P>(self, path: P, not_found_error: impl Into<RouterError>) -> Router
    where
        P: IterablePath + Debug + 'static,
    {
        #[derive(Debug)]
        struct ScopedDictRouter<P: IterablePath + Debug + 'static> {
            router: Router,
            path: P,
            not_found_error: RouterError,
        }

        #[async_trait]
        impl<P: IterablePath + Debug + 'static> Routable for ScopedDictRouter<P> {
            async fn route(&self, request: Request) -> Result<Capability, RouterError> {
                match self.router.route(request.clone()).await? {
                    Capability::Dictionary(dict) => {
                        let maybe_capability =
                            dict.get_with_request(&self.path, request.clone()).await?;
                        maybe_capability.ok_or_else(|| self.not_found_error.clone())
                    }
                    _ => Err(RoutingError::BedrockMemberAccessUnsupported.into()),
                }
            }
        }

        Router::new(ScopedDictRouter {
            router: Router::new(self),
            path,
            not_found_error: not_found_error.into(),
        })
    }
}
