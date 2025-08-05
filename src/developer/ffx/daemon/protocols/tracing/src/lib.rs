// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use async_fs::File;
use async_lock::{Mutex, MutexGuard};
use async_trait::async_trait;
use fidl_fuchsia_tracing_controller::{ProvisionerProxy, TraceConfig};
use futures::prelude::*;
use protocols::prelude::*;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;
use tasks::TaskManager;
use trace_task::{TraceTask, TracingError, TriggerAction};
use {fidl_fuchsia_developer_ffx as ffx, fidl_fuchsia_tracing_controller as trace};

/// Struct to hold on to an instance ot TraceTask started by the daemon.
#[derive(Debug)]
struct TraceTaskEntry {
    pub task: TraceTask,
    pub target_info: ffx::TargetInfo,
    pub output_file: String,
}
#[derive(Default, Debug)]
struct TraceMap {
    nodename_to_task: HashMap<String, TraceTaskEntry>,
    output_file_to_nodename: HashMap<String, String>,
}

#[ffx_protocol]
#[derive(Default)]
pub struct TracingProtocol {
    task_map: Rc<Mutex<TraceMap>>,
    iter_tasks: TaskManager,
}

async fn get_controller_proxy(
    target_query: Option<&String>,
    cx: &Context,
) -> Result<(ffx::TargetInfo, trace::ProvisionerProxy)> {
    let (target, proxy) = cx
        .open_target_proxy_with_info::<trace::ProvisionerMarker>(
            target_query.cloned(),
            "/core/trace_manager",
        )
        .await?;
    Ok((target, proxy))
}

impl TracingProtocol {
    async fn remove_output_file_or_find_target_nodename(
        &self,
        cx: &Context,
        task_map: &mut MutexGuard<'_, TraceMap>,
        output_file: &String,
    ) -> Result<String, ffx::RecordingError> {
        match task_map.output_file_to_nodename.remove(output_file) {
            Some(n) => Ok(n),
            None => {
                let target = cx
                    .get_target_collection()
                    .await
                    .map_err(|e| {
                        log::warn!("unable to get target collection: {:?}", e);
                        ffx::RecordingError::RecordingStop
                    })?
                    .query_single_enabled_target(&output_file.to_string().into())
                    .map_err(|_| {
                        log::warn!("target query '{output_file}' matches multiple targets");
                        ffx::RecordingError::NoSuchTarget
                    })?
                    .ok_or_else(|| {
                        log::warn!("target query '{}' matches no targets", output_file);
                        ffx::RecordingError::NoSuchTarget
                    })?;

                target.nodename().ok_or_else(|| {
                    log::warn!("target query '{}' matches target with no nodename", output_file);
                    ffx::RecordingError::DisconnectedTarget
                })
            }
        }
    }

    // StartRecording handler for the task protocol. The return
    // matches the API for start_recording; a target_info or a recording object,
    async fn start_recording<'a>(
        &self,
        target_info: &'a ffx::TargetInfo,
        provisioner: ProvisionerProxy,
        output_file: String,
        options: ffx::TraceOptions,
        trace_config: TraceConfig,
    ) -> Result<&'a ffx::TargetInfo, ffx::RecordingError> {
        let mut task_map = self.task_map.lock().await;

        // This should functionally never happen (a target whose nodename isn't
        // known after having been identified for service discovery would be a
        // critical error).
        let nodename = if let Some(name) = &target_info.nodename {
            name.clone()
        } else {
            return Err::<&ffx::TargetInfo, ffx::RecordingError>(
                ffx::RecordingError::TargetProxyOpen,
            );
        };
        match task_map.output_file_to_nodename.entry(output_file.clone()) {
            Entry::Occupied(_) => return Err(ffx::RecordingError::DuplicateTraceFile),
            Entry::Vacant(e) => {
                // Expand any configuration groups defined in config.
                let expanded_categories = match trace_config.categories.clone() {
                    Some(categories) => {
                        let context = match ffx_config::global_env_context()
                            .context("Discovering ffx environment context")
                        {
                            Ok(c) => c,
                            Err(e) => {
                                log::error!("Could not get global env context: {e}");
                                return Err(ffx::RecordingError::RecordingStart);
                            }
                        };

                        match ffx_trace::expand_categories(&context, categories) {
                            Ok(expanded_categories) => Some(expanded_categories),
                            Err(e) => {
                                log::error!("Could not expand categories: {e}");
                                return Err(ffx::RecordingError::RecordingStart);
                            }
                        }
                    }
                    None => None,
                };
                let config_with_expanded_categories =
                    trace::TraceConfig { categories: expanded_categories, ..trace_config.clone() };

                let writer = match File::create(output_file.clone()).await {
                    Ok(f) => f,
                    Err(e) => {
                        log::warn!("unable to create trace file: {:?}", e);
                        return Err(ffx::RecordingError::RecordingStart);
                    }
                };
                let task = match TraceTask::new(
                    // Use the target info as the task name
                    format!("{target_info:?}"),
                    writer,
                    config_with_expanded_categories,
                    options.duration_ns.map(|d| Duration::from_nanos(d as u64)),
                    options
                        .triggers
                        .map(|tv| tv.iter().map(from_ffx_trigger).collect())
                        .unwrap_or(vec![]),
                    provisioner,
                )
                .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        log::warn!("unable to start trace: {:?}", e);
                        return Err(to_recording_error(e));
                    }
                };
                e.insert(nodename.clone());
                task_map.nodename_to_task.insert(
                    nodename,
                    TraceTaskEntry { task, target_info: target_info.clone(), output_file },
                );
            }
        }
        Ok(target_info)
    }
}

#[async_trait(?Send)]
impl FidlProtocol for TracingProtocol {
    type Protocol = ffx::TracingMarker;
    type StreamHandler = FidlStreamHandler<Self>;

    async fn handle(&self, cx: &Context, req: ffx::TracingRequest) -> Result<()> {
        match req {
            ffx::TracingRequest::StartRecording {
                target_query,
                output_file,
                options,
                target_config,
                responder,
            } => {
                let (target_info, provisioner) =
                    match get_controller_proxy(target_query.string_matcher.as_ref(), cx).await {
                        Ok(p) => p,
                        Err(e) => {
                            log::warn!("getting target controller proxy: {:?}", e);
                            return responder
                                .send(Err(ffx::RecordingError::TargetProxyOpen))
                                .map_err(Into::into);
                        }
                    };
                if target_info.nodename.is_none() {
                    let target_query = target_query.string_matcher;
                    // This should functionally never happen (a target whose nodename isn't
                    // known after having been identified for service discovery would be a
                    // critical error).
                    log::warn!(
                        "query does not match a valid target with nodename: {:?}",
                        target_query
                    );
                    return responder
                        .send(Err(ffx::RecordingError::TargetProxyOpen))
                        .map_err(Into::into);
                }
                let result = self
                    .start_recording(&target_info, provisioner, output_file, options, target_config)
                    .await;
                responder.send(result).map_err(Into::into)
            }
            ffx::TracingRequest::StopRecording { name, responder } => {
                let task_entry = {
                    let mut task_map = self.task_map.lock().await;
                    let nodename = match self
                        .remove_output_file_or_find_target_nodename(cx, &mut task_map, &name)
                        .await
                    {
                        Ok(n) => n,
                        Err(e) => {
                            return responder.send(Err(e)).map_err(Into::into);
                        }
                    };
                    if let Some(task_entry) = task_map.nodename_to_task.remove(&nodename) {
                        // If we have found the task using nodename and not output file, the
                        // output_file_to_nodename mapping might still be around. Explicitly
                        // remove it to be sure.
                        let _ = task_map.output_file_to_nodename.remove(&task_entry.output_file);
                        task_entry
                    } else {
                        // TODO(https://fxbug.dev/42167418)
                        log::warn!("no task associated with trace file '{}'", name);
                        return responder
                            .send(Err(ffx::RecordingError::NoSuchTraceFile))
                            .map_err(Into::into);
                    }
                };
                let output_file = task_entry.output_file.clone();
                let target_info = task_entry.target_info.clone();
                let categories = task_entry.task.config().categories.unwrap_or_default();
                responder
                    .send(match task_entry.task.shutdown().await {
                        Ok(ref result) => Ok((&target_info, &output_file, &categories, result)),
                        Err(e) => Err(to_recording_error(e)),
                    })
                    .map_err(Into::into)
            }
            ffx::TracingRequest::Status { iterator, responder } => {
                let mut stream = iterator.into_stream();
                let res = self
                    .task_map
                    .lock()
                    .await
                    .nodename_to_task
                    .values()
                    .map(|t| ffx::TraceInfo {
                        target: Some(t.target_info.clone()),
                        output_file: Some(t.output_file.clone()),
                        duration: t.task.duration().map(|d| d.as_secs_f64()),
                        remaining_runtime: t.task.duration().map(|d| {
                            d.checked_sub(t.task.start_time().elapsed())
                                .unwrap_or(Duration::from_secs(0))
                                .as_secs_f64()
                        }),
                        config: Some(t.task.config()),
                        triggers: {
                            let tvec = t.task.triggers();
                            if !tvec.is_empty() {
                                Some(tvec.iter().map(to_ffx_trigger).collect())
                            } else {
                                None
                            }
                        },
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                self.iter_tasks.spawn(async move {
                    const CHUNK_SIZE: usize = 20;
                    let mut iter = res.chunks(CHUNK_SIZE).fuse();
                    while let Ok(Some(ffx::TracingStatusIteratorRequest::GetNext { responder })) =
                        stream.try_next().await
                    {
                        let _ = responder.send(iter.next().unwrap_or(&[])).map_err(|e| {
                            log::warn!("responding to tracing status iterator: {:?}", e);
                        });
                    }
                });
                responder.send().map_err(Into::into)
            }
        }
    }

    async fn stop(&mut self, _cx: &Context) -> Result<()> {
        let tasks = {
            let mut task_map = self.task_map.lock().await;
            task_map.output_file_to_nodename.clear();
            task_map.nodename_to_task.drain().map(|(_, v)| v.task.shutdown()).collect::<Vec<_>>()
        };
        futures::future::join_all(tasks).await;
        Ok(())
    }
}

fn to_ffx_trigger(t: &trace_task::Trigger) -> ffx::Trigger {
    ffx::Trigger {
        alert: t.alert.clone(),
        action: t.action.as_ref().map(|_a| ffx::Action::Terminate),
        ..Default::default()
    }
}

fn from_ffx_trigger(t: &ffx::Trigger) -> trace_task::Trigger {
    trace_task::Trigger {
        alert: t.alert.clone(),
        action: t.action.map(|_a| TriggerAction::Terminate),
    }
}
fn to_recording_error(e: TracingError) -> ffx::RecordingError {
    match e {
        TracingError::TargetProxyOpen => ffx::RecordingError::TargetProxyOpen,
        TracingError::RecordingAlreadyStarted => ffx::RecordingError::RecordingAlreadyStarted,
        TracingError::RecordingStop(_) => ffx::RecordingError::RecordingStop,
        TracingError::DuplicateTraceFile(_) => ffx::RecordingError::DuplicateTraceFile,
        TracingError::NoSuchTraceFile(_) => ffx::RecordingError::NoSuchTraceFile,
        TracingError::RecordingStart(_)
        | TracingError::FidlError(_)
        | TracingError::GeneralError(_) => ffx::RecordingError::RecordingStart,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_fs::File;
    use protocols::testing::FakeDaemonBuilder;
    use std::cell::RefCell;
    use trace_task::{TriggerAction, TriggersWatcher};

    const FAKE_CONTROLLER_TRACE_OUTPUT: &'static str = "HOWDY HOWDY HOWDY";

    #[derive(Default)]
    struct FakeProvisioner {
        start_error: Option<trace::StartError>,
    }

    #[async_trait(?Send)]
    impl FidlProtocol for FakeProvisioner {
        type Protocol = trace::ProvisionerMarker;
        type StreamHandler = FidlStreamHandler<Self>;

        async fn handle(&self, _cx: &Context, req: trace::ProvisionerRequest) -> Result<()> {
            match req {
                trace::ProvisionerRequest::InitializeTracing { controller, output, .. } => {
                    let start_error = self.start_error;
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
                                responder.send("").expect("Unable to send alert");
                            }
                            r => panic!("unexpected request: {:#?}", r),
                        }
                    }
                    Ok(())
                }
                r => panic!("unexpected request: {:#?}", r),
            }
        }
    }

    #[fuchsia::test]
    async fn test_trace_start_stop_write_check() {
        let daemon = FakeDaemonBuilder::new()
            .register_fidl_protocol::<FakeProvisioner>()
            .register_fidl_protocol::<TracingProtocol>()
            .target(ffx::TargetInfo { nodename: Some("foobar".to_string()), ..Default::default() })
            .build();
        let proxy = daemon.open_proxy::<ffx::TracingMarker>().await;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        proxy
            .start_recording(
                &ffx::TargetQuery {
                    string_matcher: Some("foobar".to_owned()),
                    ..Default::default()
                },
                &output,
                &ffx::TraceOptions::default(),
                &trace::TraceConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        proxy.stop_recording(&output).await.unwrap().unwrap();

        let mut f = File::open(std::path::PathBuf::from(output)).await.unwrap();
        let mut res = String::new();
        f.read_to_string(&mut res).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
    }

    #[fuchsia::test]
    async fn test_trace_error_double_start() {
        let daemon = FakeDaemonBuilder::new()
            .register_fidl_protocol::<FakeProvisioner>()
            .register_fidl_protocol::<TracingProtocol>()
            .target(ffx::TargetInfo { nodename: Some("foobar".to_string()), ..Default::default() })
            .build();
        let proxy = daemon.open_proxy::<ffx::TracingMarker>().await;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        proxy
            .start_recording(
                &ffx::TargetQuery {
                    string_matcher: Some("foobar".to_owned()),
                    ..Default::default()
                },
                &output,
                &ffx::TraceOptions::default(),
                &trace::TraceConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        // The target query needs to be empty here in order to fall back to checking
        // the trace file.
        assert_eq!(
            Err(ffx::RecordingError::DuplicateTraceFile),
            proxy
                .start_recording(
                    &ffx::TargetQuery::default(),
                    &output,
                    &ffx::TraceOptions::default(),
                    &trace::TraceConfig::default()
                )
                .await
                .unwrap()
        );
    }

    #[fuchsia::test]
    async fn test_trace_error_handling_already_started() {
        let fake_provisioner = Rc::new(RefCell::new(FakeProvisioner::default()));
        fake_provisioner.borrow_mut().start_error.replace(trace::StartError::AlreadyStarted);
        let daemon = FakeDaemonBuilder::new()
            .register_fidl_protocol::<TracingProtocol>()
            .inject_fidl_protocol(fake_provisioner)
            .target(ffx::TargetInfo { nodename: Some("foobar".to_string()), ..Default::default() })
            .build();
        let proxy = daemon.open_proxy::<ffx::TracingMarker>().await;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        assert_eq!(
            Err(ffx::RecordingError::RecordingAlreadyStarted),
            proxy
                .start_recording(
                    &ffx::TargetQuery {
                        string_matcher: Some("foobar".to_owned()),
                        ..Default::default()
                    },
                    &output,
                    &ffx::TraceOptions::default(),
                    &trace::TraceConfig::default()
                )
                .await
                .unwrap()
        );
    }

    #[fuchsia::test]
    async fn test_trace_error_handling_generic_start_error() {
        let fake_provisioner = Rc::new(RefCell::new(FakeProvisioner::default()));
        fake_provisioner.borrow_mut().start_error.replace(trace::StartError::NotInitialized);
        let daemon = FakeDaemonBuilder::new()
            .register_fidl_protocol::<TracingProtocol>()
            .inject_fidl_protocol(fake_provisioner)
            .target(ffx::TargetInfo { nodename: Some("foobar".to_string()), ..Default::default() })
            .build();
        let proxy = daemon.open_proxy::<ffx::TracingMarker>().await;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        assert_eq!(
            Err(ffx::RecordingError::RecordingStart),
            proxy
                .start_recording(
                    &ffx::TargetQuery {
                        string_matcher: Some("foobar".to_owned()),
                        ..Default::default()
                    },
                    &output,
                    &ffx::TraceOptions::default(),
                    &trace::TraceConfig::default()
                )
                .await
                .unwrap()
        );
    }

    #[fuchsia::test]
    async fn test_trace_shutdown_no_trace() {
        let daemon = FakeDaemonBuilder::new().register_fidl_protocol::<TracingProtocol>().build();
        let proxy = daemon.open_proxy::<ffx::TracingMarker>().await;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        assert_eq!(
            ffx::RecordingError::NoSuchTarget,
            proxy.stop_recording(&output).await.unwrap().unwrap_err()
        );
    }

    #[fuchsia::test]
    async fn test_trace_duration_shutdown_via_output_file() {
        let daemon = FakeDaemonBuilder::new()
            .register_fidl_protocol::<FakeProvisioner>()
            .target(ffx::TargetInfo { nodename: Some("foobar".to_owned()), ..Default::default() })
            .build();
        let protocol = Rc::new(RefCell::new(TracingProtocol::default()));
        let (proxy, _task) = protocols::testing::create_proxy(protocol.clone(), &daemon).await;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        proxy
            .start_recording(
                &ffx::TargetQuery {
                    string_matcher: Some("foobar".to_owned()),
                    ..Default::default()
                },
                &output,
                &ffx::TraceOptions { duration_ns: Some(500_000_000), ..Default::default() },
                &trace::TraceConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        proxy.stop_recording(&output).await.unwrap().unwrap();

        let mut f = File::open(std::path::PathBuf::from(output)).await.unwrap();
        let mut res = String::new();
        f.read_to_string(&mut res).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
        let task_map = protocol.borrow().task_map.clone();
        assert!(task_map.lock().await.nodename_to_task.is_empty());
    }

    #[fuchsia::test]
    async fn test_trace_duration_shutdown_via_nodename() {
        let daemon = FakeDaemonBuilder::new()
            .register_fidl_protocol::<FakeProvisioner>()
            .target(ffx::TargetInfo { nodename: Some("foobar".to_string()), ..Default::default() })
            .build();
        let protocol = Rc::new(RefCell::new(TracingProtocol::default()));
        let (proxy, _task) = protocols::testing::create_proxy(protocol.clone(), &daemon).await;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let output = temp_dir.path().join("trace-test.fxt").into_os_string().into_string().unwrap();
        proxy
            .start_recording(
                &ffx::TargetQuery {
                    string_matcher: Some("foobar".to_owned()),
                    ..Default::default()
                },
                &output,
                &ffx::TraceOptions { duration_ns: Some(500_000), ..Default::default() },
                &trace::TraceConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        proxy.stop_recording("foobar").await.unwrap().unwrap();

        let mut f = File::open(std::path::PathBuf::from(output)).await.unwrap();
        let mut res = String::new();
        f.read_to_string(&mut res).await.unwrap();
        assert_eq!(res, FAKE_CONTROLLER_TRACE_OUTPUT.to_string());
        let task_map = protocol.borrow().task_map.clone();
        assert!(task_map.lock().await.nodename_to_task.is_empty());
    }

    fn spawn_fake_alert_watcher(alert: &'static str) -> trace::SessionProxy {
        let (proxy, server) = fidl::endpoints::create_proxy::<trace::SessionMarker>();
        let mut stream = server.into_stream();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    trace::SessionRequest::WatchAlert { responder } => {
                        responder.send(alert).unwrap();
                    }
                    r => panic!("unexpected request in this test: {:?}", r),
                }
            }
        })
        .detach();
        proxy
    }

    #[fuchsia::test]
    async fn test_triggers_valid() {
        let proxy = spawn_fake_alert_watcher("foober");
        let (_sender, receiver) = async_channel::bounded::<()>(1);
        let triggers = vec![
            ffx::Trigger { alert: Some("foo".to_owned()), action: None, ..Default::default() },
            ffx::Trigger {
                alert: Some("foober".to_owned()),
                action: Some(ffx::Action::Terminate),
                ..Default::default()
            },
        ];
        let res =
            TriggersWatcher::new(proxy, triggers.iter().map(from_ffx_trigger).collect(), receiver)
                .await;
        assert_eq!(res, Some(TriggerAction::Terminate));
    }

    #[fuchsia::test]
    async fn test_triggers_server_dropped() {
        let (proxy, server) = fidl::endpoints::create_proxy::<trace::SessionMarker>();
        let (_sender, receiver) = async_channel::bounded::<()>(1);
        drop(server);
        let triggers = vec![
            ffx::Trigger { alert: Some("foo".to_owned()), action: None, ..Default::default() },
            ffx::Trigger {
                alert: Some("foober".to_owned()),
                action: Some(ffx::Action::Terminate),
                ..Default::default()
            },
        ];
        let res =
            TriggersWatcher::new(proxy, triggers.iter().map(from_ffx_trigger).collect(), receiver)
                .await;
        assert_eq!(res, None);
    }
}
