// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_topology;
use diagnostics_data::Logs;
use diagnostics_reader::{ArchiveReader, RetryConfig};
use futures::StreamExt;
use std::collections::VecDeque;
use {fidl_fuchsia_archivist_test as ftest, fidl_fuchsia_diagnostics as fdiagnostics};

const SPAM_COUNT: usize = 1001;

#[fuchsia::test]
async fn test_budget() {
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(vec![
            test_topology::PuppetDeclBuilder::new("spammer").into(),
            test_topology::PuppetDeclBuilder::new("victim").into(),
        ]),
        archivist_config: Some(ftest::ArchivistConfig {
            logs_max_cached_original_bytes: Some(98304),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .unwrap();

    let spammer_puppet = test_topology::connect_to_puppet(&realm_proxy, "spammer").await.unwrap();
    let victim_puppet = test_topology::connect_to_puppet(&realm_proxy, "victim").await.unwrap();
    spammer_puppet.wait_for_interest_change().await.unwrap();
    victim_puppet.wait_for_interest_change().await.unwrap();

    let letters = ('A'..='Z').map(|c| c.to_string()).collect::<Vec<_>>();
    let mut letters_iter = letters.iter().cycle();
    let expected = letters_iter.next().unwrap().repeat(50);
    victim_puppet
        .log(&ftest::LogPuppetLogRequest {
            severity: Some(fdiagnostics::Severity::Info),
            message: Some(expected.clone()),
            ..Default::default()
        })
        .await
        .expect("emitted log");

    let accessor =
        realm_proxy.connect_to_protocol::<fdiagnostics::ArchiveAccessorMarker>().await.unwrap();
    let mut log_reader = ArchiveReader::new();
    log_reader
        .with_archive(accessor)
        .with_minimum_schema_count(0) // we want this to return even when no log messages
        .retry(RetryConfig::never());

    let (mut observed_logs, _errors) =
        log_reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let (mut observed_logs_2, _errors) =
        log_reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();

    let msg_a = observed_logs.next().await.unwrap();
    let msg_a_2 = observed_logs_2.next().await.unwrap();
    assert_eq!(expected, msg_a.msg().unwrap());
    assert_eq!(expected, msg_a_2.msg().unwrap());

    // Spam many logs.
    let mut expected = VecDeque::new();
    for i in 0..SPAM_COUNT {
        let message = letters_iter.next().unwrap().repeat(50);
        spammer_puppet
            .log(&ftest::LogPuppetLogRequest {
                severity: Some(fdiagnostics::Severity::Info),
                message: Some(message.clone()),
                ..Default::default()
            })
            .await
            .expect("emitted log");
        expected.push_back(message);

        // Each message is about 136 bytes.  We always keep 32 KiB free in the buffer, so we can
        // hold nearly 500 messages before messages get rolled out.  Archivist delays processing
        // sockets so that it's not constantly waking up, so we process the observer in batches.
        if i % 400 == 0 {
            while let Some(message) = expected.pop_front() {
                assert_eq!(message, observed_logs.next().await.unwrap().msg().unwrap());
            }
        }
    }

    while let Some(message) = expected.pop_front() {
        assert_eq!(message, observed_logs.next().await.unwrap().msg().unwrap());
    }

    // We observe some logs were rolled out.
    while observed_logs_2.next().await.unwrap().rolled_out_logs().is_none() {}

    let mut observed_logs = log_reader.snapshot::<Logs>().await.unwrap().into_iter();
    let msg_b = observed_logs.next().unwrap();
    assert!(!msg_b.moniker.to_string().contains("puppet-victim"));

    // Victim logs should have been rolled out.
    let messages = observed_logs
        .filter(|log| log.moniker.to_string().contains("puppet-victim"))
        .collect::<Vec<_>>();
    assert!(messages.is_empty());
    assert_ne!(msg_a.msg().unwrap(), msg_b.msg().unwrap());
}
