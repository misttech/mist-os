// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42080002): Move this somewhere else and
// make the logic more generic.

use std::{cell::Cell, rc::Rc};

use diagnostics_data::{BuilderArgs, LogsData, LogsDataBuilder, Severity, Timestamp};
use event_listener::Event;
use fidl::endpoints::{DiscoverableProtocolMarker as _, RequestStream, ServerEnd};
use fidl_fuchsia_developer_ffx::{
    TargetCollectionMarker, TargetCollectionRequest, TargetMarker, TargetRequest,
};
use fidl_fuchsia_developer_remotecontrol::{
    IdentifyHostResponse, RemoteControlMarker, RemoteControlRequest,
};
use fidl_fuchsia_diagnostics::{
    LogInterestSelector, LogSettingsMarker, LogSettingsRequest, LogSettingsRequestStream,
    StreamMode,
};
use fidl_fuchsia_diagnostics_host::{
    ArchiveAccessorMarker, ArchiveAccessorRequest, ArchiveAccessorRequestStream,
};
use fuchsia_async::Task;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    StreamExt,
};

const NODENAME: &str = "Rust";

/// Test configuration
pub struct Configuration {
    pub messages: Vec<LogsData>,
    pub send_mode_event: bool,
    pub boot_timestamp: Cell<u64>,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            messages: vec![LogsDataBuilder::new(BuilderArgs {
                component_url: Some("ffx".into()),
                moniker: "ffx".into(),
                severity: Severity::Info,
                timestamp_nanos: Timestamp::from(0),
            })
            .set_pid(1)
            .set_tid(2)
            .set_message("Hello world!")
            .build()],
            send_mode_event: false,
            boot_timestamp: Cell::new(1),
        }
    }
}

pub struct Manager {
    environment: Environment,
    events_receiver: Option<UnboundedReceiver<TestEvent>>,
}

impl Manager {
    /// Create a new Manager
    pub fn new() -> Self {
        Self::new_with_config(Rc::new(Configuration::default()))
    }

    /// Create a new Manager
    pub fn new_with_config(config: Rc<Configuration>) -> Self {
        let (events_sender, events_receiver) = unbounded();
        Self {
            environment: Environment::new(events_sender, config),
            events_receiver: Some(events_receiver),
        }
    }

    pub fn get_environment(&self) -> Environment {
        self.environment.clone()
    }

    pub fn take_event_stream(&mut self) -> Option<UnboundedReceiver<TestEvent>> {
        self.events_receiver.take()
    }
}

/// Events that happen during execution of a test
#[derive(Debug)]
pub enum TestEvent {
    /// Log severity has been changed
    SeverityChanged(Vec<LogInterestSelector>),
    /// Log settings connection closed
    LogSettingsConnectionClosed,
    /// Log stream started with a specific mode.
    Connected(StreamMode),
}

/// Holds the test environment.
#[derive(Clone)]
pub struct Environment {
    event_sender: UnboundedSender<TestEvent>,
    pub config: Rc<Configuration>,
    disconnect_event: Rc<Event>,
}

impl Environment {
    /// Create a new Environment
    fn new(event_sender: UnboundedSender<TestEvent>, config: Rc<Configuration>) -> Self {
        Self { event_sender, config, disconnect_event: Rc::new(Event::new()) }
    }

    fn send_event(&mut self, event: TestEvent) {
        // Result intentionally ignored as the test might not need
        // to read the event and choose to close the channel instead.
        let _ = self.event_sender.unbounded_send(event);
    }

    /// Simulates Archivist crashing/disconnecting.  This will only work if there is an active
    /// connection to Archivist; i.e. callers probably want to call `check_for_message` prior to
    /// this.
    pub fn disconnect_target(&self) {
        self.disconnect_event.notify(usize::MAX);
    }

    /// Simulates a target reboot.
    pub fn reboot_target(&self, new_boot_time: u64) {
        self.config.boot_timestamp.set(new_boot_time);
        self.disconnect_target();
    }
}

async fn handle_archive_accessor(
    mut stream: ArchiveAccessorRequestStream,
    mut environment: Environment,
) {
    while let Some(Ok(ArchiveAccessorRequest::StreamDiagnostics {
        parameters,
        stream,
        responder,
    })) = stream.next().await
    {
        if environment.config.send_mode_event {
            environment.send_event(TestEvent::Connected(parameters.stream_mode.unwrap()));
        }
        // Ignore the result, because the client may choose to close the channel.
        let _ = responder.send();
        stream
            .write(serde_json::to_string(&environment.config.messages).unwrap().as_bytes())
            .unwrap();

        match parameters.stream_mode.unwrap() {
            StreamMode::Snapshot => {}
            StreamMode::SnapshotThenSubscribe | StreamMode::Subscribe => {
                environment.disconnect_event.listen().await
            }
        }
    }
}

async fn handle_log_settings(channel: fidl::Channel, mut environment: Environment) {
    let mut stream =
        LogSettingsRequestStream::from_channel(fuchsia_async::Channel::from_channel(channel));
    while let Some(Ok(request)) = stream.next().await {
        match request {
            LogSettingsRequest::RegisterInterest { .. } => {
                panic!("fuchsia.diagnostics/LogSettings.RegisterInterest is not supported");
            }
            LogSettingsRequest::SetInterest { selectors, responder } => {
                environment.send_event(TestEvent::SeverityChanged(selectors));
                responder.send().unwrap();
            }
        }
    }
    environment.send_event(TestEvent::LogSettingsConnectionClosed);
}

async fn handle_open_capability(
    capability_name: &str,
    channel: fidl::Channel,
    environment: Environment,
) {
    let Some(capability_name) = capability_name.strip_prefix("svc/") else {
        panic!("Expected a protocol starting with svc/. Got: {capability_name}");
    };
    match capability_name {
        ArchiveAccessorMarker::PROTOCOL_NAME => {
            handle_archive_accessor(
                ServerEnd::<ArchiveAccessorMarker>::new(channel).into_stream().unwrap(),
                environment.clone(),
            )
            .await
        }
        LogSettingsMarker::PROTOCOL_NAME => handle_log_settings(channel, environment.clone()).await,
        other => {
            unreachable!("Attempted to connect to {other:?}");
        }
    }
}

pub async fn handle_rcs_connection(
    connection: ServerEnd<RemoteControlMarker>,
    environment: Environment,
) {
    let mut stream = connection.into_stream().unwrap();
    while let Some(Ok(request)) = stream.next().await {
        match request {
            RemoteControlRequest::IdentifyHost { responder } => {
                responder
                    .send(Ok(&IdentifyHostResponse {
                        nodename: Some(NODENAME.into()),
                        boot_timestamp_nanos: Some(environment.config.boot_timestamp.get()),
                        ..Default::default()
                    }))
                    .unwrap();
            }
            RemoteControlRequest::OpenCapability {
                moniker,
                capability_set,
                capability_name,
                server_channel,
                flags: _,
                responder,
            } => {
                assert_eq!(moniker, rcs::toolbox::MONIKER);
                assert_eq!(capability_set, rcs::OpenDirType::NamespaceDir);
                let environment_2 = environment.clone();
                Task::local(async move {
                    handle_open_capability(&capability_name, server_channel, environment_2).await
                })
                .detach();
                responder.send(Ok(())).unwrap();
            }
            _ => {
                unreachable!();
            }
        }
    }
}
async fn handle_target_connection(connection: ServerEnd<TargetMarker>, environment: Environment) {
    let mut stream = connection.into_stream().unwrap();
    while let Some(Ok(TargetRequest::OpenRemoteControl { remote_control, responder })) =
        stream.next().await
    {
        Task::local(handle_rcs_connection(remote_control, environment.clone())).detach();
        responder.send(Ok(())).unwrap();
    }
}

pub async fn handle_target_collection_connection(
    connection: ServerEnd<TargetCollectionMarker>,
    environment: Environment,
) {
    let mut stream = connection.into_stream().unwrap();

    while let Some(Ok(TargetCollectionRequest::OpenTarget { query, target_handle, responder })) =
        stream.next().await
    {
        assert_eq!(query.string_matcher, Some(NODENAME.into()));
        Task::local(handle_target_connection(target_handle, environment.clone())).detach();
        responder.send(Ok(())).unwrap();
    }
}
