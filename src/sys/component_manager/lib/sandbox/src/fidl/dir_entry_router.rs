// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::specific_router;
use crate::{DirEntry, SpecificRouter, SpecificRouterResponse};
use fidl::handle::AsHandleRef;
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::TryStreamExt;

impl crate::RemotableCapability for SpecificRouter<DirEntry> {}

impl From<SpecificRouter<DirEntry>> for fsandbox::Capability {
    fn from(router: SpecificRouter<DirEntry>) -> Self {
        let (client_end, sender_stream) =
            fidl::endpoints::create_request_stream::<fsandbox::DirEntryRouterMarker>().unwrap();
        router.serve_and_register(sender_stream, client_end.get_koid().unwrap());
        fsandbox::Capability::DirEntryRouter(client_end)
    }
}

impl TryFrom<SpecificRouterResponse<DirEntry>> for fsandbox::DirEntryRouterRouteResponse {
    type Error = fsandbox::RouterError;

    fn try_from(resp: SpecificRouterResponse<DirEntry>) -> Result<Self, Self::Error> {
        match resp {
            SpecificRouterResponse::<DirEntry>::Capability(c) => {
                Ok(fsandbox::DirEntryRouterRouteResponse::DirEntry(c.into()))
            }
            SpecificRouterResponse::<DirEntry>::Unavailable => {
                Ok(fsandbox::DirEntryRouterRouteResponse::Unavailable(fsandbox::Unit {}))
            }
            SpecificRouterResponse::<DirEntry>::Debug(_) => {
                Err(fsandbox::RouterError::NotSupported)
            }
        }
    }
}

impl SpecificRouter<DirEntry> {
    async fn serve_router(
        self,
        mut stream: fsandbox::DirEntryRouterRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::DirEntryRouterRequest::Route { payload, responder } => {
                    responder.send(specific_router::route_from_fidl(&self, payload).await)?;
                }
                fsandbox::DirEntryRouterRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(
                        %ordinal, "Received unknown DirEntryRouter request"
                    );
                }
            }
        }
        Ok(())
    }

    /// Serves the `fuchsia.sandbox.Router` protocol and moves ourself into the registry.
    pub fn serve_and_register(self, stream: fsandbox::DirEntryRouterRequestStream, koid: zx::Koid) {
        let router = self.clone();

        // Move this capability into the registry.
        crate::fidl::registry::insert(self.into(), koid, async move {
            router.serve_router(stream).await.expect("failed to serve Router");
        });
    }
}
