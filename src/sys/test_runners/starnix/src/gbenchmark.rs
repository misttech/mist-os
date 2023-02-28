// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::helpers::*,
    anyhow::{anyhow, Context as _, Error},
    fidl::endpoints::create_proxy,
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio,
    fidl_fuchsia_test::{self as ftest},
    fuchsiaperf::FuchsiaPerfBenchmarkResult,
    gtest_runner_lib::parser::read_file,
    serde::{Deserialize, Serialize},
};

/// The results returned by gBenchmark.
#[derive(Serialize, Deserialize, Debug)]
pub struct BenchmarkOutput {
    pub context: BenchmarkContextData,
    pub benchmarks: Vec<BenchmarkRunData>,
}

/// The context returned by gBenchmark.
///
/// This struct is empty because its fields are not used in Fuchsiaperf results.
#[derive(Serialize, Deserialize, Debug)]
pub struct BenchmarkContextData {}

/// The benchmark results returned by gBenchmark.
///
/// More fields are reported but these are the ones needed for fuchsiaperf.
#[derive(Serialize, Deserialize, Debug)]
pub struct BenchmarkRunData {
    pub name: String,
    pub iterations: u64,
    pub real_time: f64,
    pub time_unit: String,
}

const TEST_SUITE_LABEL_KEY: &str = "test_suite_label";
const GBENCHMARK_RESULT_FILE: &str = "benchmark.json";
const FUCHSIA_PERF_RESULT_FILE: &str = "results.fuchsiaperf.json";

/// The number of benchmark iterations to report.
const MAX_BENCHMARK_COUNT: u8 = 5;

/// Runs a gbenchmark associated with a single `ftest::SuiteRequest::Run` request.
///
/// Reports results in the test component's `/custom_artifacts` to leverage
/// the test framework's exporting of custom files.
///
/// # Parameters
/// - `test`: The test invocation to run.
/// - `start_info`: The component start info of the test to run.
/// - `run_listener_proxy`: The proxy used to communicate results of the test run to the test
///                         framework.
/// - `starnix_kernel`: The kernel in which to run the test component.
pub async fn run_gbenchmark(
    test: ftest::Invocation,
    mut start_info: frunner::ComponentStartInfo,
    run_listener_proxy: &ftest::RunListenerProxy,
    starnix_kernel: &frunner::ComponentRunnerProxy,
) -> Result<(), Error> {
    let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>()?;
    let (numbered_handles, stdout_client, stderr_client) = create_numbered_handles();
    start_info.numbered_handles = numbered_handles;

    run_listener_proxy.on_test_case_started(
        test,
        ftest::StdHandles {
            out: Some(stdout_client),
            err: Some(stderr_client),
            ..ftest::StdHandles::EMPTY
        },
        case_listener,
    )?;

    let test_suite = match runner::get_value(
        start_info.program.as_ref().expect("No program"),
        TEST_SUITE_LABEL_KEY,
    ) {
        Some(fdata::DictionaryValue::Str(value)) => value.to_owned(),
        _ => return Err(anyhow!("No test suite label.")),
    };

    // Save the custom_artifacts DirectoryProxy for result reporting.
    let custom_artifacts =
        get_custom_artifacts_directory(start_info.ns.as_mut().expect("No namespace."))?;

    // Environment variables BENCHMARK_FORMAT and BENCHMARK_OUT should be set
    // so the test writes json results to this directory.
    let output_dir = add_output_dir_to_namespace(&mut start_info)?;

    // Start the test component.
    let component_controller = start_test_component(start_info, starnix_kernel)?;
    let result = read_result(component_controller.take_event_stream()).await;

    // Parse test results.
    let read_content = read_file(&output_dir, GBENCHMARK_RESULT_FILE)
        .await
        .expect("Failed to read test result file.")
        .trim()
        .to_owned();
    let perfs = gbenchmark_to_fuchsiaperf(&read_content, test_suite)?;

    // Write results to `/custom_artifacts`. The perf infrastructure looks
    // for results in this file.
    let file_proxy = fuchsia_fs::directory::open_file(
        &custom_artifacts,
        FUCHSIA_PERF_RESULT_FILE,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
    )
    .await?;
    fuchsia_fs::file::write(&file_proxy, serde_json::to_string(&perfs)?).await?;

    case_listener_proxy.finished(result)?;

    Ok(())
}

fn get_custom_artifacts_directory(
    namespace: &mut Vec<frunner::ComponentNamespaceEntry>,
) -> Result<fio::DirectoryProxy, Error> {
    for entry in namespace {
        if entry.path.as_ref().unwrap() == "/custom_artifacts" {
            return entry
                .directory
                .take()
                .unwrap()
                .into_proxy()
                .map_err(|_| anyhow!("Couldn't grab proxy."));
        }
    }

    Err(anyhow!("Couldn't find /custom artifacts."))
}

fn gbenchmark_to_fuchsiaperf(
    results: &str,
    test_suite: String,
) -> Result<Vec<FuchsiaPerfBenchmarkResult>, Error> {
    let benchmark_output: BenchmarkOutput =
        serde_json::from_str(results).context("Failed to parse benchmark results.")?;

    let mut perfs = vec![];
    let mut benchmark_count = 0;
    let mut previous_benchmark_name = " ".to_string();
    for benchmark in benchmark_output.benchmarks {
        // Conform benchmark names to Fuchsia performance metric naming style.
        // https://fuchsia.dev/fuchsia-src/development/performance/metric_naming_style
        //
        // gVisor benchmark names look like:
        // BM_GetdentsSameFD/8/real_time or BM_Sched_yield/real_time/threads:32.
        // They should end up like: GetdentsSameFd/8 or Sched_yield/threads:32.
        let label = benchmark.name.trim_start_matches("BM_").replace("/real_time", "").to_string();
        if label.starts_with(&previous_benchmark_name) {
            if benchmark_count >= MAX_BENCHMARK_COUNT {
                continue;
            }
            benchmark_count += 1;
        } else {
            previous_benchmark_name = label.split('/').collect::<Vec<_>>()[0].to_string();
            benchmark_count = 1;
        }

        perfs.push(FuchsiaPerfBenchmarkResult {
            label,
            test_suite: test_suite.clone(),
            unit: benchmark.time_unit,
            values: vec![benchmark.real_time],
        });
    }

    Ok(perfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SUITE: &str = "test_suite";

    #[test]
    fn test_gbenchmark_to_fuchsiaperf() {
        let gbenchmark = r#"{
            "context": {
              "date": "2023-02-08T18:07:17+00:00",
              "host_name": "local",
              "num_cpus": -1,
              "mhz_per_cpu": 0,
              "caches": [
              ],
              "load_avg": [],
              "library_build_type": "debug"
            },
            "benchmarks": [
              {
                "name": "BM_MetricName1/category/1",
                "family_index": 0,
                "per_family_instance_index": 0,
                "run_name": "RunName1",
                "run_type": "iteration",
                "repetitions": 1,
                "repetition_index": 0,
                "threads": 1,
                "iterations": 31798,
                "real_time": 2.1959886533832538e+04,
                "cpu_time": 2.1957717089125101e+04,
                "time_unit": "ns"
              },
              {
                "name": "BM_MetricName1/category/2",
                "family_index": 0,
                "per_family_instance_index": 1,
                "run_name": "RunName2",
                "run_type": "iteration",
                "repetitions": 1,
                "repetition_index": 0,
                "threads": 1,
                "iterations": 4648,
                "real_time": 1.2770589649314955e+06,
                "cpu_time": 1.5376424806368331e+05,
                "time_unit": "ns"
              },
              {
                "name": "BM_MetricName1/category/64/real_time",
                "family_index": 1,
                "per_family_instance_index": 0,
                "run_name": "RunName3",
                "run_type": "iteration",
                "repetitions": 1,
                "repetition_index": 0,
                "threads": 1,
                "iterations": 25687,
                "real_time": 3.1623006774103047e+04,
                "cpu_time": 3.1623803324638917e+04,
                "time_unit": "ns"
              },
              {
                "name": "BM_MetricName1/category/128/real_time",
                "family_index": 0,
                "per_family_instance_index": 2,
                "run_name": "RunName4",
                "run_type": "iteration",
                "repetitions": 1,
                "repetition_index": 0,
                "threads": 1,
                "iterations": 1000,
                "real_time": 8.2976487720006844e+06,
                "cpu_time": 1.2003764500000003e+05,
                "time_unit": "ns"
              },
              {
                "name": "BM_MetricName1/category/512/real_time",
                "family_index": 1,
                "per_family_instance_index": 1,
                "run_name": "RunName5",
                "run_type": "iteration",
                "repetitions": 1,
                "repetition_index": 0,
                "threads": 1,
                "iterations": 10000,
                "real_time": 5.4292060699663125e+04,
                "cpu_time": 5.4171954499999985e+04,
                "time_unit": "ns"
              },
              {
                "name": "BM_MetricName1/category/1024/real_time",
                "family_index": 1,
                "per_family_instance_index": 1,
                "run_name": "RunName6",
                "run_type": "iteration",
                "repetitions": 1,
                "repetition_index": 0,
                "threads": 1,
                "iterations": 10000,
                "real_time": 3.7122060699663125e+04,
                "cpu_time": 3.3291954499999985e+04,
                "time_unit": "ns"
              },
              {
                "name": "BM_MetricName2/category:1/real_time",
                "family_index": 1,
                "per_family_instance_index": 1,
                "run_name": "RunName7",
                "run_type": "iteration",
                "repetitions": 1,
                "repetition_index": 0,
                "threads": 1,
                "iterations": 10000,
                "real_time": 6.4292060699663125e+04,
                "cpu_time": 6.4171954499999985e+04,
                "time_unit": "ns"
              }
            ]
          }"#;

        let expected_perfs = vec![
            FuchsiaPerfBenchmarkResult {
                label: "MetricName1/category/1".to_string(),
                test_suite: TEST_SUITE.to_owned(),
                unit: "ns".to_string(),
                values: vec![21959.886533832538],
            },
            FuchsiaPerfBenchmarkResult {
                label: "MetricName1/category/2".to_string(),
                test_suite: TEST_SUITE.to_owned(),
                unit: "ns".to_string(),
                values: vec![1277058.9649314955],
            },
            FuchsiaPerfBenchmarkResult {
                label: "MetricName1/category/64".to_string(),
                test_suite: TEST_SUITE.to_owned(),
                unit: "ns".to_string(),
                values: vec![31623.006774103047],
            },
            FuchsiaPerfBenchmarkResult {
                label: "MetricName1/category/128".to_string(),
                test_suite: TEST_SUITE.to_owned(),
                unit: "ns".to_string(),
                values: vec![8297648.7720006844],
            },
            FuchsiaPerfBenchmarkResult {
                label: "MetricName1/category/512".to_string(),
                test_suite: TEST_SUITE.to_owned(),
                unit: "ns".to_string(),
                values: vec![54292.060699663125],
            },
            FuchsiaPerfBenchmarkResult {
                label: "MetricName2/category:1".to_string(),
                test_suite: TEST_SUITE.to_owned(),
                unit: "ns".to_string(),
                values: vec![64292.060699663125],
            },
        ];

        let perfs = gbenchmark_to_fuchsiaperf(gbenchmark, TEST_SUITE.to_owned())
            .expect("Failed to convert gbenchmark results.");
        assert_eq!(perfs.len(), expected_perfs.len());
        for i in 0..perfs.len() {
            assert_eq!(perfs[i], expected_perfs[i]);
        }
    }

    #[test]
    fn test_gbenchmark_to_fuchsiaperf_no_name() {
        let gbenchmark = r#"{
            "context": {
            },
            "benchmarks": [
              {
                "real_time": 2.1959886533832538e+04,
                "time_unit": "ns"
              },
            ]
          }"#;
        let _ = gbenchmark_to_fuchsiaperf(gbenchmark, TEST_SUITE.to_owned())
            .expect_err("Failed to parse benchmark results.");
    }

    #[test]
    fn test_gbenchmark_to_fuchsiaperf_no_time() {
        let gbenchmark = r#"{
            "context": {
            },
            "benchmarks": [
              {
                "name": "BM_MetricName1",
                "time_unit": "ns"
              },
            ]
          }"#;

        let _ = gbenchmark_to_fuchsiaperf(gbenchmark, TEST_SUITE.to_owned())
            .expect_err("Failed to parse benchmark results.");
    }

    #[test]
    fn test_gbenchmark_to_fuchsiaperf_no_time_unit() {
        let gbenchmark = r#"{
            "context": {
            },
            "benchmarks": [
              {
                "name": "BM_MetricName1",
                "real_time": 2.1959886533832538e+04,
              },
            ]
          }"#;

        let _ = gbenchmark_to_fuchsiaperf(gbenchmark, TEST_SUITE.to_owned())
            .expect_err("Failed to parse benchmark results.");
    }
}
