// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::router;
use crate::{DirConnector, Router, RouterResponse};
use fidl::handle::AsHandleRef;
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::TryStreamExt;

impl crate::RemotableCapability for Router<DirConnector> {}

impl From<Router<DirConnector>> for fsandbox::Capability {
    fn from(router: Router<DirConnector>) -> Self {
        let (client_end, sender_stream) =
            fidl::endpoints::create_request_stream::<fsandbox::DirConnectorRouterMarker>();
        router.serve_and_register(sender_stream, client_end.get_koid().unwrap());
        fsandbox::Capability::DirConnectorRouter(client_end)
    }
}

impl TryFrom<RouterResponse<DirConnector>> for fsandbox::DirConnectorRouterRouteResponse {
    type Error = fsandbox::RouterError;

    fn try_from(resp: RouterResponse<DirConnector>) -> Result<Self, Self::Error> {
        match resp {
            RouterResponse::<DirConnector>::Capability(c) => {
                Ok(fsandbox::DirConnectorRouterRouteResponse::DirConnector(c.into()))
            }
            RouterResponse::<DirConnector>::Unavailable => {
                Ok(fsandbox::DirConnectorRouterRouteResponse::Unavailable(fsandbox::Unit {}))
            }
            RouterResponse::<DirConnector>::Debug(_) => Err(fsandbox::RouterError::NotSupported),
        }
    }
}

impl Router<DirConnector> {
    async fn serve_router(
        self,
        mut stream: fsandbox::DirConnectorRouterRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::DirConnectorRouterRequest::Route { payload, responder } => {
                    responder.send(router::route_from_fidl(&self, payload).await)?;
                }
                fsandbox::DirConnectorRouterRequest::_UnknownMethod { ordinal, .. } => {
                    log::warn!(
                        ordinal:%;
                        "Received unknown DirConnectorRouter request"
                    );
                }
            }
        }
        Ok(())
    }

    /// Serves the `fuchsia.sandbox.Router` protocol and moves ourself into the registry.
    pub fn serve_and_register(
        self,
        stream: fsandbox::DirConnectorRouterRequestStream,
        koid: zx::Koid,
    ) {
        let router = self.clone();

        // Move this capability into the registry.
        crate::fidl::registry::insert(self.into(), koid, async move {
            router.serve_router(stream).await.expect("failed to serve Router");
        });
    }
}
