// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{test_topology, utils};
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Logs};
use fidl_fuchsia_archivist_test::LogPuppetLogRequest;
use fidl_fuchsia_diagnostics::Severity;
use futures::{FutureExt, StreamExt};
use realm_proxy_client::RealmProxyClient;
use {fidl_fuchsia_archivist_test as ftest, fuchsia_async as fasync};

const HELLO_WORLD: &str = "Hello, world!!!";

#[fuchsia::test]
async fn component_selectors_filter_logs() {
    let mut puppets = Vec::with_capacity(12);
    for i in 0..6 {
        puppets.push(test_topology::PuppetDeclBuilder::new(format!("puppet_a{i}")).into());
        puppets.push(test_topology::PuppetDeclBuilder::new(format!("puppet_b{i}")).into());
    }
    let realm = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(puppets),
        ..Default::default()
    })
    .await
    .expect("create base topology");

    let accessor = utils::connect_accessor(&realm, utils::ALL_PIPELINE).await;

    // Start a few components.
    for i in 0..3 {
        log_and_exit(&realm, format!("puppet_a{i}")).await;
        log_and_exit(&realm, format!("puppet_b{i}")).await;
    }

    // Start listening
    let mut reader = ArchiveReader::new();
    reader.add_selector("puppet_a*:root").with_archive(accessor).with_minimum_schema_count(5);

    let (mut stream, mut errors) =
        reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let _errors = fasync::Task::spawn(async move {
        if let Some(e) = errors.next().await {
            panic!("error in subscription: {e}");
        }
    });

    // Start a few more components
    for i in 3..6 {
        log_and_exit(&realm, format!("puppet_a{i}")).await;
        log_and_exit(&realm, format!("puppet_b{i}")).await;
    }

    // We should see logs from components started before and after we began to listen.
    for _ in 0..6 {
        let log = stream.next().await.unwrap();
        assert!(log.moniker.to_string().starts_with("puppet_a"));
        assert_data_tree!(log.payload.unwrap(), root: {
            message: {
                value: HELLO_WORLD,
            }
        });
    }
    // We only expect 6 logs.
    assert!(stream.next().now_or_never().is_none());
}

async fn log_and_exit(realm: &RealmProxyClient, puppet_name: String) {
    let puppet = test_topology::connect_to_puppet(realm, &puppet_name).await.unwrap();
    let request = LogPuppetLogRequest {
        severity: Some(Severity::Info),
        message: Some(HELLO_WORLD.to_string()),
        ..Default::default()
    };
    puppet.log(&request).await.expect("Log succeeds");
}
