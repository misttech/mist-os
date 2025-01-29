// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::{BuilderArgs, LogsData, LogsDataBuilder, Severity, Timestamp};
use fho::{FhoConnectionBehavior, FhoEnvironment, TryFromEnv};
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fidl_fuchsia_developer_remotecontrol::{
    IdentifyHostResponse, RemoteControlMarker, RemoteControlProxy, RemoteControlRequest,
};
use fidl_fuchsia_diagnostics::{
    LogInterestSelector, LogSettingsMarker, LogSettingsRequest, LogSettingsRequestStream,
    StreamMode,
};
use fidl_fuchsia_diagnostics_host::{
    ArchiveAccessorMarker, ArchiveAccessorRequest, ArchiveAccessorRequestStream,
};
use futures::channel::{mpsc, oneshot};
use futures::{Stream, StreamExt, TryStreamExt};
use log_command::parse_time;
use moniker::Moniker;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use target_connector::Connector;
use target_holders::{FakeInjector, RemoteControlProxyHolder};
use {fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync};

const NODENAME: &str = "Rust";

/// Test configuration
pub struct TestEnvironmentConfig {
    pub messages: Vec<LogsData>,
    pub boot_timestamp: u64,
    pub boot_id: Option<u64>,
    pub instances: Vec<Moniker>,
    pub send_connected_event: bool,
    pub show_initial_timestamp: bool,
}

pub fn test_log_with_severity(timestamp: i64, severity: Severity) -> LogsData {
    LogsDataBuilder::new(BuilderArgs {
        component_url: Some("ffx".into()),
        moniker: "host/ffx".try_into().unwrap(),
        severity,
        timestamp: Timestamp::from_nanos(timestamp),
    })
    .set_pid(1)
    .set_tid(2)
    .set_message("Hello world!")
    .build()
}

pub fn test_log(timestamp: i64) -> LogsData {
    LogsDataBuilder::new(BuilderArgs {
        component_url: Some("ffx".into()),
        moniker: "host/ffx".try_into().unwrap(),
        severity: Severity::Info,
        timestamp: Timestamp::from_nanos(timestamp),
    })
    .set_pid(1)
    .set_tid(2)
    .set_message("Hello world!")
    .build()
}

pub fn test_log_with_file(timestamp: i64) -> LogsData {
    LogsDataBuilder::new(BuilderArgs {
        component_url: Some("ffx".into()),
        moniker: "host/ffx".try_into().unwrap(),
        severity: Severity::Info,
        timestamp: Timestamp::from_nanos(timestamp),
    })
    .set_file("test_filename.cc")
    .set_line(42)
    .add_tag("test tag")
    .set_pid(1)
    .set_tid(2)
    .set_message("Hello world!")
    .build()
}

pub fn test_log_with_tag(timestamp: i64) -> LogsData {
    LogsDataBuilder::new(BuilderArgs {
        component_url: Some("ffx".into()),
        moniker: "host/ffx".try_into().unwrap(),
        severity: Severity::Info,
        timestamp: Timestamp::from_nanos(timestamp),
    })
    .add_tag("test tag")
    .set_pid(1)
    .set_tid(2)
    .set_message("Hello world!")
    .build()
}

pub fn naive_utc_nanos(utc_time: &str) -> i64 {
    parse_time(utc_time).unwrap().time.naive_utc().timestamp_nanos_opt().unwrap()
}

impl Default for TestEnvironmentConfig {
    fn default() -> Self {
        Self {
            messages: vec![test_log(0)],
            boot_timestamp: 1,
            instances: Vec::new(),
            send_connected_event: false,
            boot_id: Some(1),
            show_initial_timestamp: false,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum TestEvent {
    Connected(StreamMode),
    SetInterest(Vec<LogInterestSelector>),
    LogSettingsClosed,
}

pub struct TestEnvironment {
    fho_env: FhoEnvironment,
    state: Rc<State>,
    event_rcv: Option<mpsc::UnboundedReceiver<TestEvent>>,
    disconnect_snd: oneshot::Sender<()>,
}

impl TestEnvironment {
    pub async fn new(config: TestEnvironmentConfig) -> Self {
        let (event_snd, event_rcv) = mpsc::unbounded();
        let (disconnect_snd, disconnect_rcv) = oneshot::channel();
        let state = Rc::new(State::new(config, event_snd, disconnect_rcv));
        let state_clone = state.clone();
        let fake_injector = FakeInjector {
            remote_factory_closure: Box::new(move || {
                let state = state_clone.clone();
                Box::pin(async { Ok(setup_fake_rcs(state)) })
            }),
            ..Default::default()
        };
        let fho_env = FhoEnvironment::new_with_args(
            &ffx_config::EnvironmentContext::no_context(
                ffx_config::environment::ExecutableKind::Test,
                Default::default(),
                None,
                true,
            ),
            &["some", "test"],
        );
        fho_env.set_behavior(FhoConnectionBehavior::DaemonConnector(Arc::new(fake_injector))).await;
        Self { fho_env, state, event_rcv: Some(event_rcv), disconnect_snd: disconnect_snd }
    }

    pub fn take_event_stream(&mut self) -> Option<impl Stream<Item = TestEvent>> {
        self.event_rcv.take()
    }

    pub async fn rcs_connector(&self) -> Connector<RemoteControlProxyHolder> {
        Connector::try_from_env(&self.fho_env).await.expect("Could not make test connector")
    }

    /// Simulates a target reboot.
    pub fn reboot_target(&mut self, new_boot_id: Option<u64>) {
        self.state.mutable.borrow_mut().boot_id = new_boot_id;
        self.disconnect_target();
    }

    pub fn disconnect_target(&mut self) {
        let mut mutable_state = self.state.mutable.borrow_mut();
        // This must have already been taken and is been awaited on.
        assert!(mutable_state.disconnect_rcv.is_none());
        let (snd, rcv) = oneshot::channel();
        let disconnect_snd = std::mem::replace(&mut self.disconnect_snd, snd);
        let _ = disconnect_snd.send(());
        mutable_state.disconnect_rcv = Some(rcv);
    }
}

struct State {
    messages: Vec<LogsData>,
    instances: Vec<Moniker>,
    send_connected_event: bool,
    event_snd: mpsc::UnboundedSender<TestEvent>,
    mutable: RefCell<MutableState>,
}

impl State {
    fn new(
        config: TestEnvironmentConfig,
        snd: mpsc::UnboundedSender<TestEvent>,
        disconnect_rcv: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            messages: config.messages,
            instances: config.instances,
            send_connected_event: config.send_connected_event,
            event_snd: snd,
            mutable: RefCell::new(MutableState {
                boot_timestamp: config.boot_timestamp,
                boot_id: config.boot_id,
                disconnect_rcv: Some(disconnect_rcv),
            }),
        }
    }
}

struct MutableState {
    boot_timestamp: u64,
    boot_id: Option<u64>,
    disconnect_rcv: Option<oneshot::Receiver<()>>,
}

async fn handle_realm_query(
    instances: Vec<fsys::Instance>,
    server_end: fidl::endpoints::ServerEnd<fsys::RealmQueryMarker>,
) {
    let mut stream = server_end.into_stream();
    let mut instance_map = HashMap::new();
    for instance in instances {
        let moniker = Moniker::parse_str(instance.moniker.as_ref().unwrap()).unwrap();
        let previous = instance_map.insert(moniker.to_string(), instance);
        assert!(previous.is_none());
    }

    while let Some(Ok(request)) = stream.next().await {
        match request {
            fsys::RealmQueryRequest::GetInstance { moniker, responder } => {
                let moniker = Moniker::parse_str(&moniker).unwrap().to_string();
                if let Some(instance) = instance_map.get(&moniker) {
                    responder.send(Ok(instance)).unwrap();
                } else {
                    responder.send(Err(fsys::GetInstanceError::InstanceNotFound)).unwrap();
                }
            }
            fsys::RealmQueryRequest::GetAllInstances { responder } => {
                let instances = instance_map.values().cloned().collect();
                let iterator = serve_instance_iterator(instances);
                responder.send(Ok(iterator)).unwrap();
            }
            _ => panic!("Unexpected RealmQuery request"),
        }
    }
}

fn serve_instance_iterator(
    instances: Vec<fsys::Instance>,
) -> fidl::endpoints::ClientEnd<fsys::InstanceIteratorMarker> {
    let (client, mut stream) =
        fidl::endpoints::create_request_stream::<fsys::InstanceIteratorMarker>();
    fasync::Task::local(async move {
        let fsys::InstanceIteratorRequest::Next { responder } =
            stream.next().await.unwrap().unwrap();
        responder.send(&instances).unwrap();
        let Some(Ok(fsys::InstanceIteratorRequest::Next { responder })) = stream.next().await
        else {
            return;
        };
        responder.send(&[]).unwrap();
    })
    .detach();
    client
}

fn setup_fake_rcs(state: Rc<State>) -> RemoteControlProxy {
    let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>();
    fasync::Task::local(async move {
        let mut task_group = fasync::TaskGroup::new();
        while let Ok(Some(req)) = stream.try_next().await {
            match req {
                RemoteControlRequest::EchoString { value, responder } => {
                    responder.send(value.as_ref()).expect("should send");
                }
                RemoteControlRequest::DeprecatedOpenCapability {
                    moniker,
                    capability_set,
                    capability_name,
                    server_channel,
                    flags: _,
                    responder,
                } => {
                    assert_eq!(capability_set, rcs::OpenDirType::NamespaceDir);
                    let state_clone = state.clone();
                    task_group.local(async move {
                        handle_open_capability(
                            &moniker,
                            &capability_name,
                            server_channel,
                            state_clone,
                        )
                        .await
                    });
                    responder.send(Ok(())).unwrap();
                }
                RemoteControlRequest::IdentifyHost { responder } => {
                    responder
                        .send(Ok(&IdentifyHostResponse {
                            nodename: Some(NODENAME.into()),
                            boot_timestamp_nanos: Some(state.mutable.borrow().boot_timestamp),
                            boot_id: state.mutable.borrow().boot_id,
                            ..Default::default()
                        }))
                        .unwrap();
                }
                _ => panic!("unexpected request: {:?}", req),
            }
        }
        task_group.join().await;
    })
    .detach();
    proxy
}

async fn handle_open_capability(
    moniker: &str,
    capability_name: &str,
    channel: fidl::Channel,
    state: Rc<State>,
) {
    let Some(capability_name) = capability_name.strip_prefix("svc/") else {
        panic!("Expected a protocol starting with svc/. Got: {capability_name}");
    };
    match capability_name {
        ArchiveAccessorMarker::PROTOCOL_NAME => {
            assert_eq!(moniker, rcs::toolbox::MONIKER);
            handle_archive_accessor(
                fidl::endpoints::ServerEnd::<ArchiveAccessorMarker>::from(channel).into_stream(),
                state,
            )
            .await;
        }
        LogSettingsMarker::PROTOCOL_NAME => {
            assert_eq!(moniker, rcs::toolbox::MONIKER);
            handle_log_settings(
                fidl::endpoints::ServerEnd::<LogSettingsMarker>::from(channel).into_stream(),
                state,
            )
            .await;
        }
        "fuchsia.sys2.RealmQuery.root" | fsys::RealmQueryMarker::PROTOCOL_NAME => {
            assert_eq!(moniker, "toolbox");
            let server_end = fidl::endpoints::ServerEnd::from(channel);
            handle_realm_query(
                state
                    .instances
                    .iter()
                    .map(|moniker| fsys::Instance {
                        moniker: Some(moniker.to_string()),
                        url: Some("fuchsia-pkg://test".into()),
                        ..Default::default()
                    })
                    .collect(),
                server_end,
            )
            .await;
        }
        other => {
            unreachable!("Attempted to connect to {other:?}");
        }
    }
}

async fn handle_archive_accessor(mut stream: ArchiveAccessorRequestStream, state: Rc<State>) {
    while let Some(Ok(ArchiveAccessorRequest::StreamDiagnostics {
        parameters,
        stream,
        responder,
    })) = stream.next().await
    {
        if state.send_connected_event {
            let _ = state
                .event_snd
                .unbounded_send(TestEvent::Connected(parameters.stream_mode.unwrap()));
        }
        // Ignore the result, because the client may choose to close the channel.
        let _ = responder.send();
        stream.write(serde_json::to_string(&state.messages).unwrap().as_bytes()).unwrap();

        match parameters.stream_mode.unwrap() {
            StreamMode::Snapshot => {}
            StreamMode::SnapshotThenSubscribe | StreamMode::Subscribe => {
                let rcv = state.mutable.borrow_mut().disconnect_rcv.take().unwrap();
                let _ = rcv.await;
            }
        }
    }
}

async fn handle_log_settings(mut stream: LogSettingsRequestStream, state: Rc<State>) {
    while let Some(Ok(request)) = stream.next().await {
        match request {
            LogSettingsRequest::RegisterInterest { .. } => {
                panic!("fuchsia.diagnostics/LogSettings.RegisterInterest is not supported");
            }
            LogSettingsRequest::SetComponentInterest { .. } => {
                panic!("fuchsia.diagnostics/LogSettings.SetComponentInterest is not supported");
            }
            LogSettingsRequest::SetInterest { selectors, responder } => {
                let _ = state.event_snd.unbounded_send(TestEvent::SetInterest(selectors));
                responder.send().unwrap();
            }
        }
    }
    let _ = state.event_snd.unbounded_send(TestEvent::LogSettingsClosed);
}
