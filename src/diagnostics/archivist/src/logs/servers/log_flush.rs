// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logs::error::LogsError;
use fidl::endpoints::{ControlHandle, DiscoverableProtocolMarker};
use fidl_fuchsia_diagnostics as fdiagnostics;
use fuchsia_async::Scope;
use futures::StreamExt;
use log::warn;

pub struct LogFlushServer {
    scope: Scope,
}

impl LogFlushServer {
    pub fn new(scope: Scope) -> Self {
        Self { scope }
    }

    /// Listens for flush requests on a FIDL channel.
    pub fn spawn(&self, stream: fdiagnostics::LogFlusherRequestStream) {
        self.scope.spawn(async move {
            if let Err(e) = Self::handle_requests(stream).await {
                warn!("error handling Log requests: {}", e);
            }
        });
    }

    /// Actually handle the FIDL requests.
    async fn handle_requests(
        mut stream: fdiagnostics::LogFlusherRequestStream,
    ) -> Result<(), LogsError> {
        while let Some(request) = stream.next().await {
            let request = request.map_err(|source| LogsError::HandlingRequests {
                protocol: fdiagnostics::LogFlusherMarker::PROTOCOL_NAME,
                source,
            })?;

            match request {
                fdiagnostics::LogFlusherRequest::WaitUntilFlushed { responder } => {
                    // TODO(https://fxbug.dev/386831734): Actually flush logs.

                    // We don't care if the other side exits
                    let _ = responder.send();
                }
                fdiagnostics::LogFlusherRequest::_UnknownMethod {
                    ordinal,
                    method_type,
                    control_handle,
                    ..
                } => {
                    warn!(ordinal, method_type:?; "Unknown request. Closing connection");
                    control_handle.shutdown_with_epitaph(zx::Status::UNAVAILABLE);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_proxy;

    #[fuchsia::test]
    async fn all_logs_get_flushed_when_flush_is_received_before_returning_from_flush() {
        let server = LogFlushServer::new(Scope::new());

        let (proxy, stream) = create_proxy::<fdiagnostics::LogFlusherMarker>();
        server.spawn(stream.into_stream());

        proxy.wait_until_flushed().await.unwrap();
    }
}
