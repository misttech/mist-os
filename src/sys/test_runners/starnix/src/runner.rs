// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::test_suite::handle_suite_requests,
    anyhow::{anyhow, Error},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_test as ftest, fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_zircon as zx,
    futures::{StreamExt, TryStreamExt},
    std::sync::Arc,
};

/// The services exposed in the outgoing directory for the test component.
enum TestComponentExposedServices {
    Suite(ftest::SuiteRequestStream),
}

/// Handles a `fcrunner::ComponentRunnerRequestStream`.
///
/// When a run request arrives, the test suite protocol is served in the test component's outgoing
/// namespace, and then the component is run in response to `ftest::SuiteRequest::Run` requests.
///
/// See `test_suite` for more on how the test suite requests are handled.
pub async fn handle_runner_requests(
    kernels_dir: Arc<vfs::directory::immutable::Simple>,
    mut request_stream: fcrunner::ComponentRunnerRequestStream,
) -> Result<(), Error> {
    while let Some(event) = request_stream.try_next().await? {
        match event {
            fcrunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                let kernels_dir = kernels_dir.clone();
                fasync::Task::local(async move {
                    match serve_test_suite(&kernels_dir, start_info, controller).await {
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

/// Serves a `ftest::SuiteRequestStream` from `directory_channel`.
///
/// This function is used to serve a `ftest::SuiteRequestStream` in the outgoing directory of a test
/// component. This is what the test framework connects to to run test cases.
///
/// When the returned future completes, the outgoing directory has finished serving requests.
///
/// # Parameters
///   - `kernels_dir`: The root directory for the starnix_kernel instance's configuration.
///   - `start_info`: The start info associated with the test component, used to instantiate
///                   the starnix_kernel.
///   - `controller`: The server end of the component controller for the test component.
async fn serve_test_suite(
    kernels_dir: &vfs::directory::immutable::Simple,
    mut start_info: fcrunner::ComponentStartInfo,
    controller: ServerEnd<fcrunner::ComponentControllerMarker>,
) -> Result<(), Error> {
    // Serve the test suite in the outgoing directory of the test component.
    let mut outgoing_dir = ServiceFs::new_local();
    outgoing_dir.dir("svc").add_fidl_service(TestComponentExposedServices::Suite);
    let outgoing_dir_channel =
        start_info.outgoing_dir.take().ok_or(anyhow!("Missing outgoing directory"))?.into();
    outgoing_dir.serve_connection(outgoing_dir_channel)?;

    if let Some(service_request) = outgoing_dir.next().await {
        match service_request {
            TestComponentExposedServices::Suite(stream) => {
                match handle_suite_requests(kernels_dir, start_info, stream).await {
                    Ok(_) => tracing::info!("Finished serving test suite requests."),
                    Err(e) => {
                        tracing::error!("Error serving test suite requests: {:?}", e)
                    }
                }
                let _ = controller.close_with_epitaph(zx::Status::OK);
            }
        }
    }

    Ok(())
}
