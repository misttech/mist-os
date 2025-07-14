// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::triggers::{Trigger, TriggerAction, TriggersWatcher};
use crate::{trace_shutdown, TracingError};
use anyhow::Context as _;
use async_fs::File;
use async_lock::Mutex;
use fidl_fuchsia_tracing_controller::{self as trace, StopResult, TraceConfig};
use fuchsia_async::Task;
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
    /// Name of the output file for the trace data on the host machine.
    /// The [TraceTask] instance may use this name to make a temporary file on the device.
    /// The output_file will be used to rendezvous start and stop requests.
    output_file: String,
    /// Trace configuration.
    config: trace::TraceConfig,
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
    pub async fn new<F>(
        debug_tag: String,
        output_file: String,
        config: trace::TraceConfig,
        duration: Option<Duration>,
        triggers: Vec<Trigger>,
        provisioner: trace::ProvisionerProxy,
        on_complete: F,
    ) -> Result<Self, TracingError>
    where
        F: FnOnce() -> Pin<Box<dyn Future<Output = ()>>> + 'static,
    {
        // Start the tracing session immediately. Maybe we should consider separating the creating
        // of the session and the actual starting of it. This seems like a side-effect.
        let (client, server) = fidl::Socket::create_stream();
        let client = fidl::AsyncSocket::from_socket(client);
        let (client_end, server_end) = fidl::endpoints::create_proxy::<trace::SessionMarker>();
        provisioner.initialize_tracing(server_end, &config, server)?;
        client_end
            .start_tracing(&trace::StartOptions::default())
            .await?
            .map_err(Into::<TracingError>::into)?;

        // Set up copying the trace output to a file.
        let f = File::create(&output_file).await.context("opening file")?;
        let output_file_clone = output_file.clone();
        let debug_tag_clone = debug_tag.clone();

        let copy_trace_fut = async move {
            log::debug!("{} -> {output_file_clone} starting trace.", &debug_tag_clone);
            let mut out_file = f;
            let res = futures::io::copy(client, &mut out_file)
                .await
                .map_err(|e| log::warn!("file error: {:#?}", e));
            log::debug!(
                "{} -> {output_file_clone} trace complete, result: {res:#?}",
                &debug_tag_clone
            );
            // async_fs files don't guarantee that the file is flushed on drop, so we need to
            // explicitly flush the file after writing.
            if let Err(err) = out_file.flush().await {
                log::warn!("file error: {:#?}", err);
            }
        };

        let terminate_result = Rc::new(Mutex::new(None));
        let (shutdown_sender, shutdown_receiver) = async_channel::bounded::<()>(1);

        let controller = client_end.clone();
        let shutdown_controller = client_end.clone();
        let triggers_watcher =
            TriggersWatcher::new(controller, triggers.clone(), shutdown_receiver);

        let terminate_result_clone = terminate_result.clone();
        let shutdown_fut = async move {
            log::info!("shutting down trace");
            let mut done = terminate_result_clone.lock().await;
            if done.is_none() {
                let result = trace_shutdown(&shutdown_controller).await;
                match result {
                    Ok(stop) => {
                        log::debug!("recording stop successful");
                        *done = Some(stop)
                    }
                    Err(e) => {
                        log::error!("error shutting down trace: {:?}", e);
                    }
                }
            }
            // Remove the controller.
            drop(shutdown_controller);
        };

        Ok(Self {
            task_id: SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            debug_tag: debug_tag.clone(),
            config,
            proxy: Some(client_end),
            duration,
            triggers: triggers.clone(),
            terminate_result: terminate_result.clone(),
            start_time: Instant::now(),
            shutdown_sender,
            output_file: output_file.clone(),
            task: Self::make_task(
                duration,
                output_file.clone(),
                copy_trace_fut,
                shutdown_fut,
                triggers_watcher,
                on_complete,
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
                        log::warn!("error shutting down trace: {:?}", e);
                    }
                };
            }
        }
        let task_tag = self.debug_tag.clone();
        let task_id = self.task_id;
        let output_file = self.output_file.clone();
        let terminate_result = self.terminate_result.clone();
        let _ = self.shutdown_sender.send(()).await;
        self.await;
        log::trace!("trace task {task_tag}:{} -> {output_file} shutdown completed.", task_id);
        let terminate_result_guard = terminate_result.lock().await;
        Ok(terminate_result_guard.clone().unwrap_or_default())
    }

    fn make_task<F>(
        duration: Option<Duration>,
        output_file: String,
        copy_trace_fut: impl Future<Output = ()> + 'static,
        shutdown_fut: impl Future<Output = ()> + 'static,
        trigger_watcher: TriggersWatcher<'static>,
        on_complete: F,
        terminate_result: Rc<Mutex<Option<StopResult>>>,
    ) -> Task<Option<trace::StopResult>>
    where
        F: FnOnce() -> Pin<Box<dyn Future<Output = ()>>> + 'static,
    {
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

            // Wrap the callback in an Option to ensure it's only called once.
            let mut on_complete = Some(on_complete);

            futures::select! {
                    // If copying the trace completes, that's fine.
                    _ = copy_trace_fut => {},

                // Timeout, clean up and wait for copying to finish.
                _ = timeout_fut => {
                    log::debug!("timeout reached, doing cleanup");
                    // Shutdown the trace.
                    shutdown_fut.await;
                    // Drop triggers, they are no longer needed.
                    drop(trigger_fut);

                    // Wait for drop task and copy to complete.
                    if let Some(on_complete_fn) = on_complete.take() {
                    log::debug!("running on_complete callback for {}", output_file);
                    let _ = futures::join!(on_complete_fn(), copy_trace_fut);
                    } else {
                    copy_trace_fut.await;
                    }
                }
                // Trigger hit, shutdown and copy the trace.
                action = trigger_fut => {
                    if let Some(action) = action {
                        match action {
                            TriggerAction::Terminate => {
                                log::debug!("received terminate trigger");
                            }
                        }
                    }
                    shutdown_fut.await;
                    drop(trigger_fut);
                    // Wait for drop task and copy to complete.
                    if let Some(on_complete_fn) = on_complete.take() {
                    log::debug!("running on_complete callback for {}", output_file);
                    let _ = futures::join!(on_complete_fn(), copy_trace_fut);
                    } else {
                    copy_trace_fut.await;
                    }
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

    pub fn output_file(&self) -> String {
        self.output_file.clone()
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
    use async_fs::File;
    use fidl_fuchsia_tracing_controller::StartError;
    use futures::AsyncReadExt;
    use std::sync::atomic::{AtomicBool, Ordering};
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
                                        responder
                                            .send(Ok(&trace::StopResult::default()))
                                            .expect("Failed to stop")
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
    async fn test_trace_task_start_stop_write_check() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();

        let provisioner = setup_fake_provisioner_proxy(None, None);
        let completed: Rc<AtomicBool> = Rc::new(AtomicBool::new(false));
        let completed_flag = completed.clone();
        let on_complete_callback = move || {
            Box::pin(async move {
                completed_flag.store(true, Ordering::Relaxed);
            }) as Pin<Box<dyn Future<Output = ()> + 'static>>
        };

        let trace_task = TraceTask::new(
            "test_trace_start_stop_write_check".into(),
            output.clone(),
            trace::TraceConfig::default(),
            None,
            vec![],
            provisioner,
            on_complete_callback,
        )
        .await
        .expect("tracing task started");

        let shutdown_result = trace_task.shutdown().await.expect("tracing shutdown");

        let mut f = File::open(std::path::PathBuf::from(output)).await.unwrap();
        let mut res = String::new();
        f.read_to_string(&mut res).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
        assert!(completed.load(Ordering::Relaxed));
        assert_eq!(shutdown_result, trace::StopResult::default().into());
    }

    #[fuchsia::test]
    async fn test_trace_error_handling_already_started() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();

        let provisioner = setup_fake_provisioner_proxy(Some(StartError::AlreadyStarted), None);

        let completed: Rc<AtomicBool> = Rc::new(AtomicBool::new(false));
        let completed_flag = completed.clone();
        let on_complete_callback = move || {
            Box::pin(async move {
                completed_flag.store(true, Ordering::Relaxed);
            }) as Pin<Box<dyn Future<Output = ()> + 'static>>
        };

        let trace_task_result = TraceTask::new(
            "test_trace_error_handling_already_started".into(),
            output.clone(),
            trace::TraceConfig::default(),
            None,
            vec![],
            provisioner,
            on_complete_callback,
        )
        .await
        .err();

        assert_eq!(trace_task_result, Some(TracingError::RecordingAlreadyStarted));

        // On completed should not be invoked since the tracing never started.
        assert!(!completed.load(Ordering::Relaxed));
    }

    #[fuchsia::test]
    async fn test_trace_task_start_with_duration() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();

        let provisioner = setup_fake_provisioner_proxy(None, None);
        let completed: Rc<AtomicBool> = Rc::new(AtomicBool::new(false));
        let completed_flag = completed.clone();
        let on_complete_callback = move || {
            Box::pin(async move {
                completed_flag.store(true, Ordering::Relaxed);
            }) as Pin<Box<dyn Future<Output = ()> + 'static>>
        };

        let trace_task = TraceTask::new(
            "test_trace_task_start_with_duration".into(),
            output.clone(),
            trace::TraceConfig::default(),
            Some(Duration::from_millis(100)),
            vec![],
            provisioner,
            on_complete_callback,
        )
        .await
        .expect("tracing task started");

        trace_task.await;

        let mut f = File::open(std::path::PathBuf::from(output)).await.unwrap();
        let mut res = String::new();
        f.read_to_string(&mut res).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
        assert!(completed.load(Ordering::Relaxed));
    }

    #[fuchsia::test]
    async fn test_triggers_valid() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        let alert_name = "some_alert";
        let provisioner = setup_fake_provisioner_proxy(None, Some(alert_name.into()));
        let completed: Rc<AtomicBool> = Rc::new(AtomicBool::new(false));
        let completed_flag = completed.clone();
        let on_complete_callback = move || {
            Box::pin(async move {
                completed_flag.store(true, Ordering::Relaxed);
            }) as Pin<Box<dyn Future<Output = ()> + 'static>>
        };

        let trace_task = TraceTask::new(
            "test_triggers_valid".into(),
            output.clone(),
            trace::TraceConfig::default(),
            None,
            vec![Trigger {
                alert: Some(alert_name.into()),
                action: Some(TriggerAction::Terminate),
            }],
            provisioner,
            on_complete_callback,
        )
        .await
        .expect("tracing task started");

        trace_task.await;

        let mut f = File::open(std::path::PathBuf::from(output)).await.unwrap();
        let mut res = String::new();
        f.read_to_string(&mut res).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
        assert!(completed.load(Ordering::Relaxed));
    }
}
