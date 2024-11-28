// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(fuchsia_api_level_at_least = "HEAD")]

use crate::puppet::PuppetProxyExt;
use crate::test_topology;
use diagnostics_log_encoding::parse::parse_record;
use diagnostics_log_encoding::{Argument, Record, Value};
use futures::{future, StreamExt};
use {fidl_fuchsia_archivist_test as ftest, fidl_fuchsia_diagnostics as fdiagnostics};

const PUPPET_NAME: &str = "puppet";

#[fuchsia::test]
async fn listen_with_log_stream() {
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new(PUPPET_NAME).into()]),
        ..Default::default()
    })
    .await
    .expect("create realm");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, PUPPET_NAME).await.unwrap();
    puppet
        .log_messages(vec![
            (fdiagnostics::Severity::Info, "my msg: 10"),
            (fdiagnostics::Severity::Warn, "my other msg: 20"),
        ])
        .await;

    let (socket, s2) = zx::Socket::create_datagram();
    let socket = fuchsia_async::Socket::from_socket(socket);
    let log_stream =
        realm_proxy.connect_to_protocol::<fdiagnostics::LogStreamMarker>().await.unwrap();
    log_stream
        .connect(
            s2,
            &fdiagnostics::LogStreamOptions {
                mode: Some(fdiagnostics::StreamMode::SnapshotThenSubscribe),
                include_moniker: Some(true),
                include_component_url: Some(true),
                ..Default::default()
            },
        )
        .expect("connected socket");

    let mut records = socket
        .into_datagram_stream()
        .filter_map(|result| {
            let bytes = result.unwrap();
            if bytes.is_empty() {
                return future::ready(None);
            }
            let record = match parse_record(&bytes) {
                Ok((record, _)) => record,
                Err(err) => {
                    panic!("Failed to parse record: {bytes:?}: {err:?}")
                }
            };
            future::ready(Some(record.into_owned()))
        })
        .take(2);

    let record = records.next().await.unwrap();
    assert_eq!(record.severity, fdiagnostics::Severity::Info as u8);
    assert_eq!(message(&record), "my msg: 10");
    assert_eq!(moniker(&record), PUPPET_NAME);
    assert_eq!(url(&record), "puppet#meta/puppet.cm");

    let record = records.next().await.unwrap();
    assert_eq!(record.severity, fdiagnostics::Severity::Warn as u8);
    assert_eq!(message(&record), "my other msg: 20");
    assert_eq!(moniker(&record), PUPPET_NAME);
    assert_eq!(url(&record), "puppet#meta/puppet.cm");
}

fn message<'a>(record: &'a Record<'_>) -> &'a str {
    record
        .arguments
        .iter()
        .filter_map(|arg| match arg {
            Argument::Message(m) => Some(m.as_ref()),
            _ => None,
        })
        .next()
        .unwrap()
}

fn moniker<'a>(record: &'a Record<'_>) -> &'a str {
    record
        .arguments
        .iter()
        .filter_map(|arg| match arg {
            Argument::Other { name, value: Value::Text(value) }
                if name == fdiagnostics::MONIKER_ARG_NAME =>
            {
                Some(value.as_ref())
            }
            _ => None,
        })
        .next()
        .unwrap()
}

fn url<'a>(record: &'a Record<'_>) -> &'a str {
    record
        .arguments
        .iter()
        .filter_map(|arg| match arg {
            Argument::Other { name, value: Value::Text(value) }
                if name == fdiagnostics::COMPONENT_URL_ARG_NAME =>
            {
                Some(value.as_ref())
            }
            _ => None,
        })
        .next()
        .unwrap()
}
