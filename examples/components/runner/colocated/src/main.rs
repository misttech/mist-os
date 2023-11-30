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
    let numbered_handles = start_info.numbered_handles.unwrap_or(vec![]);
    let program = ColocatedProgram::new(vmo_size, numbered_handles)?;
    let termination = program.wait_for_termination();
    Ok((program, termination))
}

/// Unit test the `ComponentRunner` protocol server implementation.
#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::Proxy;
    use fidl_fuchsia_component_decl as fdecl;
    use fidl_fuchsia_process::HandleInfo;
    use fuchsia_runtime::HandleType;

    #[fuchsia::test]
    async fn test_start_stop_component() {
        let (runner, runner_stream) =
            fidl::endpoints::create_proxy_and_stream::<fcrunner::ComponentRunnerMarker>().unwrap();
        let server = fasync::Task::spawn(handle_runner_request(runner_stream));

        // Measure how much private RAM our own process is using.
        let usage_initial = private_ram();

        // Start a component using 64 MiB of RAM.
        let decl = fuchsia_fs::file::read_in_namespace_to_fidl::<fdecl::Component>(
            "/pkg/meta/colocated-component-64mb.cm",
        )
        .await
        .unwrap();
        let (controller, controller_server_end) = fidl::endpoints::create_endpoints();
        let (user0, user0_peer) = zx::EventPair::create();
        let start_info = fcrunner::ComponentStartInfo {
            program: decl.program.unwrap().info,
            numbered_handles: Some(vec![HandleInfo {
                handle: user0_peer.into(),
                id: fuchsia_runtime::HandleInfo::new(HandleType::User0, 0).as_raw(),
            }]),
            ..Default::default()
        };
        runner.start(start_info, controller_server_end).unwrap();

        // Wait until the program has allocated 64 MiB worth of pages.
        _ = fasync::OnSignals::new(&user0, zx::Signals::USER_0).await.unwrap();

        // Measure our private memory usage again. It should increase by roughly that much more.
        let usage_started = private_ram();
        assert!(
            usage_started > usage_initial,
            "initial: {usage_initial}, started: {usage_started}"
        );
        assert!(
            usage_started - usage_initial > 60 * 1024 * 1024,
            "initial: {usage_initial}, started: {usage_started}"
        );

        // Stop the component.
        let controller = controller.into_proxy().unwrap();
        controller.stop().unwrap();
        controller.on_closed().await.unwrap();

        // Measure our private memory usage again. It should roughly go back to before.
        let usage_stopped = private_ram();
        assert!(
            usage_stopped < usage_started,
            "started: {usage_started}, stopped: {usage_stopped}"
        );
        assert!(
            usage_started - usage_stopped > 60 * 1024 * 1024,
            "started: {usage_started}, stopped: {usage_stopped}"
        );

        // Close the connection and verify the server task ends successfully.
        drop(runner);
        server.await.unwrap();
    }

    #[track_caller]
    fn private_ram() -> usize {
        let process = fuchsia_runtime::process_self();
        process.task_stats().unwrap().mem_private_bytes
    }
}
