// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_topology;
use diagnostics_data::LogsData;
use diagnostics_reader::{ArchiveReader, Error, Logs};
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_diagnostics::{
    ArchiveAccessorMarker, Interest, LogInterestSelector, LogSettingsMarker, Severity,
};
use futures::{Stream, StreamExt};
use selectors::{self, parse_component_selector, VerboseError};

// This test verifies that a component only emits messages at or above its
// current interest severity level, even when the interest changes while the
// component is running.
#[fuchsia::test]
async fn set_interest() {
    const PUPPET_NAME: &str = "puppet";

    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![ftest::PuppetDecl { name: PUPPET_NAME.to_string() }]),
        ..Default::default()
    })
    .await
    .expect("create test topology");

    let accessor = realm_proxy
        .connect_to_protocol::<ArchiveAccessorMarker>()
        .await
        .expect("connect to archive accessor");

    let mut logs = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("subscribe to logs");

    let log_settings = realm_proxy
        .connect_to_protocol::<LogSettingsMarker>()
        .await
        .expect("connect to log settings");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, PUPPET_NAME)
        .await
        .expect("connect to puppet");

    let selector = parse_component_selector::<VerboseError>(PUPPET_NAME).unwrap();

    // Helper function to generate a new LogInterestSelector from severity.
    let interest_selectors = |severity: Severity| {
        [LogInterestSelector {
            selector: selector.clone(),
            interest: Interest { min_severity: Some(severity), ..Default::default() },
        }]
    };

    // Use default severity INFO.
    // Wait for the initial interest to be observed.
    let mut response = puppet.wait_for_interest_change().await.unwrap();
    assert_eq!(response.severity, Some(Severity::Info));

    // Log one info message before the first debug message to confirm the debug
    // message isn't skipped because of a race condition.
    puppet.log_messages(vec![
        (Severity::Info, "A1"),
        (Severity::Debug, "B1"), // not observed.
        (Severity::Info, "C1"),
        (Severity::Warn, "D1"),
        (Severity::Error, "E1"),
    ]);

    assert_ordered_logs(
        &mut logs,
        PUPPET_NAME,
        vec![
            (Severity::Info, "A1"),
            (Severity::Info, "C1"),
            (Severity::Warn, "D1"),
            (Severity::Error, "E1"),
        ],
    )
    .await;

    // Severity: DEBUG
    let mut interest = interest_selectors(Severity::Debug);
    log_settings.set_interest(&interest).await.expect("registered interest");
    response = puppet.wait_for_interest_change().await.unwrap();
    assert_eq!(response.severity, Some(Severity::Debug));
    puppet.log_messages(vec![
        (Severity::Debug, "A2"),
        (Severity::Info, "B2"),
        (Severity::Warn, "C2"),
        (Severity::Error, "D2"),
    ]);

    assert_ordered_logs(
        &mut logs,
        PUPPET_NAME,
        vec![
            (Severity::Debug, "A2"),
            (Severity::Info, "B2"),
            (Severity::Warn, "C2"),
            (Severity::Error, "D2"),
        ],
    )
    .await;

    // Severity: WARN
    interest = interest_selectors(Severity::Warn);
    log_settings.set_interest(&interest).await.expect("registered interest");
    response = puppet.wait_for_interest_change().await.unwrap();
    assert_eq!(response.severity, Some(Severity::Warn));
    puppet.log_messages(vec![
        (Severity::Debug, "A3"), // Not observed.
        (Severity::Info, "B3"),  // Not observed.
        (Severity::Warn, "C3"),
        (Severity::Error, "D3"),
    ]);

    assert_ordered_logs(
        &mut logs,
        PUPPET_NAME,
        vec![(Severity::Warn, "C3"), (Severity::Error, "D3")],
    )
    .await;

    // Severity: ERROR
    interest = interest_selectors(Severity::Error);
    log_settings.set_interest(&interest).await.expect("registered interest");
    response = puppet.wait_for_interest_change().await.unwrap();
    assert_eq!(response.severity, Some(Severity::Error));
    puppet.log_messages(vec![
        (Severity::Debug, "A4"), // Not observed.
        (Severity::Info, "B4"),  // Not observed.
        (Severity::Warn, "C4"),  // Not observed.
        (Severity::Error, "D4"),
    ]);

    assert_ordered_logs(&mut logs, PUPPET_NAME, vec![(Severity::Error, "D4")]).await;

    // Disconnecting the protocol, brings back an EMPTY interest, which defaults to Severity::Info.
    drop(log_settings);
    response = puppet.wait_for_interest_change().await.unwrap();
    assert_eq!(response.severity, Some(Severity::Info));

    // Again, log one info message before the first debug message to confirm the
    // debug message isn't skipped because of a race condition.
    puppet.log_messages(vec![
        (Severity::Debug, "A5"), // Not observed.
        (Severity::Info, "B5"),
        (Severity::Info, "C5"),
        (Severity::Warn, "D5"),
        (Severity::Error, "E5"),
    ]);

    assert_ordered_logs(
        &mut logs,
        PUPPET_NAME,
        vec![
            (Severity::Info, "B5"),
            (Severity::Info, "C5"),
            (Severity::Warn, "D5"),
            (Severity::Error, "E5"),
        ],
    )
    .await;
}

// This test verifies that a component only emits messages at or above its
// current interest severity level, where the interest is inherited from the
// parent realm, having been configured before the component was launched.
#[fuchsia::test]
async fn set_interest_before_startup() {
    const PUPPET_NAME: &str = "puppet";

    // Create the test realm.
    // We won't connect to the puppet until after we've configured logging interest.
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![ftest::PuppetDecl { name: PUPPET_NAME.to_string() }]),
        ..Default::default()
    })
    .await
    .expect("create test topology");

    // Helper function to generate a new LogInterestSelector from severity.
    let selector = parse_component_selector::<VerboseError>("**").unwrap();
    let interest_selectors = |severity: Severity| {
        [LogInterestSelector {
            selector: selector.clone(),
            interest: Interest { min_severity: Some(severity), ..Default::default() },
        }]
    };

    // Set the coll:* minimum severity to Severity::Debug.
    let interests = interest_selectors(Severity::Debug);
    let log_settings = realm_proxy
        .connect_to_protocol::<LogSettingsMarker>()
        .await
        .expect("connect to log settings");
    log_settings.set_interest(&interests).await.expect("set interest");

    // Start listening for logs.
    let accessor = realm_proxy
        .connect_to_protocol::<ArchiveAccessorMarker>()
        .await
        .expect("connect to archive accessor");

    let mut logs = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("subscribe to logs");

    // Connect to the component under test to start it.
    let puppet = test_topology::connect_to_puppet(&realm_proxy, PUPPET_NAME)
        .await
        .expect("connect to puppet");

    let response = puppet.wait_for_interest_change().await.unwrap();
    assert_eq!(response.severity, Some(Severity::Debug));
    puppet.log_messages(vec![
        (Severity::Debug, "debugging world"),
        (Severity::Info, "Hello, world!"),
    ]);

    // Assert logs include the Severity::Debug log.
    assert_ordered_logs(
        &mut logs,
        PUPPET_NAME,
        vec![(Severity::Debug, "debugging world"), (Severity::Info, "Hello, world!")],
    )
    .await;
}

type Message = (Severity, &'static str);

async fn assert_ordered_logs<'a, S>(mut logs: S, component_name: &str, messages: Vec<Message>)
where
    S: Stream<Item = Result<LogsData, Error>> + std::marker::Unpin,
{
    for (expected_severity, expected_msg) in messages {
        let log = logs.next().await.expect("got log response").expect("log isn't an error");
        let log_component_url = log.metadata.component_url.clone().unwrap();
        assert!(log_component_url.ends_with(component_name));
        assert_eq!(log.msg().unwrap(), expected_msg);
        assert_eq!(log.metadata.severity, expected_severity);
    }
}

trait PuppetProxyExt {
    fn log_messages(&self, messages: Vec<Message>);
}

impl PuppetProxyExt for ftest::PuppetProxy {
    fn log_messages(&self, messages: Vec<Message>) {
        for (severity, message) in messages {
            let request = &ftest::LogPuppetLogRequest {
                severity: Some(severity),
                message: Some(message.to_string()),
                ..Default::default()
            };
            self.log(request).expect("log succeeds");
        }
    }
}
