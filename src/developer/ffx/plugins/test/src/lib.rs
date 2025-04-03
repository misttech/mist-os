// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod connector;
mod suite_definition;

use crate::connector::RunConnector;
use crate::suite_definition::TestParamsOptions;
use anyhow::{format_err, Context, Result};
use async_trait::async_trait;
use either::Either;
use errors::{ffx_bail, ffx_bail_with_code, ffx_error, ffx_error_with_code, FfxError};
use ffx_test_args::{
    EarlyBootProfileCommand, ListCommand, RunCommand, TestCommand, TestSubCommand,
};
use ffx_writer::{ToolIO, VerifiedMachineWriter};
use fho::{return_user_error, FfxContext, FfxMain, FfxTool};
use fidl::endpoints::create_proxy;
use futures::FutureExt;
use itertools::Itertools;
use run_test_suite_lib::output::Reporter;
use schemars::JsonSchema;
use serde::Serialize;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::fmt::Debug;
use std::io::{stdout, Write};
use std::ops::Deref as _;
use std::sync::{Arc, LazyLock, Mutex};
use target_holders::RemoteControlProxyHolder;
use {
    fidl_fuchsia_developer_remotecontrol as fremotecontrol,
    fidl_fuchsia_test_manager as ftest_manager,
};

/// Error code returned if connecting to Test Manager fails.
pub static SETUP_FAILED_CODE: LazyLock<i32> =
    LazyLock::new(|| -fidl::Status::UNAVAILABLE.into_raw());
/// Error code returned if tests time out.
pub static TIMED_OUT_CODE: LazyLock<i32> = LazyLock::new(|| -fidl::Status::TIMED_OUT.into_raw());

/// Max number of test suites to run using a single RunBuilder connection.
/// Since we need to make n SuiteController channels when running tests on a
/// single RunBuilder channel, this max limits the number of channels that need
/// to be opened before any tests are run, allowing tests to start running faster.
/// It also limits the maximum resources that overnet needs to handle at once.
/// This isn't set to 1 as (1 RunBuilder connection) = (1 set of debug data). Since
/// pulling debug data off device is expensive we also want to limit the number of
/// times this occurs.
const SUITE_BATCH_SIZE: usize = 100;

#[derive(FfxTool)]
pub struct TestTool {
    #[command]
    cmd: TestCommand,
    rcs: fho::Deferred<RemoteControlProxyHolder>,
}

fho::embedded_plugin!(TestTool);

/// A simple Sync + Send buffer for use with machine output when we want to collect
/// the entirety of the string output.
///
/// TODO(403376806): This should not be the full solution for printing machine output. This misses
/// a lot of information that could be used for presentation purposes in machine-readable contexts.
#[derive(Clone, Default)]
struct SyncBuffer {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl SyncBuffer {
    fn contents(&self) -> String {
        let inner = self.inner.lock().expect("sync buffer lock");
        str::from_utf8(&*inner).expect("compatible utf8 string").to_owned()
    }
}

impl Write for SyncBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut inner = self.inner.lock().expect("sync buffer lock");
        inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut inner = self.inner.lock().expect("sync buffer lock");
        inner.flush()
    }
}

#[async_trait(?Send)]
impl FfxMain for TestTool {
    type Writer = VerifiedMachineWriter<TestToolMessage>;

    // TODO(https://fxbug.dev/42078544): use Writer when it becomes possible.
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let remote_control =
            self.rcs.await.map_err(|e| ffx_error_with_code!(*SETUP_FAILED_CODE, "{:?}", e))?;
        match self.cmd.subcommand {
            TestSubCommand::Run(run) => {
                let output_string = SyncBuffer::default();
                let test_run_writer: Box<dyn Write + Sync + Send + 'static> = if writer.is_machine()
                {
                    Box::new(output_string.clone())
                } else {
                    Box::new(stdout())
                };
                run_test(remote_control.deref().clone(), test_run_writer, run).await?;
                if writer.is_machine() {
                    writer.machine(&TestToolMessage::TestResults(output_string.contents()))?;
                }
                Ok(())
            }
            TestSubCommand::List(list) => get_tests(&remote_control, writer, list).await,
            TestSubCommand::EarlyBootProfile(cmd) => {
                early_boot_profile(remote_control.deref().clone(), writer, cmd).await
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TestName {
    Test { name: String },
    NoNameGiven,
}

impl std::fmt::Display for TestName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Test { name } => name,
            Self::NoNameGiven => &"<No name>".to_string(),
        };
        write!(f, "{}", message)
    }
}

#[derive(Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TestToolMessage {
    CannotCreateDirectory { err: String },
    ListTests { tests: Vec<TestName> },
    TestResults(String),
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
            enabled: match ffx_config::get(experiment_name) {
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

async fn early_boot_profile(
    remote_control: fremotecontrol::RemoteControlProxy,
    mut writer: VerifiedMachineWriter<TestToolMessage>,
    cmd: EarlyBootProfileCommand,
) -> fho::Result<()> {
    let early_boot_profile_proxy = testing_lib::connect_to_early_boot_profile(&remote_control)
        .await
        .map_err(|e| ffx_error_with_code!(*SETUP_FAILED_CODE, "{:?}", e))?;

    let (client, iterator) = fidl::endpoints::create_endpoints();
    early_boot_profile_proxy
        .register_watcher(iterator)
        .map_err(|e| ffx_error_with_code!(*SETUP_FAILED_CODE, "{:?}", e))?;

    let reporter = run_test_suite_lib::output::DirectoryReporter::new(
        cmd.output_directory,
        run_test_suite_lib::output::SchemaVersion::V1,
    )
    .bug()?;

    match reporter.new_directory_artifact(
        &run_test_suite_lib::output::EntityId::TestRun,
        &run_test_suite_lib::output::DirectoryArtifactType::Debug,
        None,
    ) {
        Ok(o) => run_test_suite_lib::copy_debug_data(client.into_proxy(), o).await,
        Err(e) => {
            writer.machine_or(
                &TestToolMessage::CannotCreateDirectory { err: e.to_string() },
                format!("Cannot create output directory: {}", e),
            )?;
            eprintln!("Cannot create output directory: {}", e);
            return Err(fho::Error::User(e.into()));
        }
    };
    // save summary
    reporter.entity_finished(&run_test_suite_lib::output::EntityId::TestRun).bug()?;

    Ok(())
}

async fn run_test<W: 'static + Write + Send + Sync>(
    remote_control: fremotecontrol::RemoteControlProxy,
    writer: W,
    cmd: RunCommand,
) -> fho::Result<()> {
    let experiments = Experiments::from_env().await;

    let min_log_severity = cmd.min_severity_logs.clone();
    let hermetic_test = cmd.realm.is_none();

    let output_directory = match (cmd.disable_output_directory, &cmd.output_directory) {
        (true, maybe_dir) => {
            eprintln!(
                "WARN: --disable-output-directory is now a no-op and will soon be \
                removed, please remove it from your invocation."
            );
            maybe_dir.clone().map(Into::into)
        }
        (false, Some(directory)) => Some(directory.clone().into()), // an override directory is specified.
        (false, None) => None,
    };
    let output_directory_options = output_directory
        .map(|root_path| run_test_suite_lib::DirectoryReporterOptions { root_path });
    let reporter =
        run_test_suite_lib::create_reporter(cmd.filter_ansi, output_directory_options, writer)?;

    let timeout_key = "test.timeout_grace_seconds";
    let run_params = run_test_suite_lib::RunParams {
        timeout_behavior: match cmd.continue_on_timeout {
            false => run_test_suite_lib::TimeoutBehavior::TerminateRemaining,
            true => run_test_suite_lib::TimeoutBehavior::Continue,
        },
        timeout_grace_seconds: ffx_config::get::<u64, _>(timeout_key)
            .user_message(format!("Could not load timeout from config at {timeout_key}"))?
            as u32,
        stop_after_failures: match cmd.stop_after_failures.map(std::num::NonZeroU32::new) {
            None => None,
            Some(None) => return_user_error!("--stop-after-failures should be greater than zero."),
            Some(Some(stop_after)) => Some(stop_after),
        },
        experimental_parallel_execution: match (
            cmd.experimental_parallel_execution,
            experiments.parallel_execution.enabled,
        ) {
            (None, _) => None,
            (Some(max_parallel_suites), true) => Some(max_parallel_suites),
            (_, false) => return_user_error!(
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
        if let Some(signal) = signals.forever().next() {
            match signal {
                SIGINT | SIGTERM => {
                    let _ = cancel_sender.send(());
                }
                _ => unreachable!(),
            }
        }
    });

    let start_time = std::time::Instant::now();
    let outcome = run_test_suite_lib::run_tests_and_get_outcome(
        RunConnector::new(remote_control, SUITE_BATCH_SIZE),
        test_definitions,
        run_params,
        reporter,
        cancel_receiver.map(|_| ()),
    )
    .await;
    let show_realm_warning = outcome == run_test_suite_lib::Outcome::Timedout
        || outcome == run_test_suite_lib::Outcome::Failed
        || outcome == run_test_suite_lib::Outcome::DidNotFinish;
    tracing::info!("ffx test duration: {:?}", start_time.elapsed().as_secs_f32());
    if hermetic_test && show_realm_warning {
        eprintln!(
            "The test was executed in the hermetic realm. If your test depends on system \
capabilities, pass in correct realm. See https://fuchsia.dev/go/components/non-hermetic-tests"
        );
    }
    match outcome {
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
                    return_user_error!("Cannot resolve test URL.")
                }
                ftest_manager::LaunchError::InvalidArgs => {
                    return_user_error!("One or more invalid arguments passed.")
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
                    return_user_error!("No test cases match specified test filters.")
                }
                ftest_manager::LaunchError::InvalidManifest => {
                    return_user_error!("Test manifest is invalid.")
                }
                other => ffx_bail!("Launch error: {:?}.", other),
            },
            other if other.is_internal_error() => {
                ffx_bail!("There was an internal error running tests: {:?}", other)
            }
            other => ffx_bail!("There was an error running tests: {:?}", other),
        },
    }
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
                break_on_failure: cmd.break_on_failure,
                no_exception_channel: cmd.no_exception_channel,
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

async fn get_tests(
    remote_control: &fremotecontrol::RemoteControlProxy,
    mut writer: VerifiedMachineWriter<TestToolMessage>,
    cmd: ListCommand,
) -> fho::Result<()> {
    let query_proxy = testing_lib::connect_to_query(&remote_control)
        .await
        .map_err(|e| ffx_error_with_code!(*SETUP_FAILED_CODE, "{:?}", e))?;
    let (iterator_proxy, iterator) = create_proxy();

    tracing::info!("launching test suite {}", cmd.test_url);

    let mut provided_realm = None;
    if let Some(realm_str) = &cmd.realm {
        let lifecycle_controller =
            ffx_component::rcs::connect_to_lifecycle_controller(&remote_control).await.map_err(
                |e| ffx_error!("Parsing realm: Cannot connect to lifecycle controller: {}", e),
            )?;

        let realm_query = ffx_component::rcs::connect_to_realm_query(&remote_control)
            .await
            .map_err(|e| ffx_error!("Parsing realm: Cannot connect to realm query: {}", e))?;

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

    let fut_response = match provided_realm {
        Some(realm) => {
            let offers = realm.offers();
            query_proxy.enumerate_in_realm(
                &cmd.test_url,
                realm
                    .get_realm_client()
                    .map_err(|e| ffx_error!("Cannot connect to realm client: {}", e))?,
                offers.as_slice(),
                realm.collection(),
                iterator,
            )
        }
        None => query_proxy.enumerate(&cmd.test_url, iterator),
    };

    fut_response
        .await
        .context("enumeration failed")?
        .map_err(|e| format_err!("error launching test: {:?}", e))?;

    let mut machine_cases = vec![];
    loop {
        let cases = iterator_proxy.get_next().await.bug()?;
        if cases.is_empty() {
            break;
        }
        let mut m_cases: Vec<_> = cases
            .iter()
            .map(|c| match &c.name {
                Some(n) => TestName::Test { name: n.to_string() },
                None => TestName::NoNameGiven,
            })
            .collect();
        machine_cases.append(&mut m_cases);
    }
    let cases_str = machine_cases.clone().into_iter().map(|c| format!("{}", c)).join("\n");
    writer.machine_or(&TestToolMessage::ListTests { tests: machine_cases }, cases_str).bug()?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_writer::{Format, TestBuffers};
    use fidl::endpoints::{create_proxy_and_stream, ProtocolMarker, RequestStream, ServerEnd};
    use fidl_fuchsia_sys2 as fsys;
    use ftest_manager::{
        DebugData, DebugDataIteratorMarker, EarlyBootProfileMarker, EarlyBootProfileRequestStream,
    };
    use futures::prelude::*;
    use std::io::Read;
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use test_list::TestTag;

    const VALID_INPUT_FILENAME: &str = "valid_defs.json";
    const INVALID_INPUT_FILENAME: &str = "invalid_defs.json";

    static VALID_INPUT_FORMAT: LazyLock<String> = LazyLock::new(|| {
        serde_json::to_string(&serde_json::json!({
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
        .expect("serialize json")
    });
    static VALID_FILE_INPUT: LazyLock<Vec<u8>> =
        LazyLock::new(|| VALID_INPUT_FORMAT.replace("{}", "file").into_bytes());
    static INVALID_INPUT: LazyLock<Vec<u8>> = LazyLock::new(|| vec![1u8; 64]);

    struct FakeRemoteControllerProvider {
        controller: Arc<fremotecontrol::RemoteControlProxy>,
        _task: fuchsia_async::Task<()>,
    }

    impl FakeRemoteControllerProvider {
        fn new() -> FakeRemoteControllerProvider {
            let (remote_control, mut stream) =
                create_proxy_and_stream::<fremotecontrol::RemoteControlMarker>();
            let _task = fuchsia_async::Task::spawn(async move {
                while let Some(request) = stream.try_next().await.unwrap() {
                    // store channels so that they do not die.
                    let mut server_channels = vec![];
                    match request {
                        fremotecontrol::RemoteControlRequest::ConnectCapability {
                            moniker,
                            capability_set,
                            capability_name,
                            server_channel,
                            responder,
                        } => {
                            assert_eq!(moniker, "toolbox");
                            assert_eq!(capability_set, fsys::OpenDirType::NamespaceDir);
                            assert!(
                                capability_name == "svc/fuchsia.sys2.RealmQuery.root"
                                    || capability_name
                                        == "svc/fuchsia.sys2.LifecycleController.root"
                            );
                            server_channels.push(server_channel);
                            responder.send(Ok(())).expect("error sending EchoString response");
                        }
                        other => {
                            unreachable!("Got unexpected request: {other:?}");
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                    count: None,
                    min_severity_logs: vec![],
                    show_full_moniker_in_logs: false,
                    max_severity_logs: None,
                    output_directory: None,
                    disable_output_directory: false,
                    continue_on_timeout: false,
                    stop_after_failures: None,
                    experimental_parallel_execution: None,
                    break_on_failure: false,
                    no_exception_channel: true,
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
                    break_on_failure: false,
                    no_exception_channel: true,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                        break_on_failure: false,
                        no_exception_channel: false,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                        break_on_failure: false,
                        no_exception_channel: false,
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
                        break_on_failure: false,
                        no_exception_channel: false,
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
                        break_on_failure: false,
                        no_exception_channel: false,
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
        // Regression test for https://fxbug.dev/42062444: using an extremely
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
                break_on_failure: false,
                no_exception_channel: false,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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
                    break_on_failure: false,
                    no_exception_channel: false,
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

    async fn fake_debug_data_iterator(iterator: ServerEnd<DebugDataIteratorMarker>) {
        let mut stream = iterator.into_stream();

        // we just need to send once sample file and not test full logic as that is tested inside the library.
        let (s1, s2) = fidl::Socket::create_stream();
        let mut debug_data = vec![DebugData {
            name: "test_file".to_string().into(),
            socket: s1.into(),
            ..Default::default()
        }];
        let mut compressor = zstd::bulk::Compressor::new(0).unwrap();
        let bytes = compressor.compress(&[1, 2, 3, 4, 5]).unwrap();
        s2.write(bytes.as_slice()).unwrap();
        while let Some(request) = stream.try_next().await.unwrap() {
            match request {
                ftest_manager::DebugDataIteratorRequest::GetNext { .. } => {
                    panic!("Not Implemented");
                }
                ftest_manager::DebugDataIteratorRequest::GetNextCompressed { responder } => {
                    responder.send(debug_data.drain(..).collect()).unwrap();
                }
            }
        }
    }

    #[fuchsia::test]
    async fn test_early_boot_profile() {
        let (remote_control, mut stream) =
            create_proxy_and_stream::<fremotecontrol::RemoteControlMarker>();
        let task = fuchsia_async::Task::spawn(async move {
            let mut once = false;
            while let Some(request) = stream.try_next().await.unwrap() {
                match request {
                    fremotecontrol::RemoteControlRequest::ConnectCapability {
                        moniker,
                        capability_set,
                        capability_name,
                        server_channel,
                        responder,
                    } => {
                        assert!(!once);
                        once = true;
                        assert_eq!(moniker, "/core/test_manager");
                        assert_eq!(capability_set, fsys::OpenDirType::ExposedDir);
                        assert!(capability_name == EarlyBootProfileMarker::DEBUG_NAME);
                        responder.send(Ok(())).expect("error sending EchoString response");

                        let mut stream = EarlyBootProfileRequestStream::from_channel(
                            fidl::AsyncChannel::from_channel(server_channel),
                        );
                        while let Some(request) = stream.try_next().await.unwrap() {
                            match request {
                                ftest_manager::EarlyBootProfileRequest::RegisterWatcher {
                                    iterator,
                                    control_handle: _,
                                } => {
                                    fake_debug_data_iterator(iterator).await;
                                }
                                other => {
                                    unreachable!("Got unexpected request: {other:?}");
                                }
                            }
                        }
                    }
                    other => {
                        unreachable!("Got unexpected request: {other:?}");
                    }
                }
            }
        });
        // Create a temporary directory
        let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
        let temp_dir_path = temp_dir.path();
        let test_buffers = TestBuffers::default();
        let writer = VerifiedMachineWriter::new_test(Some(Format::Json), &test_buffers);
        early_boot_profile(
            remote_control,
            writer,
            EarlyBootProfileCommand { output_directory: temp_dir_path.to_path_buf() },
        )
        .await
        .unwrap();
        task.await;

        assert!(test_buffers.into_stdout_str().is_empty());

        let mut found_test_file = false;
        for entry in walkdir::WalkDir::new(temp_dir_path).follow_links(false) {
            let entry = entry.unwrap();

            // Optionally, you can print other information about the entry
            if entry.file_type().is_file() {
                if entry.file_name() == "test_file" {
                    found_test_file = true;
                    let mut file_content = Vec::new();
                    let mut file = std::fs::File::open(entry.path()).expect("Failed to open file");
                    file.read_to_end(&mut file_content).expect("Failed to read file");
                    assert_eq!(file_content, [1, 2, 3, 4, 5]);
                    break;
                }
            }
        }
        assert!(found_test_file);
    }
}
