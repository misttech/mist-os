// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::helpers::clone_start_info, crate::test_suite::handle_suite_requests, anyhow::anyhow,
    anyhow::Error, fidl::endpoints::ServerEnd, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_test as ftest, fuchsia_async as fasync, fuchsia_component::server::ServiceFs,
    fuchsia_zircon as zx, futures::StreamExt, futures::TryStreamExt, parking_lot::Mutex,
    std::sync::Arc,
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

enum TestSuiteServices {
    Suite(ftest::SuiteRequestStream),
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
///                   the test container.
///   - `controller`: The server end of the component controller for the test component.
async fn serve_test_suite(
    mut start_info: fcrunner::ComponentStartInfo,
    controller: ServerEnd<fcrunner::ComponentControllerMarker>,
) -> Result<(), Error> {
    // Drop the runtime_dir handle because it's not supported for now.
    start_info.runtime_dir.take();
    let outgoing_dir =
        start_info.outgoing_dir.take().ok_or_else(|| anyhow!("Missing outgoing_dir"))?;

    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(TestSuiteServices::Suite);
    fs.serve_connection(outgoing_dir)?;

    let controller = Arc::new(Mutex::new(Some(controller)));
    let start_info = Arc::new(Mutex::new(start_info));

    fs.for_each_concurrent(None, |request| async {
        match request {
            TestSuiteServices::Suite(stream) => {
                let controller = controller.clone();
                let start_info = start_info.clone();
                let start_info =
                    clone_start_info(&mut start_info.lock()).expect("Failed to clone start info");
                match handle_suite_requests(start_info, stream).await {
                    Ok(_) => tracing::info!("Finished serving test suite requests."),
                    Err(e) => {
                        tracing::error!("Error serving test suite requests: {:?}", e)
                    }
                }
                let _ = controller.lock().take().map(|c| c.close_with_epitaph(zx::Status::OK));
            }
        }
    })
    .await;
    Ok(())
}
