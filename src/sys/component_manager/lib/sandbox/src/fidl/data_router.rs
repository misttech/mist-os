// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::router;
use crate::{ConversionError, Data, Router, RouterResponse};
use fidl::handle::AsHandleRef;
use futures::TryStreamExt;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use vfs::execution_scope::ExecutionScope;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

impl crate::RemotableCapability for Router<Data> {
    fn try_into_directory_entry(
        self,
        scope: ExecutionScope,
    ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(self.into_directory_entry(fio::DirentType::Service, scope))
    }
}

impl From<Router<Data>> for fsandbox::Capability {
    fn from(router: Router<Data>) -> Self {
        let (client_end, sender_stream) =
            fidl::endpoints::create_request_stream::<fsandbox::DataRouterMarker>();
        router.serve_and_register(sender_stream, client_end.get_koid().unwrap());
        fsandbox::Capability::DataRouter(client_end)
    }
}

impl TryFrom<RouterResponse<Data>> for fsandbox::DataRouterRouteResponse {
    type Error = fsandbox::RouterError;

    fn try_from(resp: RouterResponse<Data>) -> Result<Self, Self::Error> {
        match resp {
            RouterResponse::<Data>::Capability(c) => {
                Ok(fsandbox::DataRouterRouteResponse::Data(c.into()))
            }
            RouterResponse::<Data>::Unavailable => {
                Ok(fsandbox::DataRouterRouteResponse::Unavailable(fsandbox::Unit {}))
            }
            RouterResponse::<Data>::Debug(_) => Err(fsandbox::RouterError::NotSupported),
        }
    }
}

impl Router<Data> {
    async fn serve_router(
        self,
        mut stream: fsandbox::DataRouterRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::DataRouterRequest::Route { payload, responder } => {
                    responder.send(router::route_from_fidl(&self, payload).await)?;
                }
                fsandbox::DataRouterRequest::_UnknownMethod { ordinal, .. } => {
                    log::warn!(ordinal:%; "Received unknown DataRouter request");
                }
            }
        }
        Ok(())
    }

    /// Serves the `fuchsia.sandbox.Router` protocol and moves ourself into the registry.
    pub fn serve_and_register(self, stream: fsandbox::DataRouterRequestStream, koid: zx::Koid) {
        let router = self.clone();

        // Move this capability into the registry.
        crate::fidl::registry::insert(self.into(), koid, async move {
            router.serve_router(stream).await.expect("failed to serve Router");
        });
    }
}
