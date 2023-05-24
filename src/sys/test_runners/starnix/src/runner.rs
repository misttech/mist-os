// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::helpers::clone_start_info,
    crate::test_suite::handle_suite_requests,
    anyhow::{anyhow, Error},
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_io as fio, fidl_fuchsia_test as ftest,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::TryStreamExt,
    parking_lot::Mutex,
    std::sync::Arc,
    vfs::directory::{entry::DirectoryEntry, helper::DirectlyMutable},
};

/// Handles a `fcrunner::ComponentRunnerRequestStream`.
///
/// When a run request arrives, the test suite protocol is served in the test component's outgoing
/// namespace, and then the component is run in response to `ftest::SuiteRequest::Run` requests.
///
/// See `test_suite` for more on how the test suite requests are handled.
pub async fn handle_runner_requests(
    mut request_stream: fcrunner::ComponentRunnerRequestStream,
) -> Result<(), Error> {
    while let Some(event) = request_stream.try_next().await? {
        match event {
            fcrunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                fasync::Task::local(async move {
                    match serve_test_suite(start_info, controller).await {
                        Ok(_) => tracing::info!("Finished serving test suite for component."),
                        Err(e) => tracing::error!("Error serving test suite: {:?}", e),
                    }
                })
                .detach();
            }
        }
    }

    Ok(())
}

/// Serves a `ftest::SuiteRequestStream` within the `outgoing_dir` of `start_info`.
///
/// This function is used to serve a `ftest::SuiteRequestStream` in the outgoing directory of a test
/// component. This is what the test framework connects to to run test cases.
///
/// When the returned future completes, the outgoing directory has finished serving requests.
///
/// # Parameters
///   - `start_info`: The start info associated with the test component, used to instantiate
///                   the starnix_kernel.
///   - `controller`: The server end of the component controller for the test component.
async fn serve_test_suite(
    mut start_info: fcrunner::ComponentStartInfo,
    controller: ServerEnd<fcrunner::ComponentControllerMarker>,
) -> Result<(), Error> {
    // The name of the directory capability that is offered to the starnix_kernel instances.
    const KERNELS_DIRECTORY: &str = "kernels";
    const SVC_DIRECTORY: &str = "svc";

    let outgoing_dir = vfs::directory::immutable::simple();
    let kernels_dir = vfs::directory::immutable::simple();
    // Add the directory that is offered to starnix_kernel instances, to read their configuration
    // from.
    outgoing_dir.add_entry(KERNELS_DIRECTORY, kernels_dir.clone())?;

    let outgoing_dir_server_end =
        start_info.outgoing_dir.take().ok_or(anyhow!("Missing outgoing directory"))?;

    // Drop the runtime_dir handle because it's not supported for now.
    start_info.runtime_dir.take();

    // Wrap controller in an option, since closing it will consume the server end.
    let controller = Arc::new(Mutex::new(Some(controller)));
    let start_info = Arc::new(Mutex::new(start_info));

    let svc_dir = vfs::directory::immutable::simple();
    svc_dir.add_entry(
        ftest::SuiteMarker::PROTOCOL_NAME,
        vfs::service::host(move |requests| {
            let kernels_dir = kernels_dir.clone();
            let controller = controller.clone();
            let start_info = start_info.clone();
            async move {
                let start_info =
                    clone_start_info(&mut start_info.lock()).expect("Failed to clone start info");
                match handle_suite_requests(&kernels_dir, start_info, requests).await {
                    Ok(_) => tracing::info!("Finished serving test suite requests."),
                    Err(e) => {
                        tracing::error!("Error serving test suite requests: {:?}", e)
                    }
                }
                let _ = controller.lock().take().map(|c| c.close_with_epitaph(zx::Status::OK));
            }
        }),
    )?;
    // Serve the test suite in the outgoing directory of the test component.
    outgoing_dir.add_entry(SVC_DIRECTORY, svc_dir.clone())?;

    let execution_scope = vfs::execution_scope::ExecutionScope::new();
    outgoing_dir.open(
        execution_scope.clone(),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        vfs::path::Path::dot(),
        outgoing_dir_server_end.into_channel().into(),
    );
    execution_scope.wait().await;

    Ok(())
}
