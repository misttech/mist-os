// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_topology;
use anyhow::{Context, Error};
use diagnostics_data::LogsData;
use diagnostics_reader::{ArchiveReader, Logs};
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_diagnostics::{
    ArchiveAccessorMarker, ComponentSelector, Interest, LogInterestSelector, LogSettingsMarker,
    LogSettingsProxy, Severity,
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

    let component_log_settings = ComponentLogSettings::new(PUPPET_NAME);

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
    component_log_settings.set_interest(&log_settings, Severity::Debug).await.unwrap();
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
    component_log_settings.set_interest(&log_settings, Severity::Warn).await.unwrap();
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
    component_log_settings.set_interest(&log_settings, Severity::Error).await.unwrap();
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

    let log_settings = realm_proxy
        .connect_to_protocol::<LogSettingsMarker>()
        .await
        .expect("connect to log settings");

    // Set the minimum severity to Severity::Debug.
    let component_log_settings = ComponentLogSettings::new("**");
    component_log_settings.set_interest(&log_settings, Severity::Debug).await.unwrap();

    // Start listening for logs.
    let accessor = realm_proxy
        .connect_to_protocol::<ArchiveAccessorMarker>()
        .await
        .expect("connect to archive accessor");

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

    let mut logs = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("subscribe to logs");

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
    S: Stream<Item = Result<LogsData, diagnostics_reader::Error>> + std::marker::Unpin,
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

// A helper struct that modifies log settings for a set of components matched by a given
// component_selector.
struct ComponentLogSettings {
    component_selector: ComponentSelector,
}

impl ComponentLogSettings {
    fn new(component_selector_str: &'static str) -> Self {
        let component_selector = parse_component_selector::<VerboseError>(component_selector_str)
            .expect("is valid component selector");
        Self { component_selector }
    }
}

impl ComponentLogSettings {
    async fn set_interest(
        &self,
        log_settings: &LogSettingsProxy,
        severity: Severity,
    ) -> Result<(), Error> {
        let interests = [LogInterestSelector {
            selector: self.component_selector.clone(),
            interest: Interest { min_severity: Some(severity), ..Default::default() },
        }];
        log_settings.set_interest(&interests).await.context("set interest")?;
        Ok(())
    }
}
