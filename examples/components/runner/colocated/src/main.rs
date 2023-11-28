// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_runner as fcrunner;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_zircon as zx;
use futures::{StreamExt, TryStreamExt};
use runner::component::{ChannelEpitaph, Controllable, Controller};
use std::future::Future;
use tracing::{info, warn};

mod program;

use crate::program::ColocatedProgram;

const VMO_SIZE: &str = "vmo_size";

enum IncomingRequest {
    Runner(fcrunner::ComponentRunnerRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<()> {
    let mut service_fs = ServiceFs::new_local();
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Runner);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async move {
            match request {
                IncomingRequest::Runner(stream) => {
                    if let Err(err) = handle_runner_request(stream).await {
                        warn!("Error while serving ComponentRunner: {err}");
                    }
                }
            }
        })
        .await;

    Ok(())
}

/// Handles `fuchsia.component.runner/ComponentRunner` requests over a FIDL connection.
async fn handle_runner_request(mut stream: fcrunner::ComponentRunnerRequestStream) -> Result<()> {
    while let Some(request) =
        stream.try_next().await.context("failed to serve ComponentRunner protocol")?
    {
        let fcrunner::ComponentRunnerRequest::Start { start_info, controller, .. } = request;
        let url = start_info.resolved_url.clone().unwrap_or_else(|| "unknown url".to_string());
        info!("Colocated runner is going to start component {url}");
        match start(start_info) {
            Ok((program, on_exit)) => {
                let controller = Controller::new(program, controller.into_stream().unwrap());
                fasync::Task::spawn(controller.serve(on_exit)).detach();
            }
            Err(err) => {
                warn!("Colocated runner failed to start component {url}: {err}");
                let _ = controller.close_with_epitaph(zx::Status::from_raw(
                    fcomponent::Error::Internal.into_primitive() as i32,
                ));
            }
        }
    }

    Ok(())
}

/// Starts a colocated component.
fn start(
    start_info: fcrunner::ComponentStartInfo,
) -> Result<(impl Controllable, impl Future<Output = ChannelEpitaph> + Unpin)> {
    let vmo_size = runner::get_program_string(&start_info, VMO_SIZE)
        .ok_or(anyhow!("Missing vmo_size argument in program block"))?;
    let vmo_size: u64 = vmo_size.parse().context("vmo_size is not a valid number")?;

    let program = ColocatedProgram::new(vmo_size)?;
    let termination = program.wait_for_termination();
    Ok((program, termination))
}
