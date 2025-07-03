// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::dict_ext::request_with_dictionary_replacement;
use crate::{DictExt, RoutingError};
use async_trait::async_trait;
use cm_types::IterablePath;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{CapabilityBound, Dict, Request, Routable, Router, RouterResponse};
use std::fmt::Debug;

/// Implements the `lazy_get` function for [`Routable<Dict>`].
pub trait LazyGet<T: CapabilityBound>: Routable<Dict> {
    /// Returns a router that requests a dictionary from the specified `path` relative to
    /// the base routable or fails the request with `not_found_error` if the member is not
    /// found.
    fn lazy_get<P>(self, path: P, not_found_error: RoutingError) -> Router<T>
    where
        P: IterablePath + Debug + 'static;
}

impl<R: Routable<Dict> + 'static, T: CapabilityBound> LazyGet<T> for R {
    fn lazy_get<P>(self, path: P, not_found_error: RoutingError) -> Router<T>
    where
        P: IterablePath + Debug + 'static,
    {
        #[derive(Debug)]
        struct ScopedDictRouter<P: IterablePath + Debug + 'static> {
            router: Router<Dict>,
            path: P,
            not_found_error: RoutingError,
        }

        #[async_trait]
        impl<P: IterablePath + Debug + 'static, T: CapabilityBound> Routable<T> for ScopedDictRouter<P> {
            async fn route(
                &self,
                request: Option<Request>,
                debug: bool,
            ) -> Result<RouterResponse<T>, RouterError> {
                // If `debug` is true, that should only apply to the capability at `path`.
                // Here we're looking up the containing dictionary, so set `debug = false`, to
                // obtain the actual Dict and not its debug info.
                let init_request = if self.path.iter_segments().count() > 1 {
                    request_with_dictionary_replacement(request.as_ref())?
                } else {
                    request.as_ref().map(|r| r.try_clone()).transpose()?
                };
                match self.router.route(init_request, false).await? {
                    RouterResponse::<Dict>::Capability(dict) => {
                        let moniker: ExtendedMoniker = self.not_found_error.clone().into();
                        let resp =
                            dict.get_with_request(&moniker, &self.path, request, debug).await?;
                        let resp =
                            resp.ok_or_else(|| RouterError::from(self.not_found_error.clone()))?;
                        let resp = resp.try_into().map_err(|debug_name: &'static str| {
                            RoutingError::BedrockWrongCapabilityType {
                                expected: T::debug_typename().into(),
                                actual: debug_name.into(),
                                moniker,
                            }
                        })?;
                        return Ok(resp);
                    }
                    _ => Err(RoutingError::BedrockMemberAccessUnsupported {
                        moniker: self.not_found_error.clone().into(),
                    }
                    .into()),
                }
            }
        }

        Router::<T>::new(ScopedDictRouter {
            router: Router::<Dict>::new(self),
            path,
            not_found_error: not_found_error.into(),
        })
    }
}
