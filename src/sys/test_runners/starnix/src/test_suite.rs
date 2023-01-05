// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::gtest::*,
    crate::helpers::*,
    anyhow::{Context, Error},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_runner as frunner,
    fidl_fuchsia_data as fdata,
    fidl_fuchsia_test::{self as ftest},
    futures::prelude::stream,
    futures::{StreamExt, TryStreamExt},
    rand::Rng,
    runner::component::ComponentNamespace,
};

/// Handles a single `ftest::SuiteRequestStream`.
///
/// # Parameters
/// - `test_url`: The URL for the test component to run.
/// - `program`: The program data associated with the runner request for the test component.
/// - `namespace`: The incoming namespace to provide to the test component.
/// - `stream`: The request stream to handle.
pub async fn handle_suite_requests(
    test_url: &str,
    program: Option<fdata::Dictionary>,
    namespace: ComponentNamespace,
    mut stream: ftest::SuiteRequestStream,
) -> Result<(), Error> {
    let is_gtest = is_gtest(&program.as_ref().expect("No program."))?;
    while let Some(event) = stream.try_next().await? {
        let mut program = program.clone();
        let runner_name = format!("starnix-runner-{}", rand::thread_rng().gen::<u64>());
        let (starnix_runner, realm) = instantiate_runner_in_realm(&namespace, &runner_name).await?;

        match event {
            ftest::SuiteRequest::GetTests { iterator, .. } => {
                let stream = iterator.into_stream()?;

                if is_gtest {
                    handle_case_iterator_for_gtests(
                        test_url,
                        program,
                        &namespace,
                        starnix_runner,
                        stream,
                    )
                    .await?;
                } else {
                    handle_case_iterator(test_url, stream).await?;
                }
            }
            ftest::SuiteRequest::Run { tests, options, listener, .. } => {
                // Replace tests with program arguments if they were passed in.
                if let Some(test_args) = options.arguments {
                    if let Some(program) = &mut program {
                        replace_program_args(test_args, program);
                    }
                }

                let run_listener_proxy =
                    listener.into_proxy().context("Can't convert run listener channel to proxy")?;
                if is_gtest {
                    let num_parallel = options.parallel.unwrap_or(1);
                    let tests = stream::iter(tests);
                    tests
                        .map(Ok)
                        .try_for_each_concurrent(num_parallel as usize, |test| {
                            run_gtest_case(
                                test,
                                test_url,
                                program.clone(),
                                &run_listener_proxy,
                                &namespace,
                            )
                        })
                        .await?;
                } else {
                    if !tests.is_empty() {
                        run_test_case(
                            tests.get(0).unwrap().clone(),
                            test_url,
                            program,
                            &run_listener_proxy,
                            &namespace,
                            &starnix_runner,
                        )
                        .await?;
                    }
                }

                run_listener_proxy.on_finished()?;
            }
        }

        realm
            .destroy_child(&mut fdecl::ChildRef {
                name: runner_name.to_string(),
                collection: Some(RUNNERS_COLLECTION.into()),
            })
            .await?
            .map_err(|e| anyhow::anyhow!("failed to destory runner child: {:?}", e))?;
    }

    Ok(())
}

/// Runs a test case associated with a single `ftest::SuiteRequest::Run` request.
///
/// Running the test component is delegated to an instance of the starnix runner.
///
/// # Parameters
/// - `tests`: The tests that are to be run. Each test executes an independent run of the test
/// component.
/// - `test_url`: The URL of the test component.
/// - `program`: The program data associated with the runner request for the test component.
/// - `run_listener_proxy`: The listener proxy for the test run.
/// - `namespace`: The incoming namespace to provide to the test component.
/// - `starnix_runner`: The instance of the starnix runner that will run the test component.
async fn run_test_case(
    test: ftest::Invocation,
    test_url: &str,
    program: Option<fdata::Dictionary>,
    run_listener_proxy: &ftest::RunListenerProxy,
    namespace: &ComponentNamespace,
    starnix_runner: &frunner::ComponentRunnerProxy,
) -> Result<(), Error> {
    let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>()?;
    let (numbered_handles, stdout_client, stderr_client) = create_numbered_handles();

    run_listener_proxy.on_test_case_started(
        test,
        ftest::StdHandles {
            out: Some(stdout_client),
            err: Some(stderr_client),
            ..ftest::StdHandles::EMPTY
        },
        case_listener,
    )?;

    let component_controller =
        start_test_component(test_url, program, namespace, numbered_handles, starnix_runner)?;

    let result = read_result(component_controller.take_event_stream()).await;
    case_listener_proxy.finished(result)?;

    Ok(())
}

/// Lists all the available test cases and returns them in response to
/// `ftest::CaseIteratorRequest::GetNext`.
///
/// Currently only one "test case" is returned, named `test_name`.
async fn handle_case_iterator(
    test_name: &str,
    mut stream: ftest::CaseIteratorRequestStream,
) -> Result<(), Error> {
    let mut cases_iter = vec![ftest::Case {
        name: Some(test_name.to_string()),
        enabled: Some(true),
        ..ftest::Case::EMPTY
    }]
    .into_iter();

    while let Some(event) = stream.try_next().await? {
        match event {
            ftest::CaseIteratorRequest::GetNext { responder } => {
                responder.send(&mut cases_iter)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::{create_request_stream, ClientEnd},
        fuchsia_async as fasync, fuchsia_zircon as zx,
        futures::TryStreamExt,
        std::convert::TryFrom,
    };

    /// Returns a `ftest::CaseIteratorProxy` that is served by `super::handle_case_iterator`.
    ///
    /// # Parameters
    /// - `test_name`: The name of the test case that is provided to `handle_case_iterator`.
    fn set_up_iterator(test_name: &str) -> ftest::CaseIteratorProxy {
        let test_name = test_name.to_string();
        let (iterator_client_end, iterator_stream) =
            create_request_stream::<ftest::CaseIteratorMarker>()
                .expect("Couldn't create case iterator");
        fasync::Task::local(async move {
            let _ = handle_case_iterator(&test_name, iterator_stream).await;
        })
        .detach();

        iterator_client_end.into_proxy().expect("Failed to create proxy")
    }

    /// Spawns a `ComponentRunnerRequestStream` server that immediately closes all incoming
    /// component controllers with the epitaph specified in `component_controller_epitaph`.
    ///
    /// This function can be used to mock the starnix runner in a way that simulates a component
    /// exiting with or without error.
    ///
    /// # Parameters
    /// - `component_controller_epitaph`: The epitaph used to close the component controller.
    ///
    /// # Returns
    /// A `ComponentRunnerProxy` that serves each run request by closing the component with the
    /// provided epitaph.
    fn spawn_runner(component_controller_epitaph: zx::Status) -> frunner::ComponentRunnerProxy {
        let (proxy, mut request_stream) =
            fidl::endpoints::create_proxy_and_stream::<frunner::ComponentRunnerMarker>().unwrap();
        fasync::Task::local(async move {
            while let Some(event) =
                request_stream.try_next().await.expect("Error in test runner request stream")
            {
                match event {
                    frunner::ComponentRunnerRequest::Start {
                        start_info: _start_info,
                        controller,
                        ..
                    } => {
                        controller
                            .close_with_epitaph(component_controller_epitaph)
                            .expect("Could not close with epitaph");
                    }
                }
            }
        })
        .detach();
        proxy
    }

    /// Returns the status from the first test case reported to `run_listener_stream`.
    ///
    /// This is done by listening to the first `CaseListener` provided via `OnTestCaseStarted`.
    ///
    /// # Parameters
    /// - `run_listener_stream`: The run listener stream to extract the test status from.
    ///
    /// # Returns
    /// The status of the first test case that is run, or `None` if no such status is reported.
    async fn listen_to_test_result(
        mut run_listener_stream: ftest::RunListenerRequestStream,
    ) -> Option<ftest::Status> {
        match run_listener_stream.try_next().await.expect("..") {
            Some(ftest::RunListenerRequest::OnTestCaseStarted {
                invocation: _,
                std_handles: _,
                listener,
                ..
            }) => match listener
                .into_stream()
                .expect("Failed to get case listener stream")
                .try_next()
                .await
                .expect("Failed to get case listener stream request")
            {
                Some(ftest::CaseListenerRequest::Finished { result, .. }) => result.status,
                _ => None,
            },
            _ => None,
        }
    }

    /// Spawns a task that calls `super::run_test_cases` with the provided `run_listener` and
    /// `runner_proxy`. The call is made with a mock test case.
    fn spawn_run_test_cases(
        run_listener: ClientEnd<ftest::RunListenerMarker>,
        starnix_runner: frunner::ComponentRunnerProxy,
    ) {
        fasync::Task::local(async move {
            let _ = run_test_case(
                ftest::Invocation {
                    name: Some("".to_string()),
                    tag: Some("".to_string()),
                    ..ftest::Invocation::EMPTY
                },
                "",
                None,
                &run_listener.into_proxy().expect("Couldn't create proxy."),
                &ComponentNamespace::try_from(vec![]).expect(""),
                &starnix_runner,
            )
            .await;
        })
        .detach();
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_number_of_test_cases() {
        let iterator_proxy = set_up_iterator("test");
        let first_result = iterator_proxy.get_next().await.expect("Didn't get first result");
        let second_result = iterator_proxy.get_next().await.expect("Didn't get second result");

        assert_eq!(first_result.len(), 1);
        assert_eq!(second_result.len(), 0);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_case_name() {
        let test_name = "test_name";
        let iterator_proxy = set_up_iterator(test_name);
        let result = iterator_proxy.get_next().await.expect("Didn't get first result");
        assert_eq!(result[0].name, Some(test_name.to_string()));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_case_enabled() {
        let iterator_proxy = set_up_iterator("test");
        let result = iterator_proxy.get_next().await.expect("Didn't get first result");
        assert_eq!(result[0].enabled, Some(true));
    }

    /// Tests that when starnix closes the component controller with an `OK` status, the test case
    /// passes.
    #[fasync::run_singlethreaded(test)]
    async fn test_component_controller_epitaph_ok() {
        let starnix_runner = spawn_runner(zx::Status::OK);
        let (run_listener, run_listener_stream) =
            create_request_stream::<ftest::RunListenerMarker>()
                .expect("Couldn't create case listener");
        spawn_run_test_cases(run_listener, starnix_runner);
        assert_eq!(listen_to_test_result(run_listener_stream).await, Some(ftest::Status::Passed));
    }

    /// Tests that when starnix closes the component controller with an error status, the test case
    /// fails.
    #[fasync::run_singlethreaded(test)]
    async fn test_component_controller_epitaph_not_ok() {
        let starnix_runner = spawn_runner(zx::Status::INTERNAL);
        let (run_listener, run_listener_stream) =
            create_request_stream::<ftest::RunListenerMarker>()
                .expect("Couldn't create case listener");
        spawn_run_test_cases(run_listener, starnix_runner);
        assert_eq!(listen_to_test_result(run_listener_stream).await, Some(ftest::Status::Failed));
    }
}
