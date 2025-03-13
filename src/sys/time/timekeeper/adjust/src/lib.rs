// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Serves the `fuchsia.time.external/Adjust` FIDL API.

use anyhow::Result;
use fidl_fuchsia_time_external as ffte;
use futures::StreamExt;
use log::{debug, error};

pub fn tbd() {}

pub struct Server {}

impl Server {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn serve(&self, mut stream: ffte::AdjustRequestStream) -> Result<()> {
        debug!("time_adjust::serve: entering serving loop");
        while let Some(request) = stream.next().await {
            debug!("time_adjust::Server::serve: request: {:?}", request);
            match request {
                Ok(ffte::AdjustRequest::ReportBootToUtcMapping { responder, .. }) => {
                    // TODO: b/394636853 - Implement.
                    responder.send(Err(ffte::Error::Internal)).expect("infallible");
                    debug!("time_adjust::Server::serve: responded with error for now.");
                }
                Err(e) => {
                    error!("FIDL error: {:?}", e);
                }
            };
        }
        debug!("time_adjust::serve: exited  serving loop");
        Ok(())
    }
}

#[cfg(test)]
mod tests {}
