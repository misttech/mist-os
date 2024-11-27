// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::router;
use crate::{ConversionError, DirEntry, Router, RouterResponse};
use fidl::handle::AsHandleRef;
use futures::TryStreamExt;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use vfs::execution_scope::ExecutionScope;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

impl crate::RemotableCapability for Router<DirEntry> {
    fn try_into_directory_entry(
        self,
        scope: ExecutionScope,
    ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(self.into_directory_entry(
            // TODO(https://fxbug.dev/340891837): This assumes the DirEntry type is Service.
            // Unfortunately, with the current API there is no good way to get the DirEntry type in
            // advance. This problem should go away once we revamp or remove DirEntry.
            fio::DirentType::Service,
            scope,
        ))
    }
}

impl From<Router<DirEntry>> for fsandbox::Capability {
    fn from(router: Router<DirEntry>) -> Self {
        let (client_end, sender_stream) =
            fidl::endpoints::create_request_stream::<fsandbox::DirEntryRouterMarker>();
        router.serve_and_register(sender_stream, client_end.get_koid().unwrap());
        fsandbox::Capability::DirEntryRouter(client_end)
    }
}

impl TryFrom<RouterResponse<DirEntry>> for fsandbox::DirEntryRouterRouteResponse {
    type Error = fsandbox::RouterError;

    fn try_from(resp: RouterResponse<DirEntry>) -> Result<Self, Self::Error> {
        match resp {
            RouterResponse::<DirEntry>::Capability(c) => {
                Ok(fsandbox::DirEntryRouterRouteResponse::DirEntry(c.into()))
            }
            RouterResponse::<DirEntry>::Unavailable => {
                Ok(fsandbox::DirEntryRouterRouteResponse::Unavailable(fsandbox::Unit {}))
            }
            RouterResponse::<DirEntry>::Debug(_) => Err(fsandbox::RouterError::NotSupported),
        }
    }
}

impl Router<DirEntry> {
    async fn serve_router(
        self,
        mut stream: fsandbox::DirEntryRouterRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::DirEntryRouterRequest::Route { payload, responder } => {
                    responder.send(router::route_from_fidl(&self, payload).await)?;
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
