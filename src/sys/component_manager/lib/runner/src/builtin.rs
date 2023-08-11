// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{Runner, StartInfo},
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl::prelude::*,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_runner as fcrunner,
    fuchsia_async as fasync,
    futures::stream::StreamExt,
    thiserror::Error,
    tracing::error,
};

/// A null runner for components without a runtime environment.
///
/// Such environments, even though they don't execute any code, can still be
/// used by other components to bind to, which in turn may trigger further
/// bindings to its children.
pub struct NullRunner {}

#[async_trait]
impl Runner for NullRunner {
    async fn start(
        &self,
        _start_info: StartInfo,
        server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
    ) {
        spawn_null_controller_server(
            server_end
                .into_stream()
                .expect("NullRunner failed to convert server channel into request stream"),
        );
    }
}

/// Spawn an async execution context which takes ownership of `server_end`
/// and holds on to it until a stop or kill request is received.
fn spawn_null_controller_server(mut request_stream: fcrunner::ComponentControllerRequestStream) {
    // Listen to the ComponentController server end and exit after the first
    // one, as this is the contract we have implemented so far. Exiting will
    // cause our handle to the channel to drop and close the channel.
    fasync::Task::spawn(async move {
        if let Some(Ok(request)) = request_stream.next().await {
            match request {
                fcrunner::ComponentControllerRequest::Stop { control_handle }
                | fcrunner::ComponentControllerRequest::Kill { control_handle } => {
                    control_handle.shutdown();
                }
            }
        }
    })
    .detach();
}

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
}

#[async_trait]
impl Runner for RemoteRunner {
    async fn start(
        &self,
        start_info: StartInfo,
        server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
    ) {
        let resolved_url = start_info.resolved_url.clone();
        if let Err(e) = self.client.start(start_info.into(), server_end) {
            error!(url=%resolved_url, error=%e, "Failed to call runner to start component");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Namespace;
    use fidl::endpoints::{self, create_endpoints, Proxy};
    use fidl_fuchsia_data as fdata;
    use fidl_fuchsia_io as fio;

    #[fuchsia::test]
    async fn test_null_runner() {
        let null_runner = NullRunner {};
        let (client, server) = endpoints::create_endpoints::<fcrunner::ComponentControllerMarker>();
        let (_runtime, runtime_dir) = create_endpoints::<fio::DirectoryMarker>();
        null_runner
            .start(
                StartInfo {
                    resolved_url: "".to_string(),
                    program: fdata::Dictionary::default(),
                    namespace: Namespace::default(),
                    outgoing_dir: None,
                    runtime_dir: Some(runtime_dir),
                    numbered_handles: vec![],
                    encoded_config: None,
                    break_on_start: None,
                },
                server,
            )
            .await;
        let proxy = client.into_proxy().expect("failed converting to proxy");
        proxy.stop().expect("failed to send message to null runner");

        proxy.on_closed().await.expect("failed waiting for channel to close");
    }
}
