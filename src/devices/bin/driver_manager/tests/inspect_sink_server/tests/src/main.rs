// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_assertions::assert_json_diff;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::{create_endpoints, Proxy};
use fidl_fuchsia_inspect::InspectSinkMarker;
use fidl_test_drivermanager as ftest;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_inspect::Inspector;
use inspect_runtime::{publish, PublishOptions};
use realm_proxy_client::RealmProxyClient;

async fn create_realm() -> Result<RealmProxyClient> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    realm_factory.create_realm(server).await?.map_err(realm_proxy_client::Error::OperationError)?;
    Ok(RealmProxyClient::from(client))
}

#[fuchsia::test]
async fn test_driver_default_name_for_connections() -> Result<()> {
    let realm = create_realm().await?;

    let inspect_sink = realm.connect_to_protocol::<InspectSinkMarker>().await?;

    let inspector = Inspector::default();
    inspector.root().record_string("inspect_sink_server_test", "value");

    let _server = publish(
        &inspector,
        // not specifying a name, so should be "driver-unknown"
        PublishOptions::default().on_inspect_sink_client(inspect_sink.into_client_end().unwrap()),
    );

    let selector = "test_realm_factory/realm_builder*/driver_test_realm/realm_builder*/driver_manager:root:inspect_sink_server_test";
    let results = ArchiveReader::new().add_selector(selector).snapshot::<Inspect>().await?;

    let results =
        results.into_iter().find(|v| v.metadata.name.as_ref() == "driver-unknown").unwrap();

    assert_json_diff!(results.payload.unwrap(), root: {
        inspect_sink_server_test: "value",
    });

    Ok(())
}

#[fuchsia::test]
async fn test_with_name_specified() -> Result<()> {
    let realm = create_realm().await?;

    let inspect_sink = realm.connect_to_protocol::<InspectSinkMarker>().await?;

    let inspector = Inspector::default();
    inspector.root().record_string("inspect_sink_server_test2", "value2");

    let _server = publish(
        &inspector,
        PublishOptions::default()
            .inspect_tree_name("tree-name")
            .on_inspect_sink_client(inspect_sink.into_client_end().unwrap()),
    );
    let selector = "test_realm_factory/realm_builder*/driver_test_realm/realm_builder*/driver_manager:root:inspect_sink_server_test2";
    let results = ArchiveReader::new().add_selector(selector).snapshot::<Inspect>().await?;

    let results = results.into_iter().find(|v| v.metadata.name.as_ref() == "tree-name").unwrap();

    assert_json_diff!(results.payload.unwrap(), root: {
        inspect_sink_server_test2: "value2",
    });

    Ok(())
}
