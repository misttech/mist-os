// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use futures::stream::StreamExt;
use log::warn;
use {fidl_fuchsia_power_system as fsag, fuchsia_trace as ftrace};

/// Uses |fuchsia.power.system/ActivityGovernorListener|s to determine when to
/// trigger a flush.
pub struct FlushTrigger {
    sag: fsag::ActivityGovernorProxy,
}

impl FlushTrigger {
    pub fn new(sag: fsag::ActivityGovernorProxy) -> Self {
        Self { sag }
    }

    /// Calls |flusher| when it receives
    /// |fuchsia.power.system/ActivityGovernorListener.OnSuspendStarted| and
    /// replies to the request *AFTER* |flusher.flush| returns.
    pub async fn run<'a>(&self, flusher: &dyn FlushListener) -> Result<(), fidl::Error> {
        let (client, server) =
            fidl::endpoints::create_endpoints::<fsag::ActivityGovernorListenerMarker>();

        self.sag
            .register_listener(fsag::ActivityGovernorRegisterListenerRequest {
                listener: Some(client),
                ..Default::default()
            })
            .await?;

        let mut request_stream = server.into_stream();

        while let Some(req) = request_stream.next().await {
            match req {
                Ok(fsag::ActivityGovernorListenerRequest::OnSuspendStarted { responder }) => {
                    ftrace::duration!(crate::TRACE_CATEGORY, c"flush-triggered");
                    flusher.flush().await;
                    let _ = responder.send();
                }
                Ok(fsag::ActivityGovernorListenerRequest::OnResume { responder }) => {
                    let _ = responder.send();
                }
                Ok(fsag::ActivityGovernorListenerRequest::_UnknownMethod { .. }) => {
                    warn!("unrecognized listener method, ignoring");
                }
                Err(_) => continue,
            }
        }
        Ok(())
    }
}

#[async_trait]
pub trait FlushListener {
    async fn flush(&self);
}
