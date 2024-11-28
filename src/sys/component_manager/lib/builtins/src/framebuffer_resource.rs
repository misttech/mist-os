// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use fidl_fuchsia_kernel as fkernel;
use futures::prelude::*;
use std::sync::Arc;
use zx::{self as zx, HandleBased, Resource};

/// An implementation of fuchsia.kernel.FramebufferResource protocol.
pub struct FramebufferResource {
    resource: Resource,
}

impl FramebufferResource {
    /// `resource` must be the Framebuffer resource.
    pub fn new(resource: Resource) -> Result<Arc<Self>, Error> {
        let resource_info = resource.info()?;
        if resource_info.kind != zx::sys::ZX_RSRC_KIND_SYSTEM
            || resource_info.base != zx::sys::ZX_RSRC_SYSTEM_FRAMEBUFFER_BASE
            || resource_info.size != 1
        {
            return Err(format_err!("Framebuffer resource not available."));
        }
        Ok(Arc::new(Self { resource }))
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fkernel::FramebufferResourceRequestStream,
    ) -> Result<(), Error> {
        while let Some(fkernel::FramebufferResourceRequest::Get { responder }) =
            stream.try_next().await?
        {
            responder.send(self.resource.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_component::client::connect_to_protocol;
    use {fidl_fuchsia_kernel as fkernel, fuchsia_async as fasync};

    async fn get_framebuffer_resource() -> Result<Resource, Error> {
        let framebuffer_resource_provider =
            connect_to_protocol::<fkernel::FramebufferResourceMarker>()?;
        let framebuffer_resource_handle = framebuffer_resource_provider.get().await?;
        Ok(Resource::from(framebuffer_resource_handle))
    }

    async fn serve_framebuffer_resource() -> Result<fkernel::FramebufferResourceProxy, Error> {
        let framebuffer_resource = get_framebuffer_resource().await?;

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::FramebufferResourceMarker>();
        fasync::Task::local(
            FramebufferResource::new(framebuffer_resource)
                .unwrap_or_else(|e| {
                    panic!("Error while creating framebuffer resource service: {}", e)
                })
                .serve(stream)
                .unwrap_or_else(|e| {
                    panic!("Error while serving FRAMEBUFFER resource service: {}", e)
                }),
        )
        .detach();
        Ok(proxy)
    }

    #[fuchsia::test]
    async fn base_type_is_framebuffer() -> Result<(), Error> {
        let framebuffer_resource_provider = serve_framebuffer_resource().await?;
        let framebuffer_resource: Resource = framebuffer_resource_provider.get().await?;
        let resource_info = framebuffer_resource.info()?;
        assert_eq!(resource_info.kind, zx::sys::ZX_RSRC_KIND_SYSTEM);
        assert_eq!(resource_info.base, zx::sys::ZX_RSRC_SYSTEM_FRAMEBUFFER_BASE);
        assert_eq!(resource_info.size, 1);
        Ok(())
    }
}
