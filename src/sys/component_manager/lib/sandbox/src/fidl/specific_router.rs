// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Capability, CapabilityBound, Request, SpecificRouter, SpecificRouterResponse};
use fidl::AsHandleRef;
use fidl_fuchsia_component_sandbox as fsandbox;

/// Binds a Route request from fidl to the Rust [SpecificRouter::Route] API. Shared by
/// [SpecificRouter] server implementations.
pub(crate) async fn route_from_fidl<T, R>(
    router: &SpecificRouter<T>,
    payload: fsandbox::RouteRequest,
) -> Result<R, fsandbox::RouterError>
where
    T: CapabilityBound,
    R: TryFrom<SpecificRouterResponse<T>, Error = fsandbox::RouterError>,
{
    let resp = match (payload.requesting, payload.metadata) {
        (Some(token), Some(metadata)) => {
            let capability =
                crate::fidl::registry::get(token.token.as_handle_ref().get_koid().unwrap());
            let component = match capability {
                Some(crate::Capability::Instance(c)) => c,
                Some(_) => return Err(fsandbox::RouterError::InvalidArgs),
                None => return Err(fsandbox::RouterError::InvalidArgs),
            };
            let Capability::Dictionary(metadata) =
                Capability::try_from(fsandbox::Capability::Dictionary(metadata)).unwrap()
            else {
                return Err(fsandbox::RouterError::InvalidArgs);
            };
            let request = Request { target: component, metadata };
            router.route(Some(request), false).await?
        }
        (None, None) => router.route(None, false).await?,
        _ => {
            return Err(fsandbox::RouterError::InvalidArgs);
        }
    };
    resp.try_into()
}
