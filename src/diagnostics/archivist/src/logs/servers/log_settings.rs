// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logs::error::LogsError;
use crate::logs::repository::LogsRepository;
use fidl::endpoints::{ControlHandle, DiscoverableProtocolMarker};
use futures::StreamExt;
use log::warn;
use std::sync::Arc;
use {fidl_fuchsia_diagnostics as fdiagnostics, fuchsia_async as fasync};

pub struct LogSettingsServer {
    /// The repository holding the logs.
    logs_repo: Arc<LogsRepository>,

    /// Scope holding all of the server Tasks.
    scope: fasync::Scope,
}

impl LogSettingsServer {
    pub fn new(logs_repo: Arc<LogsRepository>, scope: fasync::Scope) -> Self {
        Self { logs_repo, scope }
    }

    /// Spawn a task to handle requests from components reading the shared log.
    pub fn spawn(&self, stream: fdiagnostics::LogSettingsRequestStream) {
        let logs_repo = Arc::clone(&self.logs_repo);
        self.scope.spawn(async move {
            if let Err(e) = Self::handle_requests(logs_repo, stream).await {
                warn!("error handling Log requests: {}", e);
            }
        });
    }

    pub async fn handle_requests(
        logs_repo: Arc<LogsRepository>,
        mut stream: fdiagnostics::LogSettingsRequestStream,
    ) -> Result<(), LogsError> {
        let connection_id = logs_repo.new_interest_connection();
        while let Some(request) = stream.next().await {
            let request = request.map_err(|source| LogsError::HandlingRequests {
                protocol: fdiagnostics::LogSettingsMarker::PROTOCOL_NAME,
                source,
            })?;
            match request {
                fdiagnostics::LogSettingsRequest::RegisterInterest { control_handle, .. } => {
                    warn!("fuchsia.diagnostics/LogSettings.RegisterInterest is not supported; closing the channel");
                    control_handle.shutdown();
                }
                fdiagnostics::LogSettingsRequest::SetInterest { selectors, responder } => {
                    logs_repo.update_logs_interest(connection_id, selectors);
                    responder.send().ok();
                }
            }
        }
        logs_repo.finish_interest_connection(connection_id);

        Ok(())
    }
}
