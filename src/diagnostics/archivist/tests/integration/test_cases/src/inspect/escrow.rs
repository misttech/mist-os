// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{test_topology, utils};
use diagnostics_assertions::{assert_data_tree, AnyProperty};
use diagnostics_data::{InspectData, InspectHandleName};
use diagnostics_reader::{ArchiveReader, Inspect, RetryConfig};
use fidl_fuchsia_diagnostics::ArchiveAccessorMarker;
use realm_proxy_client::RealmProxyClient;
use {fidl_fuchsia_archivist_test as ftest, fuchsia_async as fasync, fuchsia_zircon as zx};

const PUPPET_NAME: &str = "puppet";

#[fuchsia::test]
async fn escrow_inspect_data() {
    const REALM_NAME: &str = "escrow_inspect_data";
    let mut stop_checker = utils::StopChecker::new().await;

    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        realm_name: Some(REALM_NAME.into()),
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new(PUPPET_NAME).into()]),
        ..Default::default()
    })
    .await
    .unwrap();

    // Publish some inspect in the puppet.
    let child_puppet = test_topology::connect_to_puppet(&realm_proxy, PUPPET_NAME).await.unwrap();
    let writer = child_puppet
        .create_inspector(&ftest::InspectPuppetCreateInspectorRequest::default())
        .await
        .unwrap()
        .into_proxy()
        .unwrap();

    writer.set_health_ok().await.unwrap();

    // Assert the current live data that the component exposes.
    let data = read_data(&realm_proxy, RetryConfig::always()).await.pop().unwrap();
    assert!(!data.metadata.escrowed);
    assert_data_tree!(data.payload.as_ref().unwrap(), root: {
        "fuchsia.inspect.Health": {
            status: "OK",
            start_timestamp_nanos: AnyProperty,
        }
    });

    // Tell the puppet to escrow the data it's currently exposing.
    let token = writer
        .escrow_and_exit(&ftest::InspectWriterEscrowAndExitRequest {
            name: Some("test-escrow".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    stop_checker
        .wait_for_component_to_stop(&format!(
            "realm_factory/realm_builder:{REALM_NAME}/test/{PUPPET_NAME}"
        ))
        .await;

    // Assert that we can read the escrowed data event after the component has stopped.
    let data = read_data(&realm_proxy, RetryConfig::always()).await.pop().unwrap();
    assert!(data.metadata.escrowed);
    assert_eq!(data.metadata.name, Some(InspectHandleName::Name("test-escrow".into())));
    assert_data_tree!(data.payload.as_ref().unwrap(), root: {
        "fuchsia.inspect.Health": {
            status: "OK",
            start_timestamp_nanos: AnyProperty,
        }
    });

    // Drop token and assert there's no data anymore.
    drop(token);
    loop {
        let data = read_data(&realm_proxy, RetryConfig::never()).await;
        if data.is_empty() {
            break;
        }
        fasync::Timer::new(zx::Time::after(zx::Duration::from_millis(100))).await;
    }
}

async fn read_data(realm_proxy: &RealmProxyClient, retry: RetryConfig) -> Vec<InspectData> {
    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await.unwrap();
    ArchiveReader::new()
        .with_archive(accessor)
        .add_selector(format!("{PUPPET_NAME}:root"))
        .retry(retry)
        .snapshot::<Inspect>()
        .await
        .expect("got inspect data")
}
