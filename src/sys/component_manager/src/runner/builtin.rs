// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::ServerEnd, fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_component_runner as fcrunner, thiserror::Error, tracing::warn,
};

/// Wrapper for converting fcomponent::Error into the anyhow::Error type.
#[derive(Debug, Clone, Error)]
pub struct RemoteRunnerError(pub fcomponent::Error);

impl std::fmt::Display for RemoteRunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use the Debug formatter for Display.
        use std::fmt::Debug;
        self.0.fmt(f)
    }
}

impl std::convert::From<fcomponent::Error> for RemoteRunnerError {
    fn from(error: fcomponent::Error) -> RemoteRunnerError {
        RemoteRunnerError(error)
    }
}

/// A runner provided by another component.
pub struct RemoteRunner {
    client: fcrunner::ComponentRunnerProxy,
}

impl RemoteRunner {
    pub fn new(client: fcrunner::ComponentRunnerProxy) -> RemoteRunner {
        RemoteRunner { client }
    }

    pub async fn start(
        &self,
        start_info: fcrunner::ComponentStartInfo,
        server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
    ) {
        let resolved_url = start_info.resolved_url.clone().unwrap_or(String::new());
        if let Err(e) = self.client.start(start_info, server_end) {
            warn!(url=%resolved_url, error=%e, "Failed to call runner to start component");
        }
    }
}
