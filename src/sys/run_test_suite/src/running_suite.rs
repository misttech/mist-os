// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        artifacts,
        cancel::{Cancelled, NamedFutureExt, OrCancel},
        diagnostics,
        outcome::{Lifecycle, Outcome, RunTestSuiteError, UnexpectedEventError},
        output::{self, ArtifactType, CaseId, SuiteReporter, Timestamp},
        stream_util::StreamUtil,
        trace::duration,
    },
    diagnostics_data::Severity,
    fidl_fuchsia_test_manager::{
        self as ftest_manager, CaseArtifact, CaseFinished, CaseFound, CaseStarted, CaseStopped,
        SuiteArtifact, SuiteStopped,
    },
    fuchsia_async as fasync,
    futures::future::Either,
    futures::{prelude::*, stream::FuturesUnordered, StreamExt},
    std::collections::HashMap,
    std::io::Write,
    std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    tracing::{error, info, warn},
};

/// Struct used by |run_suite_and_collect_logs| to track the state of test cases and suites.
struct CollectedEntityState<R> {
    reporter: R,
    name: String,
    lifecycle: Lifecycle,
    artifact_tasks:
        Vec<fasync::Task<Result<Option<diagnostics::LogCollectionOutcome>, anyhow::Error>>>,
}

/// Collects results and artifacts for a single suite.
// TODO(satsukiu): There's two ways to return an error here:
// * Err(RunTestSuiteError)
// * Ok(Outcome::Error(RunTestSuiteError))
// We should consider how to consolidate these.
pub(crate) async fn run_suite_and_collect_logs<F: Future<Output = ()> + Unpin>(
    running_suite: RunningSuite,
    suite_reporter: &SuiteReporter<'_>,
    log_display: diagnostics::LogDisplayConfiguration,
    cancel_fut: F,
) -> Result<Outcome, RunTestSuiteError> {
    duration!("collect_suite");

    let RunningSuite {
        mut event_stream, stopper, timeout, timeout_grace, max_severity_logs, ..
    } = running_suite;

    let log_opts =
        diagnostics::LogCollectionOptions { format: log_display, max_severity: max_severity_logs };

    let mut test_cases: HashMap<u32, CollectedEntityState<_>> = HashMap::new();
    let mut suite_state = CollectedEntityState {
        reporter: suite_reporter,
        name: "".to_string(),
        lifecycle: Lifecycle::Found,
        artifact_tasks: vec![],
    };
    let mut suite_finish_timestamp = Timestamp::Unknown;
    let mut outcome = Outcome::Passed;

    let collect_results_fut = async {
        while let Some(event_result) = event_stream.next().named("next_event").await {
            match event_result {
                Err(e) => {
                    suite_state
                        .reporter
                        .stopped(&output::ReportedOutcome::Error, Timestamp::Unknown)?;
                    return Err(e);
                }
                Ok(event) => {
                    let timestamp = Timestamp::from_nanos(event.timestamp);
                    match event.payload.expect("event cannot be None") {
                        ftest_manager::SuiteEventPayload::CaseFound(CaseFound {
                            test_case_name,
                            identifier,
                        }) => {
                            if test_cases.contains_key(&identifier) {
                                return Err(UnexpectedEventError::InvalidCaseEvent {
                                    last_state: Lifecycle::Found,
                                    next_state: Lifecycle::Found,
                                    test_case_name,
                                    identifier,
                                }
                                .into());
                            }
                            test_cases.insert(
                                identifier,
                                CollectedEntityState {
                                    reporter: suite_reporter
                                        .new_case(&test_case_name, &CaseId(identifier))?,
                                    name: test_case_name,
                                    lifecycle: Lifecycle::Found,
                                    artifact_tasks: vec![],
                                },
                            );
                        }
                        ftest_manager::SuiteEventPayload::CaseStarted(CaseStarted {
                            identifier,
                        }) => {
                            let entry = test_cases.get_mut(&identifier).ok_or(
                                UnexpectedEventError::CaseEventButNotFound {
                                    next_state: Lifecycle::Started,
                                    identifier,
                                },
                            )?;
                            match &entry.lifecycle {
                                Lifecycle::Found => {
                                    // TODO(fxbug.dev/79712): Record per-case runtime once we have an
                                    // accurate way to measure it.
                                    entry.reporter.started(Timestamp::Unknown)?;
                                    entry.lifecycle = Lifecycle::Started;
                                }
                                other => {
                                    return Err(UnexpectedEventError::InvalidCaseEvent {
                                        last_state: *other,
                                        next_state: Lifecycle::Started,
                                        test_case_name: entry.name.clone(),
                                        identifier,
                                    }
                                    .into());
                                }
                            }
                        }
                        ftest_manager::SuiteEventPayload::CaseArtifact(CaseArtifact {
                            identifier,
                            artifact,
                        }) => {
                            let entry = test_cases.get_mut(&identifier).ok_or(
                                UnexpectedEventError::CaseArtifactButNotFound { identifier },
                            )?;
                            if matches!(entry.lifecycle, Lifecycle::Finished) {
                                return Err(UnexpectedEventError::CaseArtifactButFinished {
                                    identifier,
                                }
                                .into());
                            }
                            let artifact_fut = artifacts::drain_artifact(
                                &entry.reporter,
                                artifact,
                                log_opts.clone(),
                            )
                            .await?;
                            entry.artifact_tasks.push(fasync::Task::spawn(artifact_fut));
                        }
                        ftest_manager::SuiteEventPayload::CaseStopped(CaseStopped {
                            identifier,
                            status,
                        }) => {
                            let entry = test_cases.get_mut(&identifier).ok_or(
                                UnexpectedEventError::CaseEventButNotFound {
                                    next_state: Lifecycle::Stopped,
                                    identifier,
                                },
                            )?;
                            match &entry.lifecycle {
                                Lifecycle::Started => {
                                    // TODO(fxbug.dev/79712): Record per-case runtime once we have an
                                    // accurate way to measure it.
                                    entry.reporter.stopped(&status.into(), Timestamp::Unknown)?;
                                    entry.lifecycle = Lifecycle::Stopped;
                                }
                                other => {
                                    return Err(UnexpectedEventError::InvalidCaseEvent {
                                        last_state: *other,
                                        next_state: Lifecycle::Stopped,
                                        test_case_name: entry.name.clone(),
                                        identifier,
                                    }
                                    .into());
                                }
                            }
                        }
                        ftest_manager::SuiteEventPayload::CaseFinished(CaseFinished {
                            identifier,
                        }) => {
                            let entry = test_cases.get_mut(&identifier).ok_or(
                                UnexpectedEventError::CaseEventButNotFound {
                                    next_state: Lifecycle::Finished,
                                    identifier,
                                },
                            )?;
                            match &entry.lifecycle {
                                Lifecycle::Stopped => {
                                    // don't mark reporter finished yet, we want to finish draining
                                    // artifacts separately.
                                    entry.lifecycle = Lifecycle::Finished;
                                }
                                other => {
                                    return Err(UnexpectedEventError::InvalidCaseEvent {
                                        last_state: *other,
                                        next_state: Lifecycle::Finished,
                                        test_case_name: entry.name.clone(),
                                        identifier,
                                    }
                                    .into());
                                }
                            }
                        }
                        ftest_manager::SuiteEventPayload::SuiteArtifact(SuiteArtifact {
                            artifact,
                        }) => {
                            let artifact_fut = artifacts::drain_artifact(
                                suite_reporter,
                                artifact,
                                log_opts.clone(),
                            )
                            .await?;
                            suite_state.artifact_tasks.push(fasync::Task::spawn(artifact_fut));
                        }
                        ftest_manager::SuiteEventPayload::SuiteStarted(_) => {
                            match &suite_state.lifecycle {
                                Lifecycle::Found => {
                                    suite_state.reporter.started(timestamp)?;
                                    suite_state.lifecycle = Lifecycle::Started;
                                }
                                other => {
                                    return Err(UnexpectedEventError::InvalidSuiteEvent {
                                        last_state: *other,
                                        next_state: Lifecycle::Started,
                                    }
                                    .into());
                                }
                            }
                        }
                        ftest_manager::SuiteEventPayload::SuiteStopped(SuiteStopped { status }) => {
                            match &suite_state.lifecycle {
                                Lifecycle::Started => {
                                    suite_state.lifecycle = Lifecycle::Stopped;
                                    suite_finish_timestamp = timestamp;
                                    outcome = match status {
                                        ftest_manager::SuiteStatus::Passed => Outcome::Passed,
                                        ftest_manager::SuiteStatus::Failed => Outcome::Failed,
                                        ftest_manager::SuiteStatus::DidNotFinish => {
                                            Outcome::Inconclusive
                                        }
                                        ftest_manager::SuiteStatus::TimedOut
                                        | ftest_manager::SuiteStatus::Stopped => Outcome::Timedout,
                                        ftest_manager::SuiteStatus::InternalError => {
                                            Outcome::error(
                                                UnexpectedEventError::InternalErrorSuiteStatus,
                                            )
                                        }
                                        s => {
                                            return Err(
                                                UnexpectedEventError::UnrecognizedSuiteStatus {
                                                    status: s,
                                                }
                                                .into(),
                                            );
                                        }
                                    };
                                }
                                other => {
                                    return Err(UnexpectedEventError::InvalidSuiteEvent {
                                        last_state: *other,
                                        next_state: Lifecycle::Stopped,
                                    }
                                    .into());
                                }
                            }
                        }
                        ftest_manager::SuiteEventPayloadUnknown!() => {
                            warn!("Encountered unrecognized suite event");
                        }
                    }
                }
            }
        }
        drop(event_stream); // Explicit drop here to force ownership move.
        Ok(())
    }
    .boxed_local();

    let start_time = std::time::Instant::now();
    let (stop_timeout_future, kill_timeout_future) = match timeout {
        None => {
            (futures::future::pending::<()>().boxed(), futures::future::pending::<()>().boxed())
        }
        Some(duration) => (
            fasync::Timer::new(start_time + duration).boxed(),
            fasync::Timer::new(start_time + duration + timeout_grace).boxed(),
        ),
    };

    // This polls event collection and calling SuiteController::Stop on timeout simultaneously.
    let collect_or_stop_fut = async move {
        match futures::future::select(stop_timeout_future, collect_results_fut).await {
            Either::Left((_stop_done, collect_fut)) => {
                stopper.stop();
                collect_fut.await
            }
            Either::Right((result, _)) => result,
        }
    };

    // If kill timeout or cancel occur, we want to stop polling events.
    // kill_fut resolves to the outcome to which results should be overwritten
    // if it resolves.
    let kill_fut = async move {
        match futures::future::select(cancel_fut, kill_timeout_future).await {
            Either::Left(_) => Outcome::Cancelled,
            Either::Right(_) => Outcome::Timedout,
        }
    }
    .shared();

    let early_termination_outcome =
        match collect_or_stop_fut.boxed_local().or_cancelled(kill_fut.clone()).await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => return Err(e),
            Err(Cancelled(outcome)) => Some(outcome),
        };

    // Finish collecting artifacts and report errors.
    info!("Awaiting case artifacts");
    let mut unfinished_test_case_names = vec![];
    for (_, test_case) in test_cases.into_iter() {
        let CollectedEntityState { reporter, name, lifecycle, artifact_tasks } = test_case;
        match (lifecycle, early_termination_outcome.clone()) {
            (Lifecycle::Started | Lifecycle::Found, Some(early)) => {
                reporter.stopped(&early.into(), Timestamp::Unknown)?;
            }
            (Lifecycle::Found, None) => {
                unfinished_test_case_names.push(name.clone());
                reporter.stopped(&Outcome::Inconclusive.into(), Timestamp::Unknown)?;
            }
            (Lifecycle::Started, None) => {
                unfinished_test_case_names.push(name.clone());
                reporter.stopped(&Outcome::DidNotFinish.into(), Timestamp::Unknown)?;
            }
            (Lifecycle::Stopped | Lifecycle::Finished, _) => (),
        }

        let finish_artifacts_fut = FuturesUnordered::from_iter(artifact_tasks)
            .map(|result| match result {
                Err(e) => {
                    error!("Failed to collect artifact for {}: {:?}", name, e);
                }
                Ok(Some(_log_result)) => warn!("Unexpectedly got log results for a test case"),
                Ok(None) => (),
            })
            .collect::<()>();
        if let Err(Cancelled(_)) = finish_artifacts_fut.or_cancelled(kill_fut.clone()).await {
            warn!("Stopped polling artifacts for {} due to timeout", name);
        }

        reporter.finished()?;
    }
    if !unfinished_test_case_names.is_empty() {
        outcome = Outcome::error(UnexpectedEventError::CasesDidNotFinish {
            cases: unfinished_test_case_names,
        });
    }

    match (suite_state.lifecycle, early_termination_outcome) {
        (Lifecycle::Found | Lifecycle::Started, Some(early)) => {
            if matches!(&outcome, Outcome::Passed | Outcome::Failed) {
                outcome = early;
            }
        }
        (Lifecycle::Found | Lifecycle::Started, None) => {
            outcome = Outcome::error(UnexpectedEventError::SuiteDidNotReportStop);
        }
        // If the suite successfully reported a result, don't alter it.
        (Lifecycle::Stopped, _) => (),
        // Finished doesn't happen since there's no SuiteFinished event.
        (Lifecycle::Finished, _) => unreachable!(),
    }

    let restricted_logs_present = AtomicBool::new(false);
    let finish_artifacts_fut = FuturesUnordered::from_iter(suite_state.artifact_tasks)
        .then(|result| async {
            match result {
                Err(e) => {
                    error!("Failed to collect artifact for suite: {:?}", e);
                }
                Ok(Some(log_result)) => match log_result {
                    diagnostics::LogCollectionOutcome::Error { restricted_logs } => {
                        restricted_logs_present.store(true, Ordering::Relaxed);
                        let mut log_artifact = match suite_reporter
                            .new_artifact(&ArtifactType::RestrictedLog)
                        {
                            Ok(artifact) => artifact,
                            Err(e) => {
                                warn!("Error creating artifact to report restricted logs: {:?}", e);
                                return;
                            }
                        };
                        for log in restricted_logs.iter() {
                            if let Err(e) = writeln!(log_artifact, "{}", log) {
                                warn!("Error recording restricted logs: {:?}", e);
                                return;
                            }
                        }
                    }
                    diagnostics::LogCollectionOutcome::Passed => (),
                },
                Ok(None) => (),
            }
        })
        .collect::<()>();
    if let Err(Cancelled(_)) = finish_artifacts_fut.or_cancelled(kill_fut).await {
        warn!("Stopped polling artifacts due to timeout");
    }
    if restricted_logs_present.into_inner() && matches!(outcome, Outcome::Passed) {
        outcome = Outcome::Failed;
    }

    suite_reporter.stopped(&outcome.clone().into(), suite_finish_timestamp)?;

    Ok(outcome)
}

type SuiteEventStream = std::pin::Pin<
    Box<dyn Stream<Item = Result<ftest_manager::SuiteEvent, RunTestSuiteError>> + Send>,
>;

/// A test suite that is known to have started execution. A suite is considered started once
/// any event is produced for the suite.
pub(crate) struct RunningSuite {
    event_stream: SuiteEventStream,
    stopper: RunningSuiteStopper,
    max_severity_logs: Option<Severity>,
    timeout: Option<std::time::Duration>,
    timeout_grace: std::time::Duration,
}

struct RunningSuiteStopper(Arc<ftest_manager::SuiteControllerProxy>);

impl RunningSuiteStopper {
    fn stop(self) {
        let _ = self.0.stop();
    }
}

impl RunningSuite {
    /// Number of concurrently active GetEvents requests. Chosen by testing powers of 2 when
    /// running a set of tests using ffx test against an emulator, and taking the value at
    /// which improvement stops.
    const DEFAULT_PIPELINED_REQUESTS: usize = 8;
    pub(crate) async fn wait_for_start(
        proxy: ftest_manager::SuiteControllerProxy,
        max_severity_logs: Option<Severity>,
        timeout: Option<std::time::Duration>,
        timeout_grace: std::time::Duration,
        max_pipelined: Option<usize>,
    ) -> Self {
        let proxy = Arc::new(proxy);
        let proxy_clone = proxy.clone();
        // Stream of fidl responses, with multiple concurrently active requests.
        let unprocessed_event_stream = futures::stream::repeat_with(move || {
            proxy.get_events().inspect(|events_result| match events_result {
                Ok(Ok(ref events)) => info!("Latest suite event: {:?}", events.last()),
                _ => (),
            })
        })
        .buffered(max_pipelined.unwrap_or(Self::DEFAULT_PIPELINED_REQUESTS));
        // Terminate the stream after we get an error or empty list of events.
        let terminated_event_stream =
            unprocessed_event_stream.take_until_stop_after(|result| match &result {
                Ok(Ok(events)) => events.is_empty(),
                Err(_) | Ok(Err(_)) => true,
            });
        // Flatten the stream of vecs into a stream of single events.
        let mut event_stream = terminated_event_stream
            .map(Self::convert_to_result_vec)
            .map(futures::stream::iter)
            .flatten()
            .peekable();
        // Wait for the first event to be ready, which signals the suite has started.
        std::pin::Pin::new(&mut event_stream).peek().await;

        Self {
            event_stream: event_stream.boxed(),
            stopper: RunningSuiteStopper(proxy_clone),
            timeout,
            timeout_grace,
            max_severity_logs,
        }
    }

    fn convert_to_result_vec(
        vec: Result<
            Result<Vec<ftest_manager::SuiteEvent>, ftest_manager::LaunchError>,
            fidl::Error,
        >,
    ) -> Vec<Result<ftest_manager::SuiteEvent, RunTestSuiteError>> {
        match vec {
            Ok(Ok(events)) => events.into_iter().map(Ok).collect(),
            Ok(Err(e)) => vec![Err(e.into())],
            Err(e) => vec![Err(e.into())],
        }
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::output::{EntityId, SuiteId},
        assert_matches::assert_matches,
        fidl::endpoints::create_proxy_and_stream,
        maplit::hashmap,
    };

    async fn respond_to_get_events(
        request_stream: &mut ftest_manager::SuiteControllerRequestStream,
        events: Vec<ftest_manager::SuiteEvent>,
    ) {
        let request = request_stream
            .next()
            .await
            .expect("did not get next request")
            .expect("error getting next request");
        let responder = match request {
            ftest_manager::SuiteControllerRequest::GetEvents { responder } => responder,
            r => panic!("Expected GetEvents request but got {:?}", r),
        };

        responder.send(Ok(events)).expect("send events");
    }

    /// Serves all events to completion.
    async fn serve_all_events(
        mut request_stream: ftest_manager::SuiteControllerRequestStream,
        events: Vec<ftest_manager::SuiteEvent>,
    ) {
        const BATCH_SIZE: usize = 5;
        let mut event_iter = events.into_iter();
        while event_iter.len() > 0 {
            respond_to_get_events(
                &mut request_stream,
                event_iter.by_ref().take(BATCH_SIZE).collect(),
            )
            .await;
        }
        respond_to_get_events(&mut request_stream, vec![]).await;
    }

    /// Serves all events to completion, then wait for the channel to close.
    async fn serve_all_events_then_hang(
        mut request_stream: ftest_manager::SuiteControllerRequestStream,
        events: Vec<ftest_manager::SuiteEvent>,
    ) {
        const BATCH_SIZE: usize = 5;
        let mut event_iter = events.into_iter();
        while event_iter.len() > 0 {
            respond_to_get_events(
                &mut request_stream,
                event_iter.by_ref().take(BATCH_SIZE).collect(),
            )
            .await;
        }
        let _requests = request_stream.collect::<Vec<_>>().await;
    }

    /// Creates a SuiteEvent which is unpopulated, except for timestamp.
    /// This isn't representative of an actual event from test framework, but is sufficient
    /// to assert events are routed correctly.
    fn create_empty_event(timestamp: i64) -> ftest_manager::SuiteEvent {
        ftest_manager::SuiteEvent { timestamp: Some(timestamp), ..Default::default() }
    }

    macro_rules! assert_empty_events_eq {
        ($t1:expr, $t2:expr) => {
            assert_eq!($t1.timestamp, $t2.timestamp, "Got incorrect event.")
        };
    }

    #[fuchsia::test]
    async fn running_suite_events_simple() {
        let (suite_proxy, mut suite_request_stream) =
            create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
                .expect("create proxy");
        let suite_server_task = fasync::Task::spawn(async move {
            respond_to_get_events(&mut suite_request_stream, vec![create_empty_event(0)]).await;
            respond_to_get_events(&mut suite_request_stream, vec![]).await;
            drop(suite_request_stream);
        });

        let mut running_suite =
            RunningSuite::wait_for_start(suite_proxy, None, None, std::time::Duration::ZERO, None)
                .await;
        assert_empty_events_eq!(
            running_suite.event_stream.next().await.unwrap().unwrap(),
            create_empty_event(0)
        );
        assert!(running_suite.event_stream.next().await.is_none());
        // polling again should still give none.
        assert!(running_suite.event_stream.next().await.is_none());
        suite_server_task.await;
    }

    #[fuchsia::test]
    async fn running_suite_events_multiple_events() {
        let (suite_proxy, mut suite_request_stream) =
            create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
                .expect("create proxy");
        let suite_server_task = fasync::Task::spawn(async move {
            respond_to_get_events(
                &mut suite_request_stream,
                vec![create_empty_event(0), create_empty_event(1)],
            )
            .await;
            respond_to_get_events(
                &mut suite_request_stream,
                vec![create_empty_event(2), create_empty_event(3)],
            )
            .await;
            respond_to_get_events(&mut suite_request_stream, vec![]).await;
            drop(suite_request_stream);
        });

        let mut running_suite =
            RunningSuite::wait_for_start(suite_proxy, None, None, std::time::Duration::ZERO, None)
                .await;

        for num in 0..4 {
            assert_empty_events_eq!(
                running_suite.event_stream.next().await.unwrap().unwrap(),
                create_empty_event(num)
            );
        }
        assert!(running_suite.event_stream.next().await.is_none());
        suite_server_task.await;
    }

    #[fuchsia::test]
    async fn running_suite_events_peer_closed() {
        let (suite_proxy, mut suite_request_stream) =
            create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
                .expect("create proxy");
        let suite_server_task = fasync::Task::spawn(async move {
            respond_to_get_events(&mut suite_request_stream, vec![create_empty_event(1)]).await;
            drop(suite_request_stream);
        });

        let mut running_suite =
            RunningSuite::wait_for_start(suite_proxy, None, None, std::time::Duration::ZERO, None)
                .await;
        assert_empty_events_eq!(
            running_suite.event_stream.next().await.unwrap().unwrap(),
            create_empty_event(1)
        );
        assert_matches!(
            running_suite.event_stream.next().await,
            Some(Err(RunTestSuiteError::Fidl(fidl::Error::ClientChannelClosed { .. })))
        );
        suite_server_task.await;
    }

    fn suite_event_from_payload(
        timestamp: i64,
        payload: ftest_manager::SuiteEventPayload,
    ) -> ftest_manager::SuiteEvent {
        let mut event = create_empty_event(timestamp);
        event.payload = Some(payload);
        event
    }

    fn case_found_event(timestamp: i64, identifier: u32, name: &str) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::CaseFound(ftest_manager::CaseFound {
                test_case_name: name.into(),
                identifier,
            }),
        )
    }

    fn case_started_event(timestamp: i64, identifier: u32) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::CaseStarted(ftest_manager::CaseStarted {
                identifier,
            }),
        )
    }

    fn case_stopped_event(
        timestamp: i64,
        identifier: u32,
        status: ftest_manager::CaseStatus,
    ) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::CaseStopped(ftest_manager::CaseStopped {
                identifier,
                status,
            }),
        )
    }

    fn case_finished_event(timestamp: i64, identifier: u32) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::CaseFinished(ftest_manager::CaseFinished {
                identifier,
            }),
        )
    }

    fn case_stdout_event(
        timestamp: i64,
        identifier: u32,
        stdout: fidl::Socket,
    ) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::CaseArtifact(ftest_manager::CaseArtifact {
                identifier,
                artifact: ftest_manager::Artifact::Stdout(stdout),
            }),
        )
    }

    fn case_stderr_event(
        timestamp: i64,
        identifier: u32,
        stderr: fidl::Socket,
    ) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::CaseArtifact(ftest_manager::CaseArtifact {
                identifier,
                artifact: ftest_manager::Artifact::Stderr(stderr),
            }),
        )
    }

    fn suite_started_event(timestamp: i64) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::SuiteStarted(ftest_manager::SuiteStarted),
        )
    }

    fn suite_stopped_event(
        timestamp: i64,
        status: ftest_manager::SuiteStatus,
    ) -> ftest_manager::SuiteEvent {
        suite_event_from_payload(
            timestamp,
            ftest_manager::SuiteEventPayload::SuiteStopped(ftest_manager::SuiteStopped { status }),
        )
    }

    #[fuchsia::test]
    async fn collect_suite_events_simple() {
        let all_events = vec![
            suite_started_event(0),
            case_found_event(100, 0, "my_test_case"),
            case_started_event(200, 0),
            case_stopped_event(300, 0, ftest_manager::CaseStatus::Passed),
            case_finished_event(400, 0),
            suite_stopped_event(500, ftest_manager::SuiteStatus::Passed),
        ];

        let (proxy, stream) = create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
            .expect("create stream");
        let test_fut = async move {
            let reporter = output::InMemoryReporter::new();
            let run_reporter = output::RunReporter::new(reporter.clone());
            let suite_reporter =
                run_reporter.new_suite("test-url", &SuiteId(0)).expect("create new suite");

            let suite =
                RunningSuite::wait_for_start(proxy, None, None, std::time::Duration::ZERO, None)
                    .await;
            assert_eq!(
                run_suite_and_collect_logs(
                    suite,
                    &suite_reporter,
                    diagnostics::LogDisplayConfiguration::default(),
                    futures::future::pending()
                )
                .await
                .expect("collect results"),
                Outcome::Passed
            );
            suite_reporter.finished().expect("Reporter finished");

            let reports = reporter.get_reports();
            let case = reports
                .iter()
                .find(|report| report.id == EntityId::Case { suite: SuiteId(0), case: CaseId(0) })
                .unwrap();
            assert_eq!(case.report.name, "my_test_case");
            assert_eq!(case.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(case.report.is_finished);
            assert!(case.report.artifacts.is_empty());
            assert!(case.report.directories.is_empty());
            let suite =
                reports.iter().find(|report| report.id == EntityId::Suite(SuiteId(0))).unwrap();
            assert_eq!(suite.report.name, "test-url");
            assert_eq!(suite.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(suite.report.is_finished);
            assert!(suite.report.artifacts.is_empty());
            assert!(suite.report.directories.is_empty());
        };

        futures::future::join(serve_all_events(stream, all_events), test_fut).await;
    }

    #[fuchsia::test]
    async fn collect_suite_events_with_case_artifacts() {
        const STDOUT_CONTENT: &str = "stdout from my_test_case";
        const STDERR_CONTENT: &str = "stderr from my_test_case";

        let (stdout_write, stdout_read) = fidl::Socket::create_stream();
        let (stderr_write, stderr_read) = fidl::Socket::create_stream();
        let all_events = vec![
            suite_started_event(0),
            case_found_event(100, 0, "my_test_case"),
            case_started_event(200, 0),
            case_stdout_event(300, 0, stdout_read),
            case_stderr_event(300, 0, stderr_read),
            case_stopped_event(300, 0, ftest_manager::CaseStatus::Passed),
            case_finished_event(400, 0),
            suite_stopped_event(500, ftest_manager::SuiteStatus::Passed),
        ];

        let (proxy, stream) = create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
            .expect("create stream");
        let stdio_write_fut = async move {
            let mut async_stdout =
                fasync::Socket::from_socket(stdout_write).expect("make async socket");
            async_stdout.write_all(STDOUT_CONTENT.as_bytes()).await.expect("write to socket");
            let mut async_stderr =
                fasync::Socket::from_socket(stderr_write).expect("make async socket");
            async_stderr.write_all(STDERR_CONTENT.as_bytes()).await.expect("write to socket");
        };
        let test_fut = async move {
            let reporter = output::InMemoryReporter::new();
            let run_reporter = output::RunReporter::new(reporter.clone());
            let suite_reporter =
                run_reporter.new_suite("test-url", &SuiteId(0)).expect("create new suite");

            let suite =
                RunningSuite::wait_for_start(proxy, None, None, std::time::Duration::ZERO, None)
                    .await;
            assert_eq!(
                run_suite_and_collect_logs(
                    suite,
                    &suite_reporter,
                    diagnostics::LogDisplayConfiguration::default(),
                    futures::future::pending()
                )
                .await
                .expect("collect results"),
                Outcome::Passed
            );
            suite_reporter.finished().expect("Reporter finished");

            let reports = reporter.get_reports();
            let case = reports
                .iter()
                .find(|report| report.id == EntityId::Case { suite: SuiteId(0), case: CaseId(0) })
                .unwrap();
            assert_eq!(case.report.name, "my_test_case");
            assert_eq!(case.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(case.report.is_finished);
            assert_eq!(case.report.artifacts.len(), 2);
            assert_eq!(
                case.report
                    .artifacts
                    .iter()
                    .map(|(artifact_type, artifact)| (*artifact_type, artifact.get_contents()))
                    .collect::<HashMap<_, _>>(),
                hashmap! {
                    output::ArtifactType::Stdout => STDOUT_CONTENT.as_bytes().to_vec(),
                    output::ArtifactType::Stderr => STDERR_CONTENT.as_bytes().to_vec()
                }
            );
            assert!(case.report.directories.is_empty());

            let suite =
                reports.iter().find(|report| report.id == EntityId::Suite(SuiteId(0))).unwrap();
            assert_eq!(suite.report.name, "test-url");
            assert_eq!(suite.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(suite.report.is_finished);
            assert!(suite.report.artifacts.is_empty());
            assert!(suite.report.directories.is_empty());
        };

        futures::future::join3(serve_all_events(stream, all_events), stdio_write_fut, test_fut)
            .await;
    }

    #[fuchsia::test]
    async fn collect_suite_events_case_artifacts_complete_after_suite() {
        const STDOUT_CONTENT: &str = "stdout from my_test_case";
        const STDERR_CONTENT: &str = "stderr from my_test_case";

        let (stdout_write, stdout_read) = fidl::Socket::create_stream();
        let (stderr_write, stderr_read) = fidl::Socket::create_stream();
        let all_events = vec![
            suite_started_event(0),
            case_found_event(100, 0, "my_test_case"),
            case_started_event(200, 0),
            case_stdout_event(300, 0, stdout_read),
            case_stderr_event(300, 0, stderr_read),
            case_stopped_event(300, 0, ftest_manager::CaseStatus::Passed),
            case_finished_event(400, 0),
            suite_stopped_event(500, ftest_manager::SuiteStatus::Passed),
        ];

        let (proxy, stream) = create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
            .expect("create stream");
        let serve_fut = async move {
            // server side will send all events, then write to (and close) sockets.
            serve_all_events(stream, all_events).await;
            let mut async_stdout =
                fasync::Socket::from_socket(stdout_write).expect("make async socket");
            async_stdout.write_all(STDOUT_CONTENT.as_bytes()).await.expect("write to socket");
            let mut async_stderr =
                fasync::Socket::from_socket(stderr_write).expect("make async socket");
            async_stderr.write_all(STDERR_CONTENT.as_bytes()).await.expect("write to socket");
        };
        let test_fut = async move {
            let reporter = output::InMemoryReporter::new();
            let run_reporter = output::RunReporter::new(reporter.clone());
            let suite_reporter =
                run_reporter.new_suite("test-url", &SuiteId(0)).expect("create new suite");

            let suite =
                RunningSuite::wait_for_start(proxy, None, None, std::time::Duration::ZERO, Some(1))
                    .await;
            assert_eq!(
                run_suite_and_collect_logs(
                    suite,
                    &suite_reporter,
                    diagnostics::LogDisplayConfiguration::default(),
                    futures::future::pending()
                )
                .await
                .expect("collect results"),
                Outcome::Passed
            );
            suite_reporter.finished().expect("Reporter finished");

            let reports = reporter.get_reports();
            let case = reports
                .iter()
                .find(|report| report.id == EntityId::Case { suite: SuiteId(0), case: CaseId(0) })
                .unwrap();
            assert_eq!(case.report.name, "my_test_case");
            assert_eq!(case.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(case.report.is_finished);
            assert_eq!(case.report.artifacts.len(), 2);
            assert_eq!(
                case.report
                    .artifacts
                    .iter()
                    .map(|(artifact_type, artifact)| (*artifact_type, artifact.get_contents()))
                    .collect::<HashMap<_, _>>(),
                hashmap! {
                    output::ArtifactType::Stdout => STDOUT_CONTENT.as_bytes().to_vec(),
                    output::ArtifactType::Stderr => STDERR_CONTENT.as_bytes().to_vec()
                }
            );
            assert!(case.report.directories.is_empty());

            let suite =
                reports.iter().find(|report| report.id == EntityId::Suite(SuiteId(0))).unwrap();
            assert_eq!(suite.report.name, "test-url");
            assert_eq!(suite.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(suite.report.is_finished);
            assert!(suite.report.artifacts.is_empty());
            assert!(suite.report.directories.is_empty());
        };

        futures::future::join(serve_fut, test_fut).await;
    }

    #[fuchsia::test]
    async fn collect_suite_events_with_case_artifacts_sent_after_case_stopped() {
        const STDOUT_CONTENT: &str = "stdout from my_test_case";
        const STDERR_CONTENT: &str = "stderr from my_test_case";

        let (stdout_write, stdout_read) = fidl::Socket::create_stream();
        let (stderr_write, stderr_read) = fidl::Socket::create_stream();
        let all_events = vec![
            suite_started_event(0),
            case_found_event(100, 0, "my_test_case"),
            case_started_event(200, 0),
            case_stopped_event(300, 0, ftest_manager::CaseStatus::Passed),
            case_stdout_event(300, 0, stdout_read),
            case_stderr_event(300, 0, stderr_read),
            case_finished_event(400, 0),
            suite_stopped_event(500, ftest_manager::SuiteStatus::Passed),
        ];

        let (proxy, stream) = create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
            .expect("create stream");
        let stdio_write_fut = async move {
            let mut async_stdout =
                fasync::Socket::from_socket(stdout_write).expect("make async socket");
            async_stdout.write_all(STDOUT_CONTENT.as_bytes()).await.expect("write to socket");
            let mut async_stderr =
                fasync::Socket::from_socket(stderr_write).expect("make async socket");
            async_stderr.write_all(STDERR_CONTENT.as_bytes()).await.expect("write to socket");
        };
        let test_fut = async move {
            let reporter = output::InMemoryReporter::new();
            let run_reporter = output::RunReporter::new(reporter.clone());
            let suite_reporter =
                run_reporter.new_suite("test-url", &SuiteId(0)).expect("create new suite");

            let suite =
                RunningSuite::wait_for_start(proxy, None, None, std::time::Duration::ZERO, None)
                    .await;
            assert_eq!(
                run_suite_and_collect_logs(
                    suite,
                    &suite_reporter,
                    diagnostics::LogDisplayConfiguration::default(),
                    futures::future::pending()
                )
                .await
                .expect("collect results"),
                Outcome::Passed
            );
            suite_reporter.finished().expect("Reporter finished");

            let reports = reporter.get_reports();
            let case = reports
                .iter()
                .find(|report| report.id == EntityId::Case { suite: SuiteId(0), case: CaseId(0) })
                .unwrap();
            assert_eq!(case.report.name, "my_test_case");
            assert_eq!(case.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(case.report.is_finished);
            assert_eq!(case.report.artifacts.len(), 2);
            assert_eq!(
                case.report
                    .artifacts
                    .iter()
                    .map(|(artifact_type, artifact)| (*artifact_type, artifact.get_contents()))
                    .collect::<HashMap<_, _>>(),
                hashmap! {
                    output::ArtifactType::Stdout => STDOUT_CONTENT.as_bytes().to_vec(),
                    output::ArtifactType::Stderr => STDERR_CONTENT.as_bytes().to_vec()
                }
            );
            assert!(case.report.directories.is_empty());

            let suite =
                reports.iter().find(|report| report.id == EntityId::Suite(SuiteId(0))).unwrap();
            assert_eq!(suite.report.name, "test-url");
            assert_eq!(suite.report.outcome, Some(output::ReportedOutcome::Passed));
            assert!(suite.report.is_finished);
            assert!(suite.report.artifacts.is_empty());
            assert!(suite.report.directories.is_empty());
        };

        futures::future::join3(serve_all_events(stream, all_events), stdio_write_fut, test_fut)
            .await;
    }

    #[fuchsia::test]
    async fn collect_suite_events_timed_out_case_with_hanging_artifacts() {
        // create sockets and leave the server end open to simulate a hang.
        let (_stdout_write, stdout_read) = fidl::Socket::create_stream();
        let (_stderr_write, stderr_read) = fidl::Socket::create_stream();
        let all_events = vec![
            suite_started_event(0),
            case_found_event(100, 0, "my_test_case"),
            case_started_event(200, 0),
            case_stdout_event(300, 0, stdout_read),
            case_stderr_event(300, 0, stderr_read),
        ];

        let (proxy, stream) = create_proxy_and_stream::<ftest_manager::SuiteControllerMarker>()
            .expect("create stream");
        let test_fut = async move {
            let reporter = output::InMemoryReporter::new();
            let run_reporter = output::RunReporter::new(reporter.clone());
            let suite_reporter =
                run_reporter.new_suite("test-url", &SuiteId(0)).expect("create new suite");

            let suite = RunningSuite::wait_for_start(
                proxy,
                None,
                Some(std::time::Duration::from_secs(2)),
                std::time::Duration::ZERO,
                None,
            )
            .await;
            assert_eq!(
                run_suite_and_collect_logs(
                    suite,
                    &suite_reporter,
                    diagnostics::LogDisplayConfiguration::default(),
                    futures::future::pending()
                )
                .await
                .expect("collect results"),
                Outcome::Timedout
            );
            suite_reporter.finished().expect("Reporter finished");

            let reports = reporter.get_reports();
            let case = reports
                .iter()
                .find(|report| report.id == EntityId::Case { suite: SuiteId(0), case: CaseId(0) })
                .unwrap();
            assert_eq!(case.report.name, "my_test_case");
            assert_eq!(case.report.outcome, Some(output::ReportedOutcome::Timedout));
            assert!(case.report.is_finished);
            assert_eq!(case.report.artifacts.len(), 2);
            assert_eq!(
                case.report
                    .artifacts
                    .iter()
                    .map(|(artifact_type, artifact)| (*artifact_type, artifact.get_contents()))
                    .collect::<HashMap<_, _>>(),
                hashmap! {
                    output::ArtifactType::Stdout => vec![],
                    output::ArtifactType::Stderr => vec![]
                }
            );
            assert!(case.report.directories.is_empty());

            let suite =
                reports.iter().find(|report| report.id == EntityId::Suite(SuiteId(0))).unwrap();
            assert_eq!(suite.report.name, "test-url");
            assert_eq!(suite.report.outcome, Some(output::ReportedOutcome::Timedout));
            assert!(suite.report.is_finished);
            assert!(suite.report.artifacts.is_empty());
            assert!(suite.report.directories.is_empty());
        };

        futures::future::join(serve_all_events_then_hang(stream, all_events), test_fut).await;
    }
}
