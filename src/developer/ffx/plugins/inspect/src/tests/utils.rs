// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_data::{Data, DiagnosticsHierarchy, InspectData, InspectHandleName, Property};
use errors as _;
use ffx_writer as _;
use fidl::{
    endpoints::{create_proxy_and_stream, ServerEnd},
    Channel,
};
use fidl_fuchsia_developer_remotecontrol::{
    RemoteControlConnectCapabilityResponder, RemoteControlMarker, RemoteControlProxy,
    RemoteControlRequest,
};
use fidl_fuchsia_diagnostics::{
    ClientSelectorConfiguration, DataType, Format, StreamMode, StreamParameters,
};
use fidl_fuchsia_diagnostics_host::{
    ArchiveAccessorMarker, ArchiveAccessorProxy, ArchiveAccessorRequest,
};
use futures::{AsyncWriteExt, StreamExt, TryStreamExt};
use iquery_test_support;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct FakeArchiveIteratorResponse {
    value: String,
}

impl FakeArchiveIteratorResponse {
    pub fn new_with_value(value: String) -> Self {
        FakeArchiveIteratorResponse { value, ..Default::default() }
    }
}

pub fn setup_fake_accessor_provider(
    mut server_end: fuchsia_async::Socket,
    responses: Arc<Vec<FakeArchiveIteratorResponse>>,
) -> Result<()> {
    fuchsia_async::Task::local(async move {
        if responses.is_empty() {
            return;
        }
        assert_eq!(responses.len(), 1);
        server_end.write_all(responses[0].value.as_bytes()).await.unwrap();
    })
    .detach();
    Ok(())
}

pub struct FakeAccessorData {
    parameters: StreamParameters,
    responses: Arc<Vec<FakeArchiveIteratorResponse>>,
}

impl FakeAccessorData {
    pub fn new(
        parameters: StreamParameters,
        responses: Arc<Vec<FakeArchiveIteratorResponse>>,
    ) -> Self {
        FakeAccessorData { parameters, responses }
    }
}

pub fn setup_fake_archive_accessor(expected_data: Vec<FakeAccessorData>) -> ArchiveAccessorProxy {
    let (proxy, mut stream) = create_proxy_and_stream::<ArchiveAccessorMarker>().unwrap();
    fuchsia_async::Task::local(async move {
        'req: while let Ok(Some(req)) = stream.try_next().await {
            match req {
                ArchiveAccessorRequest::StreamDiagnostics { parameters, stream, responder } => {
                    for data in expected_data.iter() {
                        if data.parameters == parameters {
                            setup_fake_accessor_provider(
                                fuchsia_async::Socket::from_socket(stream).unwrap(),
                                data.responses.clone(),
                            )
                            .unwrap();
                            responder.send().expect("should send");
                            continue 'req;
                        }
                    }
                    assert!(false, "{:?} did not match any expected parameters", parameters);
                }
            }
        }
    })
    .detach();
    proxy
}

pub fn make_inspects_for_lifecycle() -> Vec<InspectData> {
    let fake_filename = "fake-filename";
    vec![
        make_inspect(String::from("test/moniker1"), 1, 20, fake_filename),
        make_inspect(String::from("test/moniker1"), 2, 30, fake_filename),
        make_inspect(String::from("test/moniker3"), 3, 3, fake_filename),
    ]
}

pub fn setup_fake_rcs() -> RemoteControlProxy {
    let mock_realm_query = iquery_test_support::MockRealmQuery::default();
    let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<<fidl_fuchsia_developer_remotecontrol::RemoteControlProxy as fidl::endpoints::Proxy>::Protocol>().unwrap();
    fuchsia_async::Task::local(async move {
        let querier = Arc::new(mock_realm_query);
        while let Ok(Some(req)) = stream.try_next().await {
            match req {
                RemoteControlRequest::RootRealmQuery { server, responder } => {
                    let querier = Arc::clone(&querier);
                    fuchsia_async::Task::local(querier.serve(server)).detach();
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => unreachable!("Not implemented"),
            }
        }
    })
    .detach();
    proxy
}

pub fn setup_fake_rcs_with_embedded_archive_accessor(
    accessor_proxy: ArchiveAccessorProxy,
    expected_moniker: String,
    expected_protocol: String,
) -> (RemoteControlProxy, Arc<Mutex<Vec<fuchsia_async::Task<()>>>>) {
    let mock_realm_query = iquery_test_support::MockRealmQuery::default();
    let (proxy, mut stream) =
        fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>().unwrap();
    let running_tasks = Arc::new(Mutex::new(vec![]));
    let running_tasks_clone = running_tasks.clone();
    let task = fuchsia_async::Task::local(async move {
        let querier = Arc::new(mock_realm_query);
        if let Ok(Some(req)) = stream.try_next().await {
            match req {
                RemoteControlRequest::RootRealmQuery { server, responder } => {
                    let querier = Arc::clone(&querier);
                    fuchsia_async::Task::local(async move { querier.serve(server).await }).detach();
                    responder.send(&mut Ok(())).unwrap();
                }
                RemoteControlRequest::ConnectCapability {
                    moniker,
                    capability_name,
                    server_chan,
                    flags: _,
                    responder,
                } => {
                    assert_eq!(moniker, expected_moniker);
                    assert_eq!(capability_name, expected_protocol);
                    let task =
                        handle_remote_control_connect(responder, server_chan, accessor_proxy);
                    let mut tasks = running_tasks_clone.lock().unwrap();
                    tasks.push(task);
                }
                _ => unreachable!("Not implemented"),
            }
        }
    });
    {
        let mut tasks = running_tasks.lock().unwrap();
        tasks.push(task);
    }
    (proxy, running_tasks)
}

fn handle_remote_control_connect(
    responder: RemoteControlConnectCapabilityResponder,
    service_chan: Channel,
    accessor_proxy: ArchiveAccessorProxy,
) -> fuchsia_async::Task<()> {
    responder.send(&mut Ok(())).unwrap();
    fuchsia_async::Task::local(async move {
        let server_end = ServerEnd::<ArchiveAccessorMarker>::new(service_chan);
        let mut diagnostics_stream = server_end.into_stream().unwrap();
        while let Some(Ok(ArchiveAccessorRequest::StreamDiagnostics {
            parameters,
            stream,
            responder,
        })) = diagnostics_stream.next().await
        {
            accessor_proxy.stream_diagnostics(parameters, stream).await.unwrap();
            responder.send().unwrap();
        }
    })
}

pub fn make_inspect_with_length(moniker: String, timestamp: i64, len: usize) -> InspectData {
    make_inspect(moniker, timestamp, len, "fake-filename")
}

pub fn make_inspect(moniker: String, timestamp: i64, len: usize, file_name: &str) -> InspectData {
    let long_string = std::iter::repeat("a").take(len).collect::<String>();
    let hierarchy = DiagnosticsHierarchy::new(
        String::from("name"),
        vec![Property::String(format!("hello_{}", timestamp), long_string)],
        vec![],
    );
    Data::for_inspect(
        moniker.clone(),
        Some(hierarchy),
        timestamp,
        format!("fake-url://{}", moniker),
        Some(InspectHandleName::filename(file_name)),
        vec![],
    )
}

pub fn make_inspects() -> Vec<InspectData> {
    let fake_filename = "fake-filename";
    vec![
        make_inspect(String::from("test/moniker1"), 1, 20, fake_filename),
        make_inspect(String::from("test/moniker2"), 2, 10, fake_filename),
        make_inspect(String::from("test/moniker3"), 3, 30, fake_filename),
        make_inspect(String::from("test/moniker1"), 20, 3, fake_filename),
    ]
}

pub fn inspect_accessor_data(
    client_selector_configuration: ClientSelectorConfiguration,
    inspects: Vec<InspectData>,
) -> FakeAccessorData {
    let params = fidl_fuchsia_diagnostics::StreamParameters {
        stream_mode: Some(StreamMode::Snapshot),
        data_type: Some(DataType::Inspect),
        format: Some(Format::Json),
        client_selector_configuration: Some(client_selector_configuration),
        ..Default::default()
    };
    let value = serde_json::to_string(&inspects).unwrap();
    let expected_responses = Arc::new(vec![FakeArchiveIteratorResponse::new_with_value(value)]);
    FakeAccessorData::new(params, expected_responses)
}

pub fn get_empty_value_json() -> serde_json::Value {
    serde_json::json!([])
}

pub fn get_v1_json_dump() -> serde_json::Value {
    serde_json::json!(
        [
            {
                "data_source":"Inspect",
                "metadata":{
                    "filename":"fuchsia.inspect.Tree",
                    "component_url": "fuchsia-pkg://fuchsia.com/account#meta/account_manager.cmx",
                    "timestamp":0
                },
                "moniker":"realm1/realm2/session5/account_manager.cmx",
                "payload":{
                    "root": {
                        "accounts": {
                            "active": 0,
                            "total": 0
                        },
                        "auth_providers": {
                            "types": "google"
                        },
                        "listeners": {
                            "active": 1,
                            "events": 0,
                            "total_opened": 1
                        }
                    }
                },
                "version":1
            }
        ]
    )
}

pub fn get_v1_single_value_json() -> serde_json::Value {
    serde_json::json!(
        [
            {
                "data_source":"Inspect",
                "metadata":{
                    "filename":"fuchsia.inspect.Tree",
                    "component_url": "fuchsia-pkg://fuchsia.com/account#meta/account_manager.cmx",
                    "timestamp":0
                },
                "moniker":"realm1/realm2/session5/account_manager.cmx",
                "payload":{
                    "root": {
                        "accounts": {
                            "active": 0
                        }
                    }
                },
                "version":1
            }
        ]
    )
}
