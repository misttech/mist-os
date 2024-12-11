// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::puppet::PuppetProxyExt;
use crate::{test_topology, utils};
use diagnostics_data::{LogsData, Severity};
use futures::AsyncReadExt;
use {fidl_fuchsia_archivist_test as ftest, fidl_fuchsia_diagnostics as fdiagnostics};

const PUPPET_NAME: &str = "puppet";

#[fuchsia::test]
async fn can_read_using_the_host_accessor() {
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new(PUPPET_NAME).into()]),
        ..Default::default()
    })
    .await
    .expect("create realm");

    let messages = vec![
        (fdiagnostics::Severity::Info, "my msg: 10"),
        (fdiagnostics::Severity::Warn, "my other msg: 20"),
    ];
    let puppet = test_topology::connect_to_puppet(&realm_proxy, PUPPET_NAME).await.unwrap();
    puppet.log_messages(messages.clone()).await;

    let accessor = utils::connect_host_accessor(&realm_proxy, utils::ALL_PIPELINE).await;
    let (local, remote) = zx::Socket::create_stream();
    let mut reader = fuchsia_async::Socket::from_socket(local);
    accessor
        .stream_diagnostics(
            &fdiagnostics::StreamParameters {
                data_type: Some(fdiagnostics::DataType::Logs),
                stream_mode: Some(fdiagnostics::StreamMode::SnapshotThenSubscribe),
                format: Some(fdiagnostics::Format::Json),
                client_selector_configuration: Some(
                    fdiagnostics::ClientSelectorConfiguration::SelectAll(true),
                ),
                ..Default::default()
            },
            remote,
        )
        .await
        .unwrap();

    let mut pending = messages.into_iter().peekable();
    let mut pending_data = vec![];
    while pending.peek().is_some() {
        let mut data = [0; 1024];
        let read = reader.read(&mut data).await.unwrap();
        pending_data.extend_from_slice(&data[..read]);
        if let Ok(logs) = serde_json::from_slice::<Vec<LogsData>>(&pending_data) {
            for log in logs {
                let (severity, msg) = pending.next().unwrap();
                assert_eq!(log.severity(), Severity::from(severity));
                assert_eq!(log.msg().unwrap(), msg);
            }
            pending_data = vec![];
        }
    }
}
