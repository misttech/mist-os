// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::specific_router;
use crate::{Dict, SpecificRouter, SpecificRouterResponse};
use fidl::handle::AsHandleRef;
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::TryStreamExt;

impl crate::RemotableCapability for SpecificRouter<Dict> {}

impl From<SpecificRouter<Dict>> for fsandbox::Capability {
    fn from(router: SpecificRouter<Dict>) -> Self {
        let (client_end, sender_stream) =
            fidl::endpoints::create_request_stream::<fsandbox::DictionaryRouterMarker>().unwrap();
        router.serve_and_register(sender_stream, client_end.get_koid().unwrap());
        fsandbox::Capability::DictionaryRouter(client_end)
    }
}

impl TryFrom<SpecificRouterResponse<Dict>> for fsandbox::DictionaryRouterRouteResponse {
    type Error = fsandbox::RouterError;

    fn try_from(resp: SpecificRouterResponse<Dict>) -> Result<Self, Self::Error> {
        match resp {
            SpecificRouterResponse::<Dict>::Capability(c) => {
                Ok(fsandbox::DictionaryRouterRouteResponse::Dictionary(c.into()))
            }
            SpecificRouterResponse::<Dict>::Unavailable => {
                Ok(fsandbox::DictionaryRouterRouteResponse::Unavailable(fsandbox::Unit {}))
            }
            SpecificRouterResponse::<Dict>::Debug(_) => Err(fsandbox::RouterError::NotSupported),
        }
    }
}

impl SpecificRouter<Dict> {
    async fn serve_router(
        self,
        mut stream: fsandbox::DictionaryRouterRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::DictionaryRouterRequest::Route { payload, responder } => {
                    responder.send(specific_router::route_from_fidl(&self, payload).await)?;
                }
                fsandbox::DictionaryRouterRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(
                        %ordinal, "Received unknown DictionaryRouter request"
                    );
                }
            }
        }
        Ok(())
    }

    /// Serves the `fuchsia.sandbox.Router` protocol and moves ourself into the registry.
    pub fn serve_and_register(
        self,
        stream: fsandbox::DictionaryRouterRequestStream,
        koid: zx::Koid,
    ) {
        let router = self.clone();

        // Move this capability into the registry.
        crate::fidl::registry::insert(self.into(), koid, async move {
            router.serve_router(stream).await.expect("failed to serve Router");
        });
    }
}
