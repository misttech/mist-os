// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::ClientEnd;
use fidl_test_powerelementrunner::{
    ControlRequest, ControlRequestStream, ControlStartResult, StartPowerElementError,
};
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use tracing::*;
use {fidl_fuchsia_power_broker as fbroker, fuchsia_async as fasync};

#[fuchsia::main]
async fn main() -> Result<()> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: ControlRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_power_element_runner).await;
    Ok(())
}

async fn serve_power_element_runner(mut stream: ControlRequestStream) {
    let result: Result<()> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                ControlRequest::Start {
                    initial_current_level,
                    element_name,
                    required_level_client,
                    current_level_client,
                    responder,
                } => {
                    let result = run_power_element(
                        initial_current_level,
                        element_name,
                        required_level_client,
                        current_level_client,
                    );
                    responder.send(result)?;
                }
                ControlRequest::_UnknownMethod { .. } => unimplemented!(),
            }
        }

        Ok(())
    }
    .await;

    if let Err(err) = result {
        error!("{:?}", err);
    }
}

fn run_power_element(
    initial_current_level: u8,
    element_name: String,
    required_level_client: ClientEnd<fbroker::RequiredLevelMarker>,
    current_level_client: ClientEnd<fbroker::CurrentLevelMarker>,
) -> ControlStartResult {
    let current_level_proxy = current_level_client.into_proxy().map_err(|err| {
        tracing::error!(%err, "Failed to convert current_level client_end to proxy");
        StartPowerElementError::InvalidClientEnd
    })?;
    let required_level_proxy = required_level_client.into_proxy().map_err(|err| {
        tracing::error!(%err, "Failed to convert requried_level client_end to proxy");
        StartPowerElementError::InvalidClientEnd
    })?;
    fasync::Task::local(async move {
        let mut last_required_level = initial_current_level;

        loop {
            tracing::debug!(
                ?element_name,
                ?last_required_level,
                "run_power_element: waiting for new level"
            );
            match required_level_proxy.watch().await {
                Ok(Ok(required_level)) => {
                    tracing::debug!(
                        ?element_name,
                        ?required_level,
                        ?last_required_level,
                        "run_power_element: new level requested"
                    );
                    if required_level == last_required_level {
                        tracing::debug!(
                            ?element_name,
                            ?required_level,
                            ?last_required_level,
                            "run_power_element: required level has not changed, skipping."
                        );
                        continue;
                    }

                    let res = current_level_proxy.update(required_level).await;
                    if let Err(error) = res {
                        tracing::warn!(?error, "update_fn: updating current level failed");
                    }
                    last_required_level = required_level;
                }
                error => {
                    tracing::warn!(
                        ?element_name,
                        ?error,
                        "run_power_element: watch_required_level failed"
                    );
                    break;
                }
            }
        }
    })
    .detach();
    Ok(())
}
