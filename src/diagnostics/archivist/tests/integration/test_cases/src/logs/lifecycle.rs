// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{test_topology, utils};
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Data, Logs, RetryConfig};
use fidl_fuchsia_archivist_test::LogPuppetLogRequest;
use fidl_fuchsia_diagnostics::Severity;
use futures::StreamExt;
use {fidl_fuchsia_archivist_test as ftest, fuchsia_async as fasync};

const HELLO_WORLD: &str = "Hello, world!";

#[fuchsia::test]
async fn test_logs_lifecycle() {
    let mut puppets = Vec::with_capacity(12);
    for i in 0..50 {
        puppets.push(test_topology::PuppetDeclBuilder::new(format!("puppet{i}")).into());
    }
    let realm = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(puppets),
        ..Default::default()
    })
    .await
    .expect("create base topology");

    let accessor = utils::connect_accessor(&realm, utils::ALL_PIPELINE).await;
    let mut reader = ArchiveReader::new();
    reader
        .with_archive(accessor)
        .with_minimum_schema_count(0) // we want this to return even when no log messages
        .retry(RetryConfig::never());

    let (mut subscription, mut errors) =
        reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let _log_errors = fasync::Task::spawn(async move {
        if let Some(error) = errors.next().await {
            panic!("{error:#?}");
        }
    });

    reader.retry(RetryConfig::always());
    for i in 0..50 {
        let puppet_name = format!("puppet{i}");
        let puppet = test_topology::connect_to_puppet(&realm, &puppet_name).await.unwrap();
        let request = LogPuppetLogRequest {
            severity: Some(Severity::Info),
            message: Some(HELLO_WORLD.to_string()),
            ..Default::default()
        };
        puppet.log(&request).await.expect("Log succeeds");

        check_message(&puppet_name, subscription.next().await.unwrap());

        reader.with_minimum_schema_count(i);
        let all_messages = reader.snapshot::<Logs>().await.unwrap();

        for message in all_messages {
            check_message("puppet", message);
        }
    }
}

fn check_message(expected_moniker_prefix: &str, message: Data<Logs>) {
    assert!(message.moniker.to_string().starts_with(expected_moniker_prefix));
    assert_data_tree!(message.payload.unwrap(), root: {
        message: {
            value: HELLO_WORLD,
        }
    });
}
