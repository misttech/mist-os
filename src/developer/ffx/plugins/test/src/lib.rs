// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod connector;
mod suite_definition;

use crate::{connector::RunConnector, suite_definition::TestParamsOptions};
use anyhow::{anyhow, format_err, Context, Result};
use either::Either;
use errors::{ffx_bail, ffx_bail_with_code, ffx_error, ffx_error_with_code, FfxError};
use ffx_core::ffx_plugin;
use ffx_test_args::{ListCommand, RunCommand, TestCommand, TestSubCommand};
use fidl::endpoints::create_proxy;
use fidl_fuchsia_developer_remotecontrol as fremotecontrol;
use fidl_fuchsia_test_manager as ftest_manager;
use futures::FutureExt;
use lazy_static::lazy_static;
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    iterator::Signals,
};
use std::{
    fmt::Debug,
    io::{stdout, Write},
};

lazy_static! {
    /// Error code returned if connecting to Test Manager fails.
    pub static ref SETUP_FAILED_CODE: i32 = -fidl::Status::UNAVAILABLE.into_raw();
    /// Error code returned if tests time out.
    pub static ref TIMED_OUT_CODE: i32 = -fidl::Status::TIMED_OUT.into_raw();
}

/// Max number of test suites to run using a single RunBuilder connection.
/// Since we need to make n SuiteController channels when running tests on a
/// single RunBuilder channel, this max limits the number of channels that need
/// to be opened before any tests are run, allowing tests to start running faster.
/// It also limits the maximum resources that overnet needs to handle at once.
/// This isn't set to 1 as (1 RunBuilder connection) = (1 set of debug data). Since
/// pulling debug data off device is expensive we also want to limit the number of
/// times this occurs.
const SUITE_BATCH_SIZE: usize = 100;

#[ffx_plugin()]
pub async fn test(
    remote_control_result: Result<fremotecontrol::RemoteControlProxy>,
    cmd: TestCommand,
) -> Result<()> {
    let writer = Box::new(stdout());
    let remote_control =
        remote_control_result.map_err(|e| ffx_error_with_code!(*SETUP_FAILED_CODE, "{:?}", e))?;
    match cmd.subcommand {
        TestSubCommand::Run(run) => run_test(remote_control, writer, run).await,
        TestSubCommand::List(list) => {
            let query_proxy = testing_lib::connect_to_query(&remote_control)
                .await
                .map_err(|e| ffx_error_with_code!(*SETUP_FAILED_CODE, "{:?}", e))?;
            get_tests(query_proxy, writer, list).await
        }
    }
}

struct Experiment {
    name: &'static str,
    enabled: bool,
}

struct Experiments {
    json_input: Experiment,
    parallel_execution: Experiment,
}

impl Experiments {
    async fn get_experiment(experiment_name: &'static str) -> Experiment {
        Experiment {
            name: experiment_name,
            enabled: match ffx_config::get(experiment_name).await {
                Ok(enabled) => enabled,
                Err(_) => false,
            },
        }
    }

    async fn from_env() -> Self {
        Self {
            json_input: Self::get_experiment("test.experimental_json_input").await,
            parallel_execution: Self::get_experiment("test.enable_experimental_parallel_execution")
                .await,
        }
    }
}

async fn run_test<W: 'static + Write + Send + Sync>(
    remote_control: fremotecontrol::RemoteControlProxy,
    mut writer: W,
    cmd: RunCommand,
) -> Result<()> {
    let experiments = Experiments::from_env().await;

    let min_log_severity = cmd.min_severity_logs.clone();

    let output_directory = match (cmd.disable_output_directory, &cmd.output_directory) {
        (true, maybe_dir) => {
            writeln!(
                writer,
                "WARN: --disable-output-directory is now a no-op and will soon be \
                removed, please remove it from your invocation."
            )?;
            maybe_dir.clone().map(Into::into)
        }
        (false, Some(directory)) => Some(directory.clone().into()), // an override directory is specified.
        (false, None) => None,
    };
    let output_directory_options = output_directory
        .map(|root_path| run_test_suite_lib::DirectoryReporterOptions { root_path });
    let reporter =
        run_test_suite_lib::create_reporter(cmd.filter_ansi, output_directory_options, writer)?;

    let run_params = run_test_suite_lib::RunParams {
        timeout_behavior: match cmd.continue_on_timeout {
            false => run_test_suite_lib::TimeoutBehavior::TerminateRemaining,
            true => run_test_suite_lib::TimeoutBehavior::Continue,
        },
        timeout_grace_seconds: ffx_config::get::<u64, _>("test.timeout_grace_seconds").await?
            as u32,
        stop_after_failures: match cmd.stop_after_failures.map(std::num::NonZeroU32::new) {
            None => None,
            Some(None) => ffx_bail!("--stop-after-failures should be greater than zero."),
            Some(Some(stop_after)) => Some(stop_after),
        },
        experimental_parallel_execution: match (
            cmd.experimental_parallel_execution,
            experiments.parallel_execution.enabled,
        ) {
            (None, _) => None,
            (Some(max_parallel_suites), true) => Some(max_parallel_suites),
            (_, false) => ffx_bail!(
              "Parallel test suite execution is experimental and is subject to breaking changes. \
              To enable parallel test suite execution, run: \n \
              'ffx config set {} true'",
              experiments.parallel_execution.name
            ),
        },
        accumulate_debug_data: false, // ffx never accumulates.
        log_protocol: None,
        min_severity_logs: min_log_severity,
        show_full_moniker: cmd.show_full_moniker_in_logs,
    };

    let test_definitions =
        test_params_from_args(&remote_control, cmd, experiments.json_input.enabled).await?;

    let (cancel_sender, cancel_receiver) = futures::channel::oneshot::channel::<()>();
    let mut signals = Signals::new(&[SIGINT, SIGTERM]).unwrap();
    // signals.forever() is blocking, so we need to spawn a thread rather than use async.
    let _signal_handle_thread = std::thread::spawn(move || {
        for signal in signals.forever() {
            match signal {
                SIGINT | SIGTERM => {
                    let _ = cancel_sender.send(());
                    break;
                }
                _ => unreachable!(),
            }
        }
    });

    let start_time = std::time::Instant::now();
    let result = match run_test_suite_lib::run_tests_and_get_outcome(
        RunConnector::new(remote_control, SUITE_BATCH_SIZE),
        test_definitions,
        run_params,
        reporter,
        cancel_receiver.map(|_| ()),
    )
    .await
    {
        run_test_suite_lib::Outcome::Passed => Ok(()),
        run_test_suite_lib::Outcome::Timedout => {
            ffx_bail_with_code!(*TIMED_OUT_CODE, "Tests timed out.",)
        }
        run_test_suite_lib::Outcome::Failed | run_test_suite_lib::Outcome::DidNotFinish => {
            ffx_bail!("Tests failed.")
        }
        run_test_suite_lib::Outcome::Cancelled => ffx_bail!("Tests cancelled."),
        run_test_suite_lib::Outcome::Inconclusive => ffx_bail!("Inconclusive test result."),
        run_test_suite_lib::Outcome::Error { origin } => match &*origin {
            run_test_suite_lib::RunTestSuiteError::Connection(conn_err) => {
                ffx_bail_with_code!(*SETUP_FAILED_CODE, "{:?}", conn_err)
            }
            run_test_suite_lib::RunTestSuiteError::Launch(launch_err) => match launch_err {
                ftest_manager::LaunchError::ResourceUnavailable => {
                    ffx_bail!("There were insufficient resources to launch the test.")
                }
                ftest_manager::LaunchError::InstanceCannotResolve => {
                    ffx_bail!("Cannot resolve test URL.")
                }
                ftest_manager::LaunchError::InvalidArgs => {
                    ffx_bail!("One or more invalid arguments passed.")
                }
                ftest_manager::LaunchError::FailedToConnectToTestSuite => {
                    ffx_bail!("Failed to connect to test suite.")
                }
                ftest_manager::LaunchError::CaseEnumeration => {
                    ffx_bail!("Failed to enumerate tests.")
                }
                ftest_manager::LaunchError::InternalError => {
                    ffx_bail!("An internal error occurred. Please check logs and report bug.")
                }
                ftest_manager::LaunchError::NoMatchingCases => {
                    ffx_bail!("No test cases match specified test filters.")
                }
                ftest_manager::LaunchError::InvalidManifest => {
                    ffx_bail!("Test manifest is invalid.")
                }
                other => ffx_bail!("Launch error: {:?}.", other),
            },
            other if other.is_internal_error() => {
                Err(anyhow!("There was an internal error running tests: {:?}", other))
            }
            other => ffx_bail!("There was an error running tests: {:?}", other),
        },
    };
    tracing::info!("run test suite duration: {:?}", start_time.elapsed().as_secs_f32());
    result
}

/// Generate TestParams from |cmd|.
/// |stdin_handle_fn| is a function that generates a handle to stdin and is a parameter to enable
/// testing.
async fn test_params_from_args(
    remote_control: &fremotecontrol::RemoteControlProxy,
    cmd: RunCommand,
    json_input_experiment_enabled: bool,
) -> Result<impl ExactSizeIterator<Item = run_test_suite_lib::TestParams> + Debug, FfxError> {
    let lifecycle_controller = ffx_component::rcs::connect_to_lifecycle_controller(&remote_control)
        .await
        .map_err(|e| ffx_error!("Parsing realm: Cannot connect to lifecycle controller: {}", e))?;
    let realm_query = ffx_component::rcs::connect_to_realm_query(&remote_control)
        .await
        .map_err(|e| ffx_error!("Parsing realm: Cannot connect to realm query: {}", e))?;
    match &cmd.test_file {
        Some(_) if !json_input_experiment_enabled => {
            return Err(ffx_error!(
                "The --test-file option is experimental, and the input format is \
                subject to breaking changes. To enable using --test-file, run \
                'ffx config set test.experimental_json_input true'"
            ))
        }
        Some(filename) => {
            if !cmd.test_args.is_empty() {
                return Err(ffx_error!("Tests may not be specified in both args and by file"));
            } else {
                let file = std::fs::File::open(filename)
                    .map_err(|e| ffx_error!("Failed to open file {}: {:?}", filename, e))?;
                suite_definition::test_params_from_reader(
                    file,
                    &lifecycle_controller,
                    &realm_query,
                    TestParamsOptions { ignore_test_without_known_execution: false },
                )
                .await
                .map_err(|e| ffx_error!("Failed to read test definitions: {:?}", e))
            }
        }
        .map(|file_params| Either::Left(file_params.into_iter())),
        None => {
            let mut test_args_iter = cmd.test_args.iter();
            let (test_url, test_args) = match test_args_iter.next() {
                None => return Err(ffx_error!("No tests specified!")),
                Some(test_url) => {
                    (test_url.clone(), test_args_iter.map(String::clone).collect::<Vec<_>>())
                }
            };

            let mut provided_realm = None;
            if let Some(realm_str) = &cmd.realm {
                provided_realm = Some(
                    run_test_suite_lib::parse_provided_realm(
                        &lifecycle_controller,
                        &realm_query,
                        &realm_str,
                    )
                    .await
                    .map_err(|e| ffx_error!("Error parsing realm '{}': {}", realm_str, e))?,
                );
            }

            let test_params = run_test_suite_lib::TestParams {
                test_url,
                realm: provided_realm.into(),
                timeout_seconds: cmd.timeout.and_then(std::num::NonZeroU32::new),
                test_filters: if cmd.test_filter.len() == 0 { None } else { Some(cmd.test_filter) },
                max_severity_logs: cmd.max_severity_logs,
                min_severity_logs: cmd.min_severity_logs,
                also_run_disabled_tests: cmd.run_disabled,
                parallel: cmd.parallel,
                test_args,
                tags: vec![],
            };

            let count = cmd.count.unwrap_or(1);
            let count = std::num::NonZeroU32::new(count)
                .ok_or_else(|| ffx_error!("--count should be greater than zero."))?;
            let repeated = (0..count.get()).map(move |_: u32| test_params.clone());
            Ok(repeated)
        }
        .map(Either::Right),
    }
}

async fn get_tests<W: Write>(
    query_proxy: ftest_manager::QueryProxy,
    mut write: W,
    cmd: ListCommand,
) -> Result<()> {
    let writer = &mut write;
    let (iterator_proxy, iterator) = create_proxy().unwrap();

    tracing::info!("launching test suite {}", cmd.test_url);

    query_proxy
        .enumerate(&cmd.test_url, iterator)
        .await
        .context("enumeration failed")?
        .map_err(|e| format_err!("error launching test: {:?}", e))?;

    writeln!(writer, "Tests in suite {}:\n", cmd.test_url)?;
    loop {
        let cases = iterator_proxy.get_next().await?;
        if cases.is_empty() {
            return Ok(());
        }
        for case in cases {
            match case.name {
                Some(n) => writeln!(writer, "{}", n)?,
                None => writeln!(writer, "<No name>")?,
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use futures::prelude::*;
    use lazy_static::lazy_static;
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use test_list::TestTag;

    const VALID_INPUT_FILENAME: &str = "valid_defs.json";
    const INVALID_INPUT_FILENAME: &str = "invalid_defs.json";

    lazy_static! {
        static ref VALID_INPUT_FORMAT: String = serde_json::to_string(&serde_json::json!({
          "schema_id": "experimental",
          "data": [
            {
                "name": "{}-test-1",
                "labels": ["{}-label"],
                "execution": {
                    "type": "fuchsia_component",
                    "component_url": "{}-test-url-1",
                },
                "tags": [],
            },
            {
                "name": "{}-test-2",
                "labels": ["{}-label"],
                "execution": {
                    "type": "fuchsia_component",
                    "component_url": "{}-test-url-2",
                    "timeout_seconds": 60,
                },
                "tags": [],
            },
            {
                "name": "{}-test-3",
                "labels": ["{}-label"],
                "execution": {
                    "type": "fuchsia_component",
                    "component_url": "{}-test-url-3",
                    "test_args": ["--flag"],
                    "test_filters": ["Unit"],
                    "also_run_disabled_tests": true,
                    "parallel": 4,
                    "max_severity_logs": "INFO",
                },
                "tags": [{
                    "key": "hermetic",
                    "value": "true",
                }],
            }
        ]}))
        .expect("serialize json");
        static ref VALID_FILE_INPUT: Vec<u8> =
            VALID_INPUT_FORMAT.replace("{}", "file").into_bytes();
        static ref INVALID_INPUT: Vec<u8> = vec![1u8; 64];
    }

    struct FakeRemoteControllerProvider {
        controller: Arc<fremotecontrol::RemoteControlProxy>,
        _task: fuchsia_async::Task<()>,
    }

    impl FakeRemoteControllerProvider {
        fn new() -> FakeRemoteControllerProvider {
            let (remote_control, mut stream) =
                create_proxy_and_stream::<fremotecontrol::RemoteControlMarker>().unwrap();
            let _task = fuchsia_async::Task::spawn(async move {
                while let Some(request) = stream.try_next().await.unwrap() {
                    // store channels so that they do not die.
                    let mut server_channels = vec![];
                    match request {
                        fremotecontrol::RemoteControlRequest::RootLifecycleController {
                            server,
                            responder,
                        } => {
                            server_channels.push(server.into_channel());
                            responder.send(Ok(())).expect("error sending EchoString response");
                        }

                        fremotecontrol::RemoteControlRequest::RootRealmQuery {
                            server,
                            responder,
                        } => {
                            server_channels.push(server.into_channel());
                            responder.send(Ok(())).expect("error sending EchoString response");
                        }
                        _ => {
                            panic!("Not implemented: {:?}", request);
                        }
                    }
                }
            });
            FakeRemoteControllerProvider { controller: remote_control.into(), _task }
        }

        fn remote_controller(&self) -> &fremotecontrol::RemoteControlProxy {
            self.controller.as_ref()
        }
    }

    #[fuchsia::test]
    async fn test_get_test_params() {
        let dir = tempfile::tempdir().expect("Create temp dir");
        std::fs::write(dir.path().join("test_defs.json"), &*VALID_FILE_INPUT).expect("write file");

        let cases = vec![
            (
                RunCommand {
                    timeout: None,
                    test_args: vec!["my-test-url".to_string()],
                    test_file: None,
                    test_filter: vec![],
                    realm: None,
                    run_disabled: false,
                    filter_ansi: false,
                    parallel: None,
                    count: None,
                    min_severity_logs: vec![],
                    show_full_moniker_in_logs: false,
                    max_severity_logs: None,
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                },
                vec![run_test_suite_lib::TestParams {
                    test_url: "my-test-url".to_string(),
                    realm: None.into(),
                    timeout_seconds: None,
                    test_filters: None,
                    also_run_disabled_tests: false,
                    parallel: None,
                    test_args: vec![],
                    max_severity_logs: None,
                    min_severity_logs: vec![],
                    tags: vec![],
                }],
            ),
            (
                RunCommand {
                    timeout: None,
                    test_args: vec!["my-test-url".to_string()],
                    test_file: None,
                    test_filter: vec![],
                    realm: None,
                    run_disabled: false,
                    filter_ansi: false,
                    parallel: None,
                    count: Some(10),
                    min_severity_logs: vec![],
                    show_full_moniker_in_logs: false,
                    max_severity_logs: Some(diagnostics_data::Severity::Warn),
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                },
                vec![
                    run_test_suite_lib::TestParams {
                        test_url: "my-test-url".to_string(),
                        realm: None.into(),
                        timeout_seconds: None,
                        test_filters: None,
                        also_run_disabled_tests: false,
                        max_severity_logs: Some(diagnostics_data::Severity::Warn),
                        min_severity_logs: vec![],
                        parallel: None,
                        test_args: vec![],
                        tags: vec![],
                    };
                    10
                ],
            ),
            (
                RunCommand {
                    timeout: Some(10),
                    test_args: vec!["my-test-url".to_string(), "--".to_string(), "arg".to_string()],
                    test_file: None,
                    test_filter: vec!["filter".to_string()],
                    realm: None,
                    run_disabled: true,
                    filter_ansi: false,
                    parallel: Some(20),
                    count: None,
                    show_full_moniker_in_logs: false,
                    min_severity_logs: vec![],
                    max_severity_logs: None,
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                },
                vec![run_test_suite_lib::TestParams {
                    test_url: "my-test-url".to_string(),
                    realm: None.into(),
                    timeout_seconds: Some(NonZeroU32::new(10).unwrap()),
                    test_filters: Some(vec!["filter".to_string()]),
                    also_run_disabled_tests: true,
                    max_severity_logs: None,
                    min_severity_logs: vec![],
                    parallel: Some(20),
                    test_args: vec!["--".to_string(), "arg".to_string()],
                    tags: vec![],
                }],
            ),
            (
                RunCommand {
                    timeout: None,
                    test_args: vec![],
                    test_file: Some(
                        dir.path().join("test_defs.json").to_str().unwrap().to_string(),
                    ),
                    test_filter: vec![],
                    realm: None,
                    run_disabled: false,
                    filter_ansi: false,
                    parallel: None,
                    count: None,
                    min_severity_logs: vec![],
                    show_full_moniker_in_logs: false,
                    max_severity_logs: None,
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                },
                vec![
                    run_test_suite_lib::TestParams {
                        test_url: "file-test-url-1".to_string(),
                        realm: None.into(),
                        timeout_seconds: None,
                        test_filters: None,
                        also_run_disabled_tests: false,
                        max_severity_logs: None,
                        min_severity_logs: vec![],
                        parallel: None,
                        test_args: vec![],
                        tags: vec![],
                    },
                    run_test_suite_lib::TestParams {
                        test_url: "file-test-url-2".to_string(),
                        realm: None.into(),
                        timeout_seconds: Some(NonZeroU32::new(60).unwrap()),
                        test_filters: None,
                        also_run_disabled_tests: false,
                        max_severity_logs: None,
                        min_severity_logs: vec![],
                        parallel: None,
                        test_args: vec![],
                        tags: vec![],
                    },
                    run_test_suite_lib::TestParams {
                        test_url: "file-test-url-3".to_string(),
                        realm: None.into(),
                        timeout_seconds: None,
                        test_filters: Some(vec!["Unit".to_string()]),
                        also_run_disabled_tests: true,
                        max_severity_logs: Some(diagnostics_data::Severity::Info),
                        min_severity_logs: vec![],
                        parallel: Some(4),
                        test_args: vec!["--flag".to_string()],
                        tags: vec![TestTag {
                            key: "hermetic".to_string(),
                            value: "true".to_string(),
                        }],
                    },
                ],
            ),
        ];
        let fake_contoller = FakeRemoteControllerProvider::new();
        for (run_command, expected_test_params) in cases.into_iter() {
            let result = test_params_from_args(
                fake_contoller.remote_controller(),
                run_command.clone(),
                true,
            )
            .await;
            assert!(
                result.is_ok(),
                "Error getting test params from {:?}: {:?}",
                run_command,
                result.unwrap_err()
            );
            assert_eq!(result.unwrap().into_iter().collect::<Vec<_>>(), expected_test_params);
        }
    }

    #[fuchsia::test]
    async fn test_get_test_params_count() {
        // Regression test for https://fxbug.dev/111145: using an extremely
        // large test count should result in a modest memory allocation. If
        // that wasn't the case, this test would fail.
        const COUNT: u32 = u32::MAX;
        let fake_contoller = FakeRemoteControllerProvider::new();
        let params = test_params_from_args(
            fake_contoller.remote_controller(),
            RunCommand {
                test_args: vec!["my-test-url".to_string()],
                count: Some(COUNT),
                timeout: None,
                test_file: None,
                test_filter: vec![],
                realm: None,
                run_disabled: false,
                filter_ansi: false,
                parallel: None,
                min_severity_logs: vec![],
                show_full_moniker_in_logs: false,
                max_severity_logs: Some(diagnostics_data::Severity::Warn),
                output_directory: None,
                disable_output_directory: false,
                continue_on_timeout: false,
                stop_after_failures: None,
                experimental_parallel_execution: None,
            },
            true,
        )
        .await
        .expect("should succeed");
        assert_eq!(params.len(), usize::try_from(COUNT).unwrap());
    }

    #[fuchsia::test]
    async fn test_get_test_params_invalid_args() {
        let dir = tempfile::tempdir().expect("Create temp dir");
        std::fs::write(dir.path().join(VALID_INPUT_FILENAME), &*VALID_FILE_INPUT)
            .expect("write file");
        std::fs::write(dir.path().join(INVALID_INPUT_FILENAME), &*INVALID_INPUT)
            .expect("write file");
        let cases = vec![
            (
                "no tests specified",
                RunCommand {
                    timeout: None,
                    test_args: vec![],
                    test_file: None,
                    test_filter: vec![],
                    realm: None,
                    run_disabled: false,
                    filter_ansi: false,
                    parallel: None,
                    count: None,
                    min_severity_logs: vec![],
                    show_full_moniker_in_logs: false,
                    max_severity_logs: None,
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                },
            ),
            (
                "tests specified in both args and file",
                RunCommand {
                    timeout: None,
                    test_args: vec!["my-test".to_string()],
                    test_file: Some(
                        dir.path().join(VALID_INPUT_FILENAME).to_str().unwrap().to_string(),
                    ),
                    test_filter: vec![],
                    realm: None,
                    run_disabled: false,
                    filter_ansi: false,
                    parallel: None,
                    count: None,
                    min_severity_logs: vec![],
                    show_full_moniker_in_logs: false,
                    max_severity_logs: None,
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                },
            ),
            (
                "read invalid input from file",
                RunCommand {
                    timeout: None,
                    test_args: vec![],
                    test_file: Some(
                        dir.path().join(INVALID_INPUT_FILENAME).to_str().unwrap().to_string(),
                    ),
                    test_filter: vec![],
                    realm: None,
                    run_disabled: false,
                    filter_ansi: false,
                    parallel: None,
                    count: None,
                    min_severity_logs: vec![],
                    show_full_moniker_in_logs: false,
                    max_severity_logs: None,
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                },
            ),
        ];
        let fake_contoller = FakeRemoteControllerProvider::new();
        for (case_name, invalid_run_command) in cases.into_iter() {
            let result = test_params_from_args(
                fake_contoller.remote_controller(),
                invalid_run_command,
                true,
            )
            .await;
            assert!(
                result.is_err(),
                "Getting test params for case '{}' unexpectedly succeeded",
                case_name
            );
        }
    }
}
