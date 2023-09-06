// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl_fuchsia_boot as fboot,
    fuchsia_zircon::{self as zx, HandleBased, Resource},
    futures::prelude::*,
    std::sync::Arc,
};

/// An implementation of the `fuchsia.boot.RootResource` protocol.
pub struct RootResource {
    resource: Resource,
}

impl RootResource {
    pub fn new(resource: Resource) -> Arc<Self> {
        Arc::new(Self { resource })
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fboot::RootResourceRequestStream,
    ) -> Result<(), Error> {
        while let Some(fboot::RootResourceRequest::Get { responder }) = stream.try_next().await? {
            responder.send(self.resource.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
        }
        Ok(())
    }
}
