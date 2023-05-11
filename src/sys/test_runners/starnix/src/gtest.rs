// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::helpers::*,
    crate::results_parser::*,
    anyhow::{anyhow, Error},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_data as fdata,
    fidl_fuchsia_test::{self as ftest, Result_ as TestResult, Status},
    fuchsia_zircon as zx,
    fuchsia_zircon::sys::ZX_CHANNEL_MAX_MSG_BYTES,
    futures::TryStreamExt,
    gtest_runner_lib::parser::*,
    rust_measure_tape_for_case::Measurable as _,
    std::collections::HashMap,
    test_runners_lib::cases::TestCaseInfo,
    test_runners_lib::elf::SuiteServerError,
};

const DYNAMIC_SKIP_RESULT: &str = "SKIPPED";

const LIST_TESTS_ARG: &str = "list_tests";
const FILTER_ARG: &str = "filter=";
const OUTPUT_PATH: &str = "/test_data/";

#[derive(PartialEq)]
pub enum TestType {
    BinderLatency,
    Gbenchmark,
    Gtest,
    Gunit,
    GtestXmlOutput,
    Unknown,
}

impl TestType {
    pub fn is_gtest_like(&self) -> bool {
        match self {
            TestType::Gtest | TestType::Gunit | TestType::GtestXmlOutput => true,
            _ => false,
        }
    }
}

/// Determines what type of tests the program is.
pub fn test_type(program: &fdata::Dictionary) -> TestType {
    // The program argument that specifies if the test is a gtest.
    const GTEST_KEY: &str = "test_type";
    let test_type_val = runner::get_value(program, GTEST_KEY);
    match test_type_val {
        Some(fdata::DictionaryValue::Str(value)) => match value.as_str() {
            "binder_latency" => TestType::BinderLatency,
            "gtest" => TestType::Gtest,
            "gunit" => TestType::Gunit,
            "gtest_xml_output" => TestType::GtestXmlOutput,
            "gbenchmark" => TestType::Gbenchmark,
            _ => TestType::Unknown,
        },
        _ => TestType::Unknown,
    }
}

/// Runs the test component with `--gunit_list_tests` and returns the parsed test cases
/// in response to `ftest::CaseIteratorRequest::GetNext`.
pub async fn handle_case_iterator_for_gtests(
    mut start_info: frunner::ComponentStartInfo,
    starnix_kernel: &frunner::ComponentRunnerProxy,
    mut stream: ftest::CaseIteratorRequestStream,
) -> Result<(), Error> {
    // Replace the program args to get test cases in a json file.
    let test_type = test_type(start_info.program.as_ref().unwrap());
    let list_tests_arg = format_arg(&test_type, LIST_TESTS_ARG)?;
    let output_file_name = unique_filename();
    let output_path = format!("output=json:{}{}", OUTPUT_PATH, output_file_name);
    let output_arg = format_arg(&test_type, &output_path)?;
    replace_program_args(
        vec![list_tests_arg, output_arg],
        start_info.program.as_mut().expect("No program."),
    );

    let (outgoing_dir, _outgoing_dir_server) = zx::Channel::create();
    start_info.outgoing_dir = Some(outgoing_dir.into());
    start_info.numbered_handles = Some(vec![]);
    let output_dir = add_output_dir_to_namespace(&mut start_info)?;

    let component_controller = start_test_component(start_info, starnix_kernel)?;
    let _ = read_result(component_controller.take_event_stream()).await;

    // Parse tests from output file.
    let read_content = read_file(&output_dir, &output_file_name)
        .await
        .expect("Failed to read tests from output file.");
    let tests = parse_test_cases(read_content).expect("Failed to parse tests.");
    let cases: Vec<_> = tests
        .iter()
        .map(|TestCaseInfo { name, enabled }| ftest::Case {
            name: Some(name.clone()),
            enabled: Some(*enabled),
            ..Default::default()
        })
        .collect();
    let mut remaining_cases = &cases[..];

    while let Some(event) = stream.try_next().await? {
        match event {
            ftest::CaseIteratorRequest::GetNext { responder } => {
                // Paginate cases
                // Page overhead of message header + vector
                let mut bytes_used: usize = 32;
                let mut case_count = 0;
                for case in remaining_cases {
                    bytes_used += case.measure().num_bytes;
                    if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize {
                        break;
                    }
                    case_count += 1;
                }
                responder
                    .send(&remaining_cases[..case_count])
                    .map_err(SuiteServerError::Response)?;
                remaining_cases = &remaining_cases[case_count..];
            }
        }
    }

    Ok(())
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
/// - `starnix_kernel`: The kernel in which to run the test component.
/// - `test_type`: The type of test to run, used to determine which arguments to pass to the test.
pub async fn run_gtest_cases(
    tests: Vec<ftest::Invocation>,
    mut start_info: frunner::ComponentStartInfo,
    run_listener_proxy: &ftest::RunListenerProxy,
    starnix_kernel: &frunner::ComponentRunnerProxy,
    test_type: &TestType,
) -> Result<(), Error> {
    let (numbered_handles, stdout_client, stderr_client) = create_numbered_handles();
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
        ftest::StdHandles {
            out: Some(stdout_client),
            err: Some(stderr_client),
            ..Default::default()
        },
        overall_test_listener,
    )?;

    let mut test_filter_arg = format_arg(test_type, FILTER_ARG)?;
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
        TestType::Gbenchmark | TestType::BinderLatency | TestType::Unknown => {
            panic!("unexpected type")
        }
    };
    let output_arg = format_arg(
        &test_type,
        &format!("output={}:{}{}", output_format, OUTPUT_PATH, output_file_name),
    )?;
    append_program_args(
        vec![test_filter_arg, output_arg],
        start_info.program.as_mut().expect("No program."),
    );

    let output_dir = add_output_dir_to_namespace(&mut start_info)?;

    // Start the test component.
    let component_controller = start_test_component(start_info, starnix_kernel)?;
    let _ = read_result(component_controller.take_event_stream()).await;
    overall_test_listener_proxy
        .finished(&TestResult { status: Some(Status::Passed), ..Default::default() })?;

    // Parse test results.
    let mut read_content = read_file(&output_dir, &output_file_name)
        .await
        .expect("Failed to read test result file.")
        .to_string();
    read_content = read_content.trim().to_string();
    let test_list = parse_results(test_type, &read_content)?;

    for suite in &test_list.testsuites {
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

    Ok(())
}

fn format_arg(test_type: &TestType, test_arg: &str) -> Result<String, Error> {
    match test_type {
        TestType::Gtest | TestType::GtestXmlOutput => Ok(format!("--gtest_{}", test_arg)),
        TestType::Gunit => Ok(format!("--gunit_{}", test_arg)),
        TestType::Gbenchmark | TestType::BinderLatency | TestType::Unknown => {
            Err(anyhow!("Unknown test type"))
        }
    }
}

fn unique_filename() -> String {
    format!("test_result-{}.json", uuid::Uuid::new_v4())
}
