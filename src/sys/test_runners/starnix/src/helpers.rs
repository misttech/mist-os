// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context as _, Error};
use fidl::endpoints::{create_proxy, Proxy};
use fidl::HandleBased;
use fidl_fuchsia_test::{self as ftest};
use frunner::ComponentNamespaceEntry;
use fuchsiaperf::FuchsiaPerfBenchmarkResult;
use futures::StreamExt;
use gtest_runner_lib::parser::read_file;
use namespace::Namespace;
use tracing::debug;
use {
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio,
    fidl_fuchsia_process as fprocess, fuchsia_runtime as fruntime,
};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TestType {
    BinderLatency,
    Gbenchmark,
    Gtest,
    GtestXmlOutput,
    Gunit,
    Ltp,
    SeLinux,
    SingleTest,
}

pub fn get_opt_str_value_from_dict(
    dict: &fdata::Dictionary,
    name: &str,
) -> Result<Option<String>, Error> {
    match runner::get_value(dict, name) {
        Some(fdata::DictionaryValue::Str(value)) => Ok(Some(value.clone())),
        Some(_) => Err(anyhow!("{} must a string", name)),
        _ => Ok(None),
    }
}

pub fn get_str_value_from_dict(dict: &fdata::Dictionary, name: &str) -> Result<String, Error> {
    match get_opt_str_value_from_dict(dict, name)? {
        Some(s) => Ok(s),
        None => Err(anyhow!("{} is not specified", name)),
    }
}

pub async fn run_starnix_benchmark(
    test: ftest::Invocation,
    mut start_info: frunner::ComponentStartInfo,
    run_listener_proxy: &ftest::RunListenerProxy,
    component_runner: &frunner::ComponentRunnerProxy,
    test_data_path: &str,
    converter: impl FnOnce(&str, &str) -> Result<Vec<FuchsiaPerfBenchmarkResult>, Error>,
) -> Result<(), Error> {
    let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>()?;
    let (numbered_handles, std_handles) = create_numbered_handles();
    start_info.numbered_handles = numbered_handles;

    debug!("notifying client test case started");
    run_listener_proxy.on_test_case_started(&test, std_handles, case_listener)?;

    debug!("getting test suite label");
    let program = start_info.program.as_ref().context("No program")?;
    let test_suite = get_str_value_from_dict(program, "test_suite_label")?;

    // Save the custom_artifacts DirectoryProxy for result reporting.
    let custom_artifacts =
        get_custom_artifacts_directory(start_info.ns.as_mut().expect("No namespace."))?;

    // Environment variables BENCHMARK_FORMAT and BENCHMARK_OUT should be set
    // so the test writes json results to this directory.
    let output_dir = add_output_dir_to_namespace(&mut start_info)?;

    // Start the test component.
    let component_controller = start_test_component(start_info, component_runner)?;
    let result = read_result(component_controller.take_event_stream()).await;

    // Read the output it produced.
    let test_data = read_file(&output_dir, test_data_path)
        .await
        .with_context(|| format!("reading {test_data_path} from test data"))?
        .trim()
        .to_owned();

    let perfs =
        converter(&test_data, &test_suite).context("converting test output to fuchsiaperf")?;

    // Write JSON to custom artifacts directory where perf test infra expects it.
    let file_proxy = fuchsia_fs::directory::open_file(
        &custom_artifacts,
        "results.fuchsiaperf.json",
        fio::PERM_WRITABLE | fio::Flags::FLAG_MAYBE_CREATE,
    )
    .await?;
    fuchsia_fs::file::write(&file_proxy, serde_json::to_string(&perfs)?).await?;

    case_listener_proxy.finished(&result)?;

    Ok(())
}

/// Replace the arguments in `program` with `test_arguments`, which were provided to the test
/// framework directly.
pub fn replace_program_args(test_arguments: Vec<String>, program: &mut fdata::Dictionary) {
    update_program_args(test_arguments, program, false);
}

/// Insert `new_args` into the arguments in `program`.
pub fn append_program_args(new_args: Vec<String>, program: &mut fdata::Dictionary) {
    update_program_args(new_args, program, true);
}

/// Clones relevant fields of `start_info`. `&mut` is required to clone the `ns` field, but the
/// `start_info` is not modified.
///
/// The `outgoing_dir` of the result will be present, but unusable.
pub fn clone_start_info(
    start_info: &mut frunner::ComponentStartInfo,
) -> Result<frunner::ComponentStartInfo, Error> {
    let ns = Namespace::try_from(start_info.ns.take().unwrap())?;
    // Reset the namespace of the start_info, since it was moved out above.
    start_info.ns = Some(ns.clone().try_into()?);

    let (outgoing_dir, _outgoing_dir_server) = zx::Channel::create();

    Ok(frunner::ComponentStartInfo {
        resolved_url: start_info.resolved_url.clone(),
        program: start_info.program.clone(),
        ns: Some(ns.try_into()?),
        outgoing_dir: Some(outgoing_dir.into()),
        runtime_dir: None,
        numbered_handles: Some(vec![]),
        encoded_config: None,
        break_on_start: None,
        ..Default::default()
    })
}

/// Creates numbered handles for a test component with a respective `StdHandles` that should be
/// passed to the `RunListener`.
pub fn create_numbered_handles() -> (Option<Vec<fprocess::HandleInfo>>, ftest::StdHandles) {
    let (test_stdin, _) = zx::Socket::create_stream();
    let (test_stdout, stdout_client) = zx::Socket::create_stream();
    let (test_stderr, stderr_client) = zx::Socket::create_stream();
    let stdin_handle_info = fprocess::HandleInfo {
        handle: test_stdin.into_handle(),
        id: fruntime::HandleInfo::new(fruntime::HandleType::FileDescriptor, 0).as_raw(),
    };
    let stdout_handle_info = fprocess::HandleInfo {
        handle: test_stdout.into_handle(),
        id: fruntime::HandleInfo::new(fruntime::HandleType::FileDescriptor, 1).as_raw(),
    };
    let stderr_handle_info = fprocess::HandleInfo {
        handle: test_stderr.into_handle(),
        id: fruntime::HandleInfo::new(fruntime::HandleType::FileDescriptor, 2).as_raw(),
    };

    let numbered_handles = Some(vec![stdin_handle_info, stdout_handle_info, stderr_handle_info]);
    let std_handles = ftest::StdHandles {
        out: Some(stdout_client),
        err: Some(stderr_client),
        ..Default::default()
    };

    (numbered_handles, std_handles)
}

/// Starts the test component and returns its proxy.
pub fn start_test_component(
    start_info: frunner::ComponentStartInfo,
    component_runner: &frunner::ComponentRunnerProxy,
) -> Result<frunner::ComponentControllerProxy, Error> {
    let (component_controller, component_controller_server_end) =
        create_proxy::<frunner::ComponentControllerMarker>()?;

    debug!(?start_info, "asking kernel to start component");
    component_runner.start(start_info, component_controller_server_end)?;

    Ok(component_controller)
}

/// Reads the epitaph from the provided `event_stream`.
pub async fn read_component_epitaph(
    mut event_stream: frunner::ComponentControllerEventStream,
) -> zx::Status {
    match event_stream.next().await {
        Some(Err(fidl::Error::ClientChannelClosed { status, .. })) => status,
        result => {
            tracing::error!(
                "Didn't get epitaph from the component controller, instead got: {:?}",
                result
            );
            // Fail the test case here, since the component controller's epitaph couldn't be
            // read.
            zx::Status::INTERNAL
        }
    }
}

/// Reads the result of the test run from `event_stream`.
///
/// The result is determined by reading the epitaph from the provided `event_stream`.
pub async fn read_result(event_stream: frunner::ComponentControllerEventStream) -> ftest::Result_ {
    match read_component_epitaph(event_stream).await {
        zx::Status::OK => {
            ftest::Result_ { status: Some(ftest::Status::Passed), ..Default::default() }
        }
        _ => ftest::Result_ { status: Some(ftest::Status::Failed), ..Default::default() },
    }
}

pub fn add_output_dir_to_namespace(
    start_info: &mut frunner::ComponentStartInfo,
) -> Result<fio::DirectoryProxy, Error> {
    const TEST_DATA_DIR: &str = "/tmp/test_data";

    let test_data_path = format!("{}/{}", TEST_DATA_DIR, uuid::Uuid::new_v4());
    std::fs::create_dir_all(&test_data_path).expect("cannot create test output directory.");
    let data_dir_proxy = fuchsia_fs::directory::open_in_namespace(
        &test_data_path,
        fio::PERM_READABLE | fio::PERM_WRITABLE,
    )
    .expect("Cannot open test data directory.");

    let data_dir = data_dir_proxy.into_client_end().expect("Cannot get client end from proxy.");
    start_info.ns.as_mut().ok_or(anyhow!("Missing namespace."))?.push(ComponentNamespaceEntry {
        path: Some("/test_data".to_string()),
        directory: Some(data_dir),
        ..Default::default()
    });

    let test_data_dir =
        fuchsia_fs::directory::open_in_namespace(&test_data_path, fio::PERM_READABLE)
            .expect("Cannot open test data directory.");
    Ok(test_data_dir)
}

/// Replace or append the arguments in `program` with `new_args`.
fn update_program_args(mut new_args: Vec<String>, program: &mut fdata::Dictionary, append: bool) {
    /// The program argument key name.
    const ARGS_KEY: &str = "args";

    if new_args.is_empty() {
        return;
    }

    let mut new_entry = fdata::DictionaryEntry {
        key: ARGS_KEY.to_string(),
        value: Some(Box::new(fdata::DictionaryValue::StrVec(new_args.clone()))),
    };
    if let Some(entries) = &mut program.entries {
        if let Some(index) = entries.iter().position(|entry| entry.key == ARGS_KEY) {
            let entry = entries.remove(index);

            if append {
                if let Some(mut box_value) = entry.value {
                    if let fdata::DictionaryValue::StrVec(ref mut args) = &mut *box_value {
                        args.append(&mut new_args);
                        new_entry.value =
                            Some(Box::new(fdata::DictionaryValue::StrVec(args.to_vec())));
                    };
                }
            }
        }
        entries.push(new_entry);
    } else {
        let entries = vec![new_entry];
        program.entries = Some(entries);
    };
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

pub async fn read_tests_list(
    start_info: &mut frunner::ComponentStartInfo,
) -> Result<Vec<TestDefinition>, Error> {
    let program = start_info.program.as_ref().unwrap();
    let tests_list_file = get_str_value_from_dict(program, "tests_list")?;
    Ok(read_file_from_component_ns(start_info, &tests_list_file)
        .await
        .with_context(|| format!("Failed to read {}", tests_list_file))?
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(parse_test_definition)
        .collect())
}

pub struct TestDefinition {
    pub name: String,
    pub command: String,
}

pub fn parse_test_definition(test_def: &str) -> TestDefinition {
    let test_def = test_def.trim();
    match test_def.split_once(' ') {
        Some((name, command)) => {
            TestDefinition { name: name.to_string(), command: command.trim().to_string() }
        }
        None => TestDefinition { name: test_def.to_string(), command: test_def.to_string() },
    }
}

pub async fn read_file_from_dir(dir: &fio::DirectoryProxy, path: &str) -> Result<String, Error> {
    let file_proxy = fuchsia_fs::directory::open_file_async(&dir, path, fuchsia_fs::PERM_READABLE)?;
    fuchsia_fs::file::read_to_string(&file_proxy).await.map_err(Into::into)
}

pub async fn read_file_from_component_ns(
    start_info: &mut frunner::ComponentStartInfo,
    path: &str,
) -> Result<String, Error> {
    for entry in start_info.ns.as_mut().ok_or(anyhow!("Component NS is not set"))?.iter_mut() {
        if entry.path == Some("/pkg".to_string()) {
            let dir = entry.directory.take().ok_or(anyhow!("NS entry directory is not set"))?;
            let dir_proxy = dir.into_proxy()?;

            let result = read_file_from_dir(&dir_proxy, path).await;

            // Return the directory back to the `start_info`.
            entry.directory = Some(fidl::endpoints::ClientEnd::new(
                dir_proxy.into_channel().unwrap().into_zx_channel(),
            ));

            return result;
        }
    }

    Err(anyhow!("/pkg is not in the namespace"))
}
