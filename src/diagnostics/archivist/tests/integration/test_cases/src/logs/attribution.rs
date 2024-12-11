// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{test_topology, utils};
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Logs, Severity};
use fidl_fuchsia_archivist_test::LogPuppetLogRequest;
use futures::StreamExt;
use {fidl_fuchsia_archivist_test as ftest, fidl_fuchsia_diagnostics as fdiagnostics};

// This test verifies that Archivist knows about logging from this component.
#[fuchsia::test]
async fn log_attribution() {
    const REALM_NAME: &str = "child";
    let realm = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new(REALM_NAME).into()]),
        ..Default::default()
    })
    .await
    .expect("create base topology");

    let accessor = utils::connect_accessor(&realm, utils::ALL_PIPELINE).await;
    let mut result = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("snapshot then subscribe");

    let puppet = test_topology::connect_to_puppet(&realm, REALM_NAME).await.unwrap();
    let messages = ["This is a syslog message", "This is another syslog message"];
    for message in messages {
        puppet
            .log(&LogPuppetLogRequest {
                severity: Some(fdiagnostics::Severity::Info),
                message: Some(message.to_string()),
                ..Default::default()
            })
            .await
            .expect("Log succeeds");
    }

    for log_str in &messages {
        let log_record = result.next().await.expect("received log").expect("log is not an error");
        assert_eq!(log_record.moniker.to_string(), REALM_NAME);
        assert_eq!(log_record.metadata.severity, Severity::Info);
        assert_data_tree!(log_record.payload.unwrap(), root: contains {
            message: {
              value: log_str.to_string(),
            }
        });
    }
}
