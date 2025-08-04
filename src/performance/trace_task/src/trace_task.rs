// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::triggers::{Trigger, TriggerAction, TriggersWatcher};
use crate::{trace_shutdown, TracingError};
use async_lock::Mutex;
use fidl_fuchsia_tracing_controller::{self as trace, StopResult, TraceConfig};
use fuchsia_async::Task;
use futures::io::AsyncWrite;
use futures::prelude::*;
use futures::task::{Context as FutContext, Poll};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

static SERIAL: AtomicU64 = AtomicU64::new(100);

#[derive(Debug)]
pub struct TraceTask {
    /// Unique identifier for this task. The value of this id monotonicallly increases.
    task_id: u64,
    /// Tag used to identify this task in the log.
    debug_tag: String,
    /// Trace configuration.
    config: trace::TraceConfig,
    /// Requested categories. These are unexpanded from the user.
    requested_categories: Vec<String>,
    /// Duration to capture trace. None indicates capture until canceled.
    duration: Option<Duration>,
    /// Triggers for terminating the trace.
    triggers: Vec<Trigger>,
    /// Trace session proxy to the tracing support on the device.
    proxy: Option<trace::SessionProxy>,
    /// The result of the trace task, None if incomplete.
    terminate_result: Rc<Mutex<Option<trace::StopResult>>>,
    /// Start time of the task.
    start_time: Instant,
    /// Channel used to shutdown this task.
    shutdown_sender: async_channel::Sender<()>,
    /// The task.
    task: Task<Option<trace::StopResult>>,
}

// This is just implemented for convenience so the wrapper is await-able.
impl Future for TraceTask {
    type Output = Option<trace::StopResult>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut FutContext<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.task).poll(cx)
    }
}

impl TraceTask {
    pub async fn new<W>(
        debug_tag: String,
        mut output_writer: W,
        config: trace::TraceConfig,
        duration: Option<Duration>,
        triggers: Vec<Trigger>,
        requested_categories: Option<Vec<String>>,
        provisioner: trace::ProvisionerProxy,
    ) -> Result<Self, TracingError>
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        // Start the tracing session immediately. Maybe we should consider separating the creating
        // of the session and the actual starting of it. This seems like a side-effect.
        let task_id = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (client, server) = fidl::Socket::create_stream();
        let client = fidl::AsyncSocket::from_socket(client);
        let (client_end, server_end) = fidl::endpoints::create_proxy::<trace::SessionMarker>();
        provisioner.initialize_tracing(server_end, &config, server)?;
        client_end
            .start_tracing(&trace::StartOptions::default())
            .await?
            .map_err(Into::<TracingError>::into)?;

        let logging_prefix_og = format!("Task {task_id} ({debug_tag})");

        let copy_trace_fut = {
            let logging_prefix = logging_prefix_og.clone();
            async move {
                log::debug!("{logging_prefix} starting background copying of trace data.");
                let res = futures::io::copy(client, &mut output_writer)
                    .await
                    .map_err(|e| log::warn!("output writer error: {:#?}", e));
                log::debug!("{logging_prefix} copying of trace data complete, result: {res:#?}");
                // async_fs files don't guarantee that the file is flushed on drop, so we need to
                // explicitly flush the file after writing.
                if let Err(err) = output_writer.flush().await {
                    log::warn!(
                        "{logging_prefix} error flushing trace data output writer error: {err:#?}"
                    );
                }
            }
        };

        let terminate_result = Rc::new(Mutex::new(None));
        let (shutdown_sender, shutdown_receiver) = async_channel::bounded::<()>(1);

        let controller = client_end.clone();
        let shutdown_controller = client_end.clone();
        let triggers_watcher =
            TriggersWatcher::new(controller, triggers.clone(), shutdown_receiver);

        let terminate_result_clone = terminate_result.clone();
        let shutdown_fut = {
            let logging_prefix = logging_prefix_og;
            async move {
                log::info!("{logging_prefix} Running shutdown future.");
                let mut done = terminate_result_clone.lock().await;
                if done.is_none() {
                    let result = trace_shutdown(&shutdown_controller).await;
                    match result {
                        Ok(stop) => {
                            log::debug!("{logging_prefix} call to trace_shutdown successful.");
                            *done = Some(stop)
                        }
                        Err(e) => {
                            log::error!("{logging_prefix} call to trace_shutdown failed: {e:?}");
                        }
                    }
                }
                // Remove the controller.
                drop(shutdown_controller);
            }
        };

        Ok(Self {
            task_id,
            debug_tag: debug_tag.clone(),
            config,
            proxy: Some(client_end),
            duration,
            triggers: triggers.clone(),
            requested_categories: requested_categories.unwrap_or_default(),
            terminate_result: terminate_result.clone(),
            start_time: Instant::now(),
            shutdown_sender,
            task: Self::make_task(
                task_id,
                debug_tag,
                duration,
                copy_trace_fut,
                shutdown_fut,
                triggers_watcher,
                terminate_result,
            ),
        })
    }

    /// Shutdown the tracing task.
    pub async fn shutdown(mut self) -> Result<trace::StopResult, TracingError> {
        {
            let proxy = self.proxy.take().expect("missing trace session proxy");
            let mut terminate_result_guard = self.terminate_result.lock().await;
            if terminate_result_guard.is_none() {
                match trace_shutdown(&proxy).await {
                    Ok(trace_result) => {
                        *terminate_result_guard = trace_result.into();
                    }
                    Err(e) => {
                        log::warn!(
                            "Trace {} ({}) trace_shutdown failed: {e:?}",
                            self.task_id,
                            self.debug_tag
                        );
                    }
                };
            }
        }
        let debug_tag = self.debug_tag.clone();
        let task_id = self.task_id;
        let terminate_result = self.terminate_result.clone();
        let _ = self.shutdown_sender.send(()).await;
        self.await;
        log::trace!("trace task {task_id} ({debug_tag}): task completed.");
        let terminate_result_guard = terminate_result.lock().await;
        Ok(terminate_result_guard.clone().unwrap_or_default())
    }

    fn make_task(
        task_id: u64,
        debug_tag: String,
        duration: Option<Duration>,
        copy_trace_fut: impl Future<Output = ()> + 'static,
        shutdown_fut: impl Future<Output = ()> + 'static,
        trigger_watcher: TriggersWatcher<'static>,
        terminate_result: Rc<Mutex<Option<StopResult>>>,
    ) -> Task<Option<trace::StopResult>> {
        Task::local(async move {
            let mut timeout_fut = Box::pin(async move {
                if let Some(duration) = duration {
                    fuchsia_async::Timer::new(duration).await;
                } else {
                    std::future::pending::<()>().await;
                }
            })
            .fuse();
            let mut copy_trace_fut = Box::pin(copy_trace_fut).fuse();
            let mut trigger_fut = trigger_watcher.fuse();

            futures::select! {
                // If copying the trace completes, that's fine.
                // This means the trace data has been processed, and there is nothing more for the
                // task to do.
                _ = copy_trace_fut => (),

                // Timeout, clean up and wait for copying to finish.
                _ = timeout_fut => {
                    log::info!("Trace {task_id} (debug_tag): timeout of {} successfully completed. Stopping and cleaning up.",
                     duration.map(|d| format!("{} secs", d.as_secs())).unwrap_or_else(|| "infinite?".into()));
                    // Shutdown the trace.
                    shutdown_fut.await;
                    // Drop triggers, they are no longer needed.
                    drop(trigger_fut);

                    // Wait for drop task and copy to complete.
                    copy_trace_fut.await;
                }
                // Trigger hit, shutdown and copy the trace.
                action = trigger_fut => {
                    if let Some(action) = action {
                        match action {
                            TriggerAction::Terminate => {
                                log::info!("Task {task_id} ({debug_tag}): received terminate trigger");
                            }
                        }
                    } else {
                        // This usually means the proxy was closed.
                        log::debug!("Task {task_id} ({debug_tag}): Trigger future completed without an action!");
                    }
                    shutdown_fut.await;
                    drop(trigger_fut);
                    // Wait for drop task and copy to complete.
                    copy_trace_fut.await;
                }
            };
            terminate_result.clone().lock().await.clone()
        })
    }

    pub fn triggers(&self) -> Vec<Trigger> {
        self.triggers.clone()
    }
    pub fn config(&self) -> TraceConfig {
        self.config.clone()
    }

    pub fn start_time(&self) -> Instant {
        self.start_time
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration.clone()
    }

    pub fn requested_categories(&self) -> Vec<String> {
        self.requested_categories.clone()
    }

    pub async fn wait_for_completion(self) -> Result<StopResult, TracingError> {
        match self.await {
            Some(result) => Ok(result),
            None => {
                Err(TracingError::GeneralError(anyhow::anyhow!("Tracing completion unsuccessful")))
            }
        }
    }

    pub async fn stop_result(&self) -> Option<trace::StopResult> {
        self.terminate_result.clone().lock().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_tracing_controller::StartError;
    use futures::io::BufWriter;
    const FAKE_CONTROLLER_TRACE_OUTPUT: &'static str = "HOWDY HOWDY HOWDY";

    fn setup_fake_provisioner_proxy(
        start_error: Option<StartError>,
        trigger_name: Option<&'static str>,
    ) -> trace::ProvisionerProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<trace::ProvisionerMarker>();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    trace::ProvisionerRequest::InitializeTracing { controller, output, .. } => {
                        let mut stream = controller.into_stream();
                        while let Ok(Some(req)) = stream.try_next().await {
                            match req {
                                trace::SessionRequest::StartTracing { responder, .. } => {
                                    let response = match start_error {
                                        Some(e) => Err(e),
                                        None => Ok(()),
                                    };
                                    responder.send(response).expect("Failed to start")
                                }
                                trace::SessionRequest::StopTracing { responder, payload } => {
                                    if start_error.is_some() {
                                        responder
                                            .send(Err(trace::StopError::NotStarted))
                                            .expect("Failed to stop")
                                    } else {
                                        assert_eq!(payload.write_results.unwrap(), true);
                                        assert_eq!(
                                            FAKE_CONTROLLER_TRACE_OUTPUT.len(),
                                            output
                                                .write(FAKE_CONTROLLER_TRACE_OUTPUT.as_bytes())
                                                .unwrap()
                                        );
                                        let stop_result = trace::StopResult {
                                            provider_stats: Some(vec![]),
                                            ..Default::default()
                                        };
                                        responder.send(Ok(&stop_result)).expect("Failed to stop")
                                    }
                                    break;
                                }
                                trace::SessionRequest::WatchAlert { responder } => {
                                    responder
                                        .send(trigger_name.unwrap_or(""))
                                        .expect("Unable to send alert");
                                }
                                r => panic!("unexpected request: {:#?}", r),
                            }
                        }
                    }
                    r => panic!("unexpected request: {:#?}", r),
                }
            }
        })
        .detach();
        proxy
    }

    #[fuchsia::test]
    async fn test_trace_task_start_stop_write_check_with_vec() {
        let provisioner = setup_fake_provisioner_proxy(None, None);
        let writer = BufWriter::new(Vec::new());

        let trace_task = TraceTask::new(
            "test_trace_start_stop_write_check".into(),
            writer,
            trace::TraceConfig::default(),
            None,
            vec![],
            None,
            provisioner,
        )
        .await
        .expect("tracing task started");

        let shutdown_result = trace_task.shutdown().await.expect("tracing shutdown");
        assert_eq!(shutdown_result, trace::StopResult::default().into());
    }

    #[cfg(not(target_os = "fuchsia"))]
    #[fuchsia::test]
    async fn test_trace_task_start_stop_write_check_with_file() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt");

        let provisioner = setup_fake_provisioner_proxy(None, None);
        let writer = async_fs::File::create(&output).await.unwrap();

        let trace_task = TraceTask::new(
            "test_trace_start_stop_write_check".into(),
            writer,
            trace::TraceConfig::default(),
            None,
            vec![],
            None,
            provisioner,
        )
        .await
        .expect("tracing task started");

        let shutdown_result = trace_task.shutdown().await.expect("tracing shutdown");

        let res = async_fs::read_to_string(&output).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
        let expected = trace::StopResult { provider_stats: Some(vec![]), ..Default::default() };
        assert_eq!(shutdown_result, expected);
    }

    #[fuchsia::test]
    async fn test_trace_error_handling_already_started() {
        let provisioner = setup_fake_provisioner_proxy(Some(StartError::AlreadyStarted), None);
        let writer = BufWriter::new(Vec::new());

        let trace_task_result = TraceTask::new(
            "test_trace_error_handling_already_started".into(),
            writer,
            trace::TraceConfig::default(),
            None,
            vec![],
            None,
            provisioner,
        )
        .await
        .err();

        assert_eq!(trace_task_result, Some(TracingError::RecordingAlreadyStarted));
    }

    #[cfg(not(target_os = "fuchsia"))]
    #[fuchsia::test]
    async fn test_trace_task_start_with_duration() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt");

        let provisioner = setup_fake_provisioner_proxy(None, None);
        let writer = async_fs::File::create(&output).await.unwrap();

        let trace_task = TraceTask::new(
            "test_trace_task_start_with_duration".into(),
            writer,
            trace::TraceConfig::default(),
            Some(Duration::from_millis(100)),
            vec![],
            None,
            provisioner,
        )
        .await
        .expect("tracing task started");

        if let Some(stop_result) = trace_task.await {
            assert!(stop_result.provider_stats.is_some());
        } else {
            panic!("Expected stop result from trace_task.await");
        }

        let mut f = async_fs::File::open(std::path::PathBuf::from(output)).await.unwrap();
        let mut res = String::new();
        f.read_to_string(&mut res).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
    }

    #[cfg(not(target_os = "fuchsia"))]
    #[fuchsia::test]
    async fn test_triggers_valid() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt");
        let alert_name = "some_alert";
        let provisioner = setup_fake_provisioner_proxy(None, Some(alert_name.into()));
        let writer = async_fs::File::create(output.clone()).await.unwrap();

        let trace_task = TraceTask::new(
            "test_triggers_valid".into(),
            writer,
            trace::TraceConfig::default(),
            None,
            vec![Trigger {
                alert: Some(alert_name.into()),
                action: Some(TriggerAction::Terminate),
            }],
            None,
            provisioner,
        )
        .await
        .expect("tracing task started");

        trace_task.await;

        let res = async_fs::read_to_string(&output).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
    }
}
