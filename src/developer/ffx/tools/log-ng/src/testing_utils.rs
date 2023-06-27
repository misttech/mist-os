// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/129623): Move this somewhere else and
// make the logic more generic.

use std::{future::Future, pin::Pin};

use diagnostics_data::{BuilderArgs, LogsDataBuilder, Severity, Timestamp};
use ffx_core::macro_deps::futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    future::{select, Either},
    stream::FuturesUnordered,
    StreamExt,
};
use fidl::endpoints::{DiscoverableProtocolMarker as _, RequestStream, ServerEnd};
use fidl_fuchsia_developer_ffx::{
    TargetCollectionMarker, TargetCollectionRequest, TargetMarker, TargetRequest,
};
use fidl_fuchsia_developer_remotecontrol::{
    IdentifyHostResponse, RemoteControlMarker, RemoteControlRequest,
};
use fidl_fuchsia_diagnostics::{
    LogInterestSelector, LogSettingsMarker, LogSettingsRequest, LogSettingsRequestStream,
};
use fidl_fuchsia_diagnostics_host::{
    ArchiveAccessorMarker, ArchiveAccessorRequest, ArchiveAccessorRequestStream,
};

const NODENAME: &str = "Rust";

/// A scoped task executor that receives tasks to run over a channel
/// and runs them to completion.
pub struct TaskManager {
    tasks: FuturesUnordered<Pin<Box<dyn Future<Output = ()>>>>,
    scheduler: TaskScheduler,
    receiver: UnboundedReceiver<Pin<Box<dyn Future<Output = ()>>>>,
    events_receiver: Option<UnboundedReceiver<TestEvent>>,
}

impl TaskManager {
    /// Create a new TaskManager
    pub fn new() -> Self {
        let (sender, receiver) = unbounded();
        let (events_sender, events_receiver) = unbounded();
        Self {
            tasks: FuturesUnordered::new(),
            scheduler: TaskScheduler::new(sender, events_sender),
            receiver,
            events_receiver: Some(events_receiver),
        }
    }

    pub fn get_scheduler(&self) -> TaskScheduler {
        self.scheduler.clone()
    }

    /// Spawn a task onto the task manager
    pub fn spawn(&self, task: impl Future<Output = ()> + 'static) {
        self.tasks.push(Box::pin(task));
    }

    /// Gets the result of executing a future, and sends it on a channel.
    async fn get_result<T: 'static>(
        task: impl Future<Output = T> + 'static,
        sender: UnboundedSender<T>,
    ) {
        // Not all tests may care about the return value, and may choose
        // to drop the receiver.
        let _ = sender.unbounded_send(task.await);
    }

    /// Spawns a task onto the task manager, and exposes a channel
    /// to read its eventual result from.
    pub fn spawn_result<T: 'static>(
        &self,
        task: impl Future<Output = T> + 'static,
    ) -> UnboundedReceiver<T> {
        let (sender, receiver) = unbounded();
        self.spawn(Box::pin(Self::get_result(task, sender)));
        receiver
    }

    pub fn take_event_stream(&mut self) -> Option<UnboundedReceiver<TestEvent>> {
        self.events_receiver.take()
    }

    /// Runs the task manager to completion, consuming the task manager.
    pub async fn run(self) {
        let mut receiver = self.receiver;
        let mut tasks = self.tasks;
        loop {
            let pin = Box::pin(tasks.next());
            match select(receiver.next(), pin).await {
                Either::Left((Some(task), _)) => {
                    tasks.push(task);
                }
                Either::Right((Some(_), _)) => {
                    // Nothing to do here
                }
                _ => {
                    break;
                }
            }
        }
    }
}

/// Events that happen during execution of a test
#[derive(Debug)]
pub enum TestEvent {
    /// Log severity has been changed
    SeverityChanged(Vec<LogInterestSelector>),
    /// Log settings connection closed
    LogSettingsConnectionClosed,
}

/// A task scheduler that spawns tasks onto the task manager
#[derive(Clone)]
pub struct TaskScheduler {
    sender: UnboundedSender<Pin<Box<dyn Future<Output = ()>>>>,
    event_sender: UnboundedSender<TestEvent>,
}

impl TaskScheduler {
    /// Create a new TaskScheduler
    fn new(
        sender: UnboundedSender<Pin<Box<dyn Future<Output = ()>>>>,
        event_sender: UnboundedSender<TestEvent>,
    ) -> Self {
        Self { sender, event_sender }
    }

    /// Spawn a task onto the task manager
    fn spawn(&self, task: impl Future<Output = ()> + 'static) {
        self.sender.unbounded_send(Box::pin(task)).unwrap();
    }

    fn send_event(&mut self, event: TestEvent) {
        // Result intentionally ignored as the test might not need
        // to read the event and choose to close the channel instead.
        let _ = self.event_sender.unbounded_send(event);
    }
}

async fn handle_archive_accessor(mut stream: ArchiveAccessorRequestStream) {
    while let Some(Ok(ArchiveAccessorRequest::StreamDiagnostics {
        parameters: _,
        stream,
        responder,
    })) = stream.next().await
    {
        // Ignore the result, because the client may choose to close the channel.
        let _ = responder.send();
        stream
            .write(
                serde_json::to_string(
                    &LogsDataBuilder::new(BuilderArgs {
                        component_url: Some("ffx".into()),
                        moniker: "ffx".into(),
                        severity: Severity::Info,
                        timestamp_nanos: Timestamp::from(0),
                    })
                    .set_message("Hello world!")
                    .build(),
                )
                .unwrap()
                .as_bytes(),
            )
            .unwrap();
    }
}

async fn handle_log_settings(channel: fidl::Channel, mut scheduler: TaskScheduler) {
    let mut stream = LogSettingsRequestStream::from_channel(
        fuchsia_async::Channel::from_channel(channel).unwrap(),
    );
    while let Some(Ok(LogSettingsRequest::SetInterest { selectors, responder })) =
        stream.next().await
    {
        scheduler.send_event(TestEvent::SeverityChanged(selectors));
        responder.send().unwrap();
    }
    scheduler.send_event(TestEvent::LogSettingsConnectionClosed);
}

async fn handle_connect_capability(
    capability_name: &str,
    channel: fidl::Channel,
    scheduler: TaskScheduler,
) {
    match capability_name {
        ArchiveAccessorMarker::PROTOCOL_NAME => {
            handle_archive_accessor(
                ServerEnd::<ArchiveAccessorMarker>::new(channel).into_stream().unwrap(),
            )
            .await
        }
        LogSettingsMarker::PROTOCOL_NAME => handle_log_settings(channel, scheduler.clone()).await,
        _ => {
            unreachable!();
        }
    }
}

pub async fn handle_rcs_connection(
    connection: ServerEnd<RemoteControlMarker>,
    scheduler: TaskScheduler,
) {
    let mut stream = connection.into_stream().unwrap();
    while let Some(Ok(request)) = stream.next().await {
        match request {
            RemoteControlRequest::IdentifyHost { responder } => {
                responder
                    .send(Ok(&IdentifyHostResponse {
                        nodename: Some(NODENAME.into()),
                        ..Default::default()
                    }))
                    .unwrap();
            }
            RemoteControlRequest::ConnectCapability {
                moniker: _,
                capability_name,
                server_chan,
                flags: _,
                responder,
            } => {
                let scheduler_2 = scheduler.clone();
                scheduler.spawn(async move {
                    handle_connect_capability(&capability_name, server_chan, scheduler_2).await
                });
                responder.send(Ok(())).unwrap();
            }
            _ => {
                unreachable!();
            }
        }
    }
}
async fn handle_target_connection(connection: ServerEnd<TargetMarker>, scheduler: TaskScheduler) {
    let mut stream = connection.into_stream().unwrap();
    while let Some(Ok(TargetRequest::OpenRemoteControl { remote_control, responder })) =
        stream.next().await
    {
        scheduler.spawn(handle_rcs_connection(remote_control, scheduler.clone()));
        responder.send(Ok(())).unwrap();
    }
}

pub async fn handle_target_collection_connection(
    connection: ServerEnd<TargetCollectionMarker>,
    scheduler: TaskScheduler,
) {
    let mut stream = connection.into_stream().unwrap();

    while let Some(Ok(TargetCollectionRequest::OpenTarget { query, target_handle, responder })) =
        stream.next().await
    {
        assert_eq!(query.string_matcher, Some(NODENAME.into()));
        scheduler.spawn(handle_target_connection(target_handle, scheduler.clone()));
        responder.send(Ok(())).unwrap();
    }
}
