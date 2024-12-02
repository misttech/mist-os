// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_data::{
    DiagnosticsHierarchy, InspectData, InspectDataBuilder, InspectHandleName, Property, Timestamp,
};
use fidl::endpoints::{create_proxy_and_stream, ServerEnd};
use fidl::Channel;
use fidl_fuchsia_developer_remotecontrol::{
    RemoteControlMarker, RemoteControlProxy, RemoteControlRequest,
};
use fidl_fuchsia_diagnostics::{
    ClientSelectorConfiguration, DataType, Format, StreamMode, StreamParameters,
};
use fidl_fuchsia_diagnostics_host::{
    ArchiveAccessorMarker, ArchiveAccessorProxy, ArchiveAccessorRequest,
};
use futures::{AsyncWriteExt, StreamExt, TryStreamExt};
use std::rc::Rc;
use {errors as _, ffx_writer as _, fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync};

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
    responses: Rc<Vec<FakeArchiveIteratorResponse>>,
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
    responses: Rc<Vec<FakeArchiveIteratorResponse>>,
}

impl FakeAccessorData {
    pub fn new(
        parameters: StreamParameters,
        responses: Rc<Vec<FakeArchiveIteratorResponse>>,
    ) -> Self {
        FakeAccessorData { parameters, responses }
    }
}

pub fn setup_fake_archive_accessor(expected_data: Vec<FakeAccessorData>) -> ArchiveAccessorProxy {
    let (proxy, mut stream) = create_proxy_and_stream::<ArchiveAccessorMarker>();
    fuchsia_async::Task::local(async move {
        'req: while let Ok(Some(req)) = stream.try_next().await {
            match req {
                ArchiveAccessorRequest::StreamDiagnostics { parameters, stream, responder } => {
                    for data in expected_data.iter() {
                        if data.parameters == parameters {
                            setup_fake_accessor_provider(
                                fuchsia_async::Socket::from_socket(stream),
                                data.responses.clone(),
                            )
                            .unwrap();
                            responder.send().expect("should send");
                            continue 'req;
                        }
                    }
                    unreachable!(
                        "{:#?} did not match any expected parameters: {:#?}",
                        parameters,
                        expected_data.into_iter().map(|d| d.parameters).collect::<Vec<_>>()
                    );
                }
                ArchiveAccessorRequest::_UnknownMethod { .. } => {
                    unreachable!("We don't expect any other call");
                }
            }
        }
    })
    .detach();
    proxy
}

pub fn make_inspects_for_lifecycle() -> Vec<InspectData> {
    let fake_name = "fake-name";
    vec![
        make_inspect("test/moniker1", 1, 20, fake_name),
        make_inspect("test/moniker1", 2, 30, fake_name),
        make_inspect("test/moniker3", 3, 3, fake_name),
    ]
}

// `components` are component monikers that should report as existing in the resultant RealmQuery.
// This will make them appear in fuzzy searches using RemoteControlProxy/RealmQuery
pub fn setup_fake_rcs(components: Vec<&str>) -> RemoteControlProxy {
    let mut mock_realm_query_builder = iquery_test_support::MockRealmQueryBuilder::prefilled();
    for c in components {
        mock_realm_query_builder =
            mock_realm_query_builder.when(c).moniker(format!("{c}").as_ref()).add();
    }

    let mock_realm_query = mock_realm_query_builder.build();
    let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>();
    fuchsia_async::Task::local(async move {
        let querier = Rc::new(mock_realm_query);
        while let Ok(Some(req)) = stream.try_next().await {
            match req {
                RemoteControlRequest::DeprecatedOpenCapability {
                    moniker,
                    capability_set,
                    capability_name,
                    server_channel,
                    flags: _,
                    responder,
                } => {
                    assert_eq!(moniker, "toolbox");
                    assert_eq!(capability_set, rcs::OpenDirType::NamespaceDir);
                    assert_eq!(capability_name, "svc/fuchsia.sys2.RealmQuery.root");
                    let querier = Rc::clone(&querier);
                    fuchsia_async::Task::local(querier.serve(ServerEnd::new(server_channel)))
                        .detach();
                    responder.send(Ok(())).unwrap();
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
) -> (RemoteControlProxy, fasync::Scope) {
    let mock_realm_query = iquery_test_support::MockRealmQuery::default();
    let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>();
    let scope = fasync::Scope::new();
    let inner_scope = scope.make_ref();
    scope.spawn_local(async move {
        let querier = Rc::new(mock_realm_query);
        while let Ok(Some(req)) = stream.try_next().await {
            match req {
                RemoteControlRequest::DeprecatedOpenCapability {
                    moniker,
                    capability_set,
                    capability_name,
                    server_channel,
                    flags: _,
                    responder,
                } => {
                    if moniker == "toolbox" {
                        assert_eq!(capability_set, rcs::OpenDirType::NamespaceDir);
                        assert_eq!(capability_name, "svc/fuchsia.sys2.RealmQuery.root");
                        let querier = Rc::clone(&querier);
                        inner_scope.spawn_local(querier.serve(ServerEnd::new(server_channel)));
                        responder.send(Ok(())).unwrap();
                    } else if moniker == expected_moniker {
                        assert_eq!(capability_set, fsys::OpenDirType::ExposedDir);
                        assert_eq!(moniker, expected_moniker);
                        assert_eq!(capability_name, expected_protocol);
                        inner_scope.spawn_local(handle_remote_control_connect(
                            server_channel,
                            accessor_proxy.clone(),
                        ));
                        responder.send(Ok(())).unwrap();
                    } else {
                        panic!("Got unexpected moniker: {moniker}");
                    }
                }
                _ => unreachable!("Not implemented"),
            }
        }
    });
    (proxy, scope)
}

async fn handle_remote_control_connect(
    service_chan: Channel,
    accessor_proxy: ArchiveAccessorProxy,
) {
    let server_end = ServerEnd::<ArchiveAccessorMarker>::new(service_chan);
    let mut diagnostics_stream = server_end.into_stream();
    while let Some(Ok(ArchiveAccessorRequest::StreamDiagnostics {
        parameters,
        stream,
        responder,
    })) = diagnostics_stream.next().await
    {
        accessor_proxy.stream_diagnostics(&parameters, stream).await.unwrap();
        responder.send().unwrap();
    }
}

pub fn make_inspect_with_length(moniker: &str, timestamp: i64, len: usize) -> InspectData {
    make_inspect(moniker, timestamp, len, "fake-name")
}

pub fn make_inspect(moniker: &str, timestamp: i64, len: usize, tree_name: &str) -> InspectData {
    let long_string = std::iter::repeat("a").take(len).collect::<String>();
    let hierarchy = DiagnosticsHierarchy::new(
        String::from("name"),
        vec![Property::String(format!("hello_{}", timestamp), long_string)],
        vec![],
    );
    InspectDataBuilder::new(
        moniker.try_into().unwrap(),
        format!("fake-url://{}", moniker),
        Timestamp::from_nanos(timestamp),
    )
    .with_hierarchy(hierarchy)
    .with_name(InspectHandleName::name(tree_name))
    .build()
}

pub fn make_inspects() -> Vec<InspectData> {
    let fake_name = "fake-name";
    vec![
        make_inspect("test/moniker1", 1, 20, fake_name),
        make_inspect("test/moniker2", 2, 10, fake_name),
        make_inspect("test/moniker3", 3, 30, fake_name),
        make_inspect("test/moniker1", 20, 3, fake_name),
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
    let expected_responses = Rc::new(vec![FakeArchiveIteratorResponse::new_with_value(value)]);
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
                    "name":"fuchsia.inspect.Tree",
                    "component_url": "fuchsia-pkg://fuchsia.com/account#meta/account_manager",
                    "timestamp":0
                },
                "moniker":"realm1/realm2/session5/account_manager",
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
                    "name":"fuchsia.inspect.Tree",
                    "component_url": "fuchsia-pkg://fuchsia.com/account#meta/account_manager",
                    "timestamp":0
                },
                "moniker":"realm1/realm2/session5/account_manager",
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
