// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logs::error::LogsError;
use fidl::endpoints::{ControlHandle, DiscoverableProtocolMarker};
use fidl_fuchsia_diagnostics_system as ftarget;
use futures::StreamExt;
use log::warn;
use zx::EventPair;

pub struct LogFreezeServer {
    freeze_token: EventPair,
}

impl LogFreezeServer {
    pub fn new(freeze_token: EventPair) -> Self {
        Self { freeze_token }
    }

    /// Handles a single FIDL request to freeze the serial log stream.
    pub async fn wait_for_client_freeze_request(
        self,
        stream: ftarget::SerialLogControlRequestStream,
    ) {
        if let Err(e) = Self::handle_requests(stream, self.freeze_token).await {
            warn!("error handling Log requests: {}", e);
        }
    }

    /// Actually handle the FIDL request. This handles only a single request, then exits.
    async fn handle_requests(
        mut stream: ftarget::SerialLogControlRequestStream,
        freeze_token: EventPair,
    ) -> Result<(), LogsError> {
        while let Some(request) = stream.next().await {
            let request = request.map_err(|source| LogsError::HandlingRequests {
                protocol: ftarget::SerialLogControlMarker::PROTOCOL_NAME,
                source,
            })?;

            match request {
                fidl_fuchsia_diagnostics_system::SerialLogControlRequest::FreezeSerialForwarding { responder } => {
                    responder.send(freeze_token)?;
                    return Ok(());
                },
                ftarget::SerialLogControlRequest::_UnknownMethod {
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
