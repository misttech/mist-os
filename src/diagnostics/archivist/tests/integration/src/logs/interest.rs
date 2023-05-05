// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{constants::*, test_topology};
use diagnostics_data::LogsData;
use diagnostics_reader::{ArchiveReader, Error, Logs, Severity};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_diagnostics::{
    ArchiveAccessorMarker, Interest, LogInterestSelector, LogSettingsMarker,
    Severity as FidlSeverity,
};
use fuchsia_component_test::ScopedInstanceFactory;
use futures::{Stream, StreamExt};
use selectors::{self, VerboseError};

const LOG_AND_EXIT_COMPONENT: &str = "log_and_exit";

#[fuchsia::test]
async fn register_interest() {
    let (builder, test_realm) = test_topology::create(test_topology::Options::default())
        .await
        .expect("create test topology");
    test_topology::add_eager_child(&test_realm, "child", LOGGER_COMPONENT_FOR_INTEREST_URL)
        .await
        .expect("add child");
    let instance = builder.build().await.expect("create instance");

    let accessor = instance
        .root
        .connect_to_protocol_at_exposed_dir::<ArchiveAccessorMarker>()
        .expect("connect to archive accessor");
    let mut logs = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("subscribe to logs");
    let log_settings = instance
        .root
        .connect_to_protocol_at_exposed_dir::<LogSettingsMarker>()
        .expect("connect to log settings");

    let expected_logs = vec![
        (Severity::Debug, "debug msg"),
        (Severity::Info, "info msg"),
        (Severity::Warn, "warn msg"),
        (Severity::Error, "error msg"),
    ];

    let selector = selectors::parse_component_selector::<VerboseError>(&format!(
        "realm_builder\\:{}/test/child",
        instance.root.child_name()
    ))
    .unwrap();

    // 1. Assert logs for default interest registration (info)
    assert_messages(&mut logs, &expected_logs[1..], LOGGER_COMPONENT_FOR_INTEREST_URL).await;

    // 2. Interest registration with min_severity = debug
    let interests = &[LogInterestSelector {
        selector: selector.clone(),
        interest: Interest { min_severity: Some(FidlSeverity::Debug), ..Default::default() },
    }];
    log_settings.register_interest(interests).expect("registered interest");

    // 3. Assert logs
    assert_messages(&mut logs, &expected_logs, LOGGER_COMPONENT_FOR_INTEREST_URL).await;

    // 4. Interest registration with min_severity = warn
    let interests = &[LogInterestSelector {
        selector,
        interest: Interest { min_severity: Some(FidlSeverity::Warn), ..Default::default() },
    }];
    log_settings.register_interest(interests).expect("registered interest");

    // 5. Assert logs
    assert_messages(&mut logs, &expected_logs[2..], LOGGER_COMPONENT_FOR_INTEREST_URL).await;

    // 6. Disconnecting the protocol, brings back an EMPTY interest, which defaults to INFO.
    drop(log_settings);
    assert_messages(&mut logs, &expected_logs[1..], LOGGER_COMPONENT_FOR_INTEREST_URL).await;
}

#[fuchsia::test]
async fn set_interest_before_startup() {
    // Set up topology.
    let (builder, test_realm) = test_topology::create(test_topology::Options::default())
        .await
        .expect("create test topology");
    test_topology::add_collection(&test_realm, "coll").await.unwrap();
    test_topology::expose_test_realm_protocol(&builder, &test_realm).await;
    let instance = builder.build().await.unwrap();

    // Set the coll:* minimum severity to DEBUG.
    let log_settings = instance
        .root
        .connect_to_protocol_at_exposed_dir::<LogSettingsMarker>()
        .expect("connect to log settings");
    let selector = selectors::parse_component_selector::<VerboseError>(&format!(
        "realm_builder\\:{}/test/**",
        instance.root.child_name()
    ))
    .unwrap();
    let interests = &[LogInterestSelector {
        selector,
        interest: Interest { min_severity: Some(FidlSeverity::Debug), ..Default::default() },
    }];
    log_settings.set_interest(interests).await.expect("set interest");

    // Start listening for logs
    let accessor = instance
        .root
        .connect_to_protocol_at_exposed_dir::<ArchiveAccessorMarker>()
        .expect("connect to archive accessor");
    let mut logs = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("subscribe to logs");

    // Create the component under test.
    let realm_proxy =
        instance.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();
    let child_instance = ScopedInstanceFactory::new("coll")
        .with_realm_proxy(realm_proxy)
        .new_named_instance(LOG_AND_EXIT_COMPONENT, LOG_AND_EXIT_COMPONENT_URL)
        .await
        .unwrap();
    let _ =
        child_instance.connect_to_protocol_at_exposed_dir::<fcomponent::BinderMarker>().unwrap();

    // Assert logs include the DEBUG log.
    assert_messages(
        &mut logs,
        &[(Severity::Debug, "debugging world"), (Severity::Info, "Hello, world!")],
        LOG_AND_EXIT_COMPONENT_URL,
    )
    .await;
}

async fn assert_messages<S>(mut logs: S, messages: &[(Severity, &str)], url: &str)
where
    S: Stream<Item = Result<LogsData, Error>> + std::marker::Unpin,
{
    for (expected_severity, expected_msg) in messages {
        let log = logs.next().await.expect("got log response").expect("log isn't an error");
        assert_eq!(log.metadata.component_url, Some(url.to_string()));
        assert_eq!(&log.msg().unwrap(), expected_msg);
        assert_eq!(log.metadata.severity, *expected_severity);
    }
}
