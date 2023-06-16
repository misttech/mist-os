// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::helpers::*,
    crate::results_parser::*,
    anyhow::{Context, Error},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_component_runner as frunner,
    fidl_fuchsia_test::{self as ftest, Result_ as TestResult, Status},
    fuchsia_zircon as zx,
    gtest_runner_lib::parser::*,
    std::collections::HashMap,
    test_runners_lib::cases::TestCaseInfo,
};

const DYNAMIC_SKIP_RESULT: &str = "SKIPPED";

const LIST_TESTS_ARG: &str = "list_tests";
const FILTER_ARG: &str = "filter=";
const OUTPUT_PATH: &str = "/test_data/";

/// Runs the test component with `--gunit_list_tests` and returns the parsed test cases list.
pub async fn get_cases_list_for_gtests(
    mut start_info: frunner::ComponentStartInfo,
    component_runner: &frunner::ComponentRunnerProxy,
    test_type: TestType,
) -> Result<Vec<ftest::Case>, Error> {
    // Replace the program args to get test cases in a json file.
    let list_tests_arg = format_arg(test_type, LIST_TESTS_ARG);
    let output_file_name = unique_filename();
    let output_path = format!("output=json:{}{}", OUTPUT_PATH, output_file_name);
    let output_arg = format_arg(test_type, &output_path);
    replace_program_args(
        vec![list_tests_arg, output_arg],
        start_info.program.as_mut().expect("No program."),
    );

    let (outgoing_dir, _outgoing_dir_server) = zx::Channel::create();
    start_info.outgoing_dir = Some(outgoing_dir.into());
    start_info.numbered_handles = Some(vec![]);
    let output_dir = add_output_dir_to_namespace(&mut start_info)?;

    let component_controller = start_test_component(start_info, component_runner)?;
    let _ = read_result(component_controller.take_event_stream()).await;

    // Parse tests from output file.
    let read_content = read_file(&output_dir, &output_file_name)
        .await
        .expect("Failed to read tests from output file.");
    let tests = parse_test_cases(read_content).expect("Failed to parse tests.");
    Ok(tests
        .iter()
        .map(|TestCaseInfo { name, enabled }| ftest::Case {
            name: Some(name.clone()),
            enabled: Some(*enabled),
            ..Default::default()
        })
        .collect())
}

/// Runs a gtest case associated with a single `ftest::SuiteRequest::Run` request.
///
/// Running the test component is delegated to an instance of the starnix kernel.
/// stdout logs are filtered before they're reported to the test framework.
///
/// # Parameters
/// - `tests`: The test invocations to run.
/// - `start_info`: The component start info of the test to run.
/// - `run_listener_proxy`: The proxy used to communicate results of the test run to the test
///                         framework.
/// - `component_runner`: The runner that will run the test component.
/// - `test_type`: The type of test to run, used to determine which arguments to pass to the test.
pub async fn run_gtest_cases(
    tests: Vec<ftest::Invocation>,
    mut start_info: frunner::ComponentStartInfo,
    run_listener_proxy: &ftest::RunListenerProxy,
    component_runner: &frunner::ComponentRunnerProxy,
    test_type: TestType,
) -> Result<(), Error> {
    let (numbered_handles, std_handles) = create_numbered_handles();
    start_info.numbered_handles = numbered_handles;

    // Hacky - report an overall case to see all of stdout/stderr.
    let (overall_test_listener_proxy, overall_test_listener) =
        create_proxy::<ftest::CaseListenerMarker>()?;
    run_listener_proxy.on_test_case_started(
        &ftest::Invocation {
            name: Some(start_info.resolved_url.clone().unwrap_or_default()),
            tag: None,
            ..Default::default()
        },
        std_handles,
        overall_test_listener,
    )?;

    let mut test_filter_arg = format_arg(test_type, FILTER_ARG);
    let mut run_listener_proxies = HashMap::new();
    for test in tests {
        let test_name = test.name.clone().expect("No test name.");
        test_filter_arg = format!("{}{}:", test_filter_arg, &test_name);

        let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>()?;
        run_listener_proxy.on_test_case_started(
            &test,
            ftest::StdHandles::default(),
            case_listener,
        )?;

        run_listener_proxies.insert(test_name, case_listener_proxy);
    }

    let output_file_name = unique_filename();
    let output_format = match test_type {
        TestType::Gtest | TestType::Gunit => "json",
        TestType::GtestXmlOutput => "xml",
        _ => panic!("unexpected test type"),
    };
    let output_arg = format_arg(
        test_type,
        &format!("output={}:{}{}", output_format, OUTPUT_PATH, output_file_name),
    );
    append_program_args(
        vec![test_filter_arg, output_arg],
        start_info.program.as_mut().expect("No program."),
    );

    let output_dir = add_output_dir_to_namespace(&mut start_info)?;

    // Start the test component.
    let component_controller = start_test_component(start_info, component_runner)?;
    let _ = read_result(component_controller.take_event_stream()).await;
    overall_test_listener_proxy
        .finished(&TestResult { status: Some(Status::Passed), ..Default::default() })?;

    // Parse test results.
    let test_results = read_file(&output_dir, &output_file_name)
        .await
        .with_context(|| format!("Failed to read {}", output_file_name))
        .and_then(|x| {
            parse_results(test_type, x.trim())
                .with_context(|| format!("Failed to parse {}", output_file_name))
        });

    // If tests crashes then we would fail to read or parse the output file. We
    // still want to report these cases as failed before returning the error;
    let (test_list, result) = match test_results {
        Ok(results) => (results.testsuites, Ok(())),
        Err(e) => (vec![], Err(e)),
    };

    for suite in &test_list {
        for test in &suite.testsuite {
            let name = format!("{}.{}", suite.name, test.name);
            let status = match &test.status {
                IndividualTestOutputStatus::NotRun => Status::Skipped,
                IndividualTestOutputStatus::Run => match test.result.as_str() {
                    DYNAMIC_SKIP_RESULT => Status::Skipped,
                    _ => match &test.failures {
                        Some(_failures) => Status::Failed,
                        None => Status::Passed,
                    },
                },
            };

            let case_listener_proxy = run_listener_proxies
                .remove(&name)
                .unwrap_or_else(|| panic!("No case listener for test case {name}"));
            case_listener_proxy
                .finished(&TestResult { status: Some(status), ..Default::default() })?;
        }
    }

    // Mark any tests without results as failed.
    for (name, case_listener_proxy) in run_listener_proxies {
        tracing::warn!("Did not receive result for {name}. Marking as failed.");
        case_listener_proxy
            .finished(&TestResult { status: Some(Status::Failed), ..Default::default() })?;
    }

    result
}

fn format_arg(test_type: TestType, test_arg: &str) -> String {
    match test_type {
        TestType::Gtest | TestType::GtestXmlOutput => format!("--gtest_{}", test_arg),
        TestType::Gunit => format!("--gunit_{}", test_arg),
        _ => panic!("unexpected test type"),
    }
}

fn unique_filename() -> String {
    format!("test_result-{}.json", uuid::Uuid::new_v4())
}
