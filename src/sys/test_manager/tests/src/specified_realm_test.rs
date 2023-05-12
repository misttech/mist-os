// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Error},
    fidl::endpoints::ClientEnd,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_test_manager as ftest_manager,
    ftest_manager::{CaseStatus, RunOptions, SuiteStatus},
    fuchsia_async as fasync,
    fuchsia_component::client,
    futures::{prelude::*, stream},
    pretty_assertions::assert_eq,
    test_diagnostics::collect_string_from_socket,
    test_manager_test_lib::{
        collect_suite_events, default_run_option, GroupRunEventByTestCase, RunEvent, TestBuilder,
        TestRunEventPayload,
    },
};

const ECHO_TEST_COL: &str = "echo_test_coll";
const HERMETIC_TEST_COL: &str = "hermetic_test_coll";

macro_rules! connect_run_builder {
    () => {
        client::connect_to_protocol::<ftest_manager::RunBuilderMarker>()
            .context("cannot connect to run builder proxy")
    };
}

fn connect_realm() -> Result<ClientEnd<fcomponent::RealmMarker>, Error> {
    let (client_end, server_end) = fidl::endpoints::create_endpoints::<fcomponent::RealmMarker>();
    client::connect_channel_to_protocol::<fcomponent::RealmMarker>(server_end.into_channel())
        .context("could not connect to Realm service")?;
    Ok(client_end)
}

fn default_event_offers() -> Vec<fdecl::Offer> {
    vec![
        fdecl::Offer::EventStream(fdecl::OfferEventStream {
            target_name: Some("capability_requested".to_string()),
            ..Default::default()
        }),
        fdecl::Offer::EventStream(fdecl::OfferEventStream {
            target_name: Some("directory_ready".to_string()),
            ..Default::default()
        }),
    ]
}

async fn run_test_in_echo_test_realm(
    test_url: &str,
    run_options: RunOptions,
) -> Result<(Vec<RunEvent>, Vec<String>), Error> {
    let realm = connect_realm().unwrap();
    let mut offers = default_event_offers();
    offers.push(fdecl::Offer::Protocol(fdecl::OfferProtocol {
        source_name: Some("fidl.examples.routing.echo.Echo".into()),
        target_name: Some("fidl.examples.routing.echo.Echo".into()),
        source: None,
        target: None,
        dependency_type: None,
        ..Default::default()
    }));
    run_single_test(realm, &offers, ECHO_TEST_COL, test_url, run_options).await
}

async fn run_test_in_hermetic_test_realm(
    test_url: &str,
    run_options: RunOptions,
) -> Result<(Vec<RunEvent>, Vec<String>), Error> {
    let realm = connect_realm().unwrap();
    let offers = default_event_offers();
    run_single_test(realm, &offers, HERMETIC_TEST_COL, test_url, run_options).await
}

async fn run_single_test(
    realm: ClientEnd<fcomponent::RealmMarker>,
    offers: &[fdecl::Offer],
    test_collection: &str,
    test_url: &str,
    run_options: RunOptions,
) -> Result<(Vec<RunEvent>, Vec<String>), Error> {
    let builder = TestBuilder::new(connect_run_builder!()?);
    let suite_instance = builder
        .add_suite_in_realm(realm, offers, test_collection, test_url, run_options)
        .await
        .context("Cannot create suite instance")?;
    let builder_run = fasync::Task::spawn(async move { builder.run().await });
    let ret = collect_suite_events(suite_instance).await;
    builder_run.await.context("builder execution failed")?;
    ret
}

#[fuchsia::test]
async fn launch_and_test_echo_test() {
    let test_url =
        "fuchsia-pkg://fuchsia.com/test_manager_specified_realm_test#meta/echo_test_client.cm";
    let (events, logs) = run_test_in_echo_test_realm(test_url, default_run_option()).await.unwrap();

    let expected_events = vec![
        RunEvent::suite_started(),
        RunEvent::case_found("EchoTest"),
        RunEvent::case_started("EchoTest"),
        RunEvent::case_stopped("EchoTest", CaseStatus::Passed),
        RunEvent::case_finished("EchoTest"),
        RunEvent::suite_stopped(SuiteStatus::Passed),
    ];

    assert_eq!(logs, Vec::<String>::new());
    assert_eq!(&expected_events, &events);
}

#[fuchsia::test]
async fn launch_and_test_echo_test_in_hermetic_realm() {
    let test_url =
        "fuchsia-pkg://fuchsia.com/test_manager_specified_realm_test#meta/echo_test_client.cm";
    let (events, _logs) =
        run_test_in_hermetic_test_realm(test_url, default_run_option()).await.unwrap();

    // this will fail because the hermetic realm does not have access to echo service.
    let expected_events = vec![
        RunEvent::suite_started(),
        RunEvent::case_found("EchoTest"),
        RunEvent::case_started("EchoTest"),
        RunEvent::case_stopped("EchoTest", CaseStatus::Failed),
        RunEvent::case_finished("EchoTest"),
        RunEvent::suite_stopped(SuiteStatus::Failed),
    ];

    //assert_eq!(logs, Vec::<String>::new());
    assert_eq!(&expected_events, &events);
}

#[fuchsia::test]
async fn launch_and_test_hermetic_echo_test_in_hermetic_realm() {
    // This test does not depend on system echo service so should pass in hermetic realm.
    let test_url =
        "fuchsia-pkg://fuchsia.com/test_manager_specified_realm_test#meta/echo_test_realm.cm";
    let (events, logs) =
        run_test_in_hermetic_test_realm(test_url, default_run_option()).await.unwrap();

    let expected_events = vec![
        RunEvent::suite_started(),
        RunEvent::case_found("EchoTest"),
        RunEvent::case_started("EchoTest"),
        RunEvent::case_stopped("EchoTest", CaseStatus::Passed),
        RunEvent::case_finished("EchoTest"),
        RunEvent::suite_stopped(SuiteStatus::Passed),
    ];

    assert_eq!(logs, Vec::<String>::new());
    assert_eq!(&expected_events, &events);
}

#[fuchsia::test]
async fn collect_isolated_logs_using_default_log_iterator() {
    let test_url = "fuchsia-pkg://fuchsia.com/test-manager-diagnostics-tests#meta/test-root.cm";
    let (_events, logs) =
        run_test_in_hermetic_test_realm(test_url, default_run_option()).await.unwrap();

    assert_eq!(
        logs,
        vec!["Started diagnostics publisher".to_owned(), "Finishing through Stop".to_owned()]
    );
}

#[fuchsia::test]
async fn collect_isolated_logs_using_batch() {
    let test_url = "fuchsia-pkg://fuchsia.com/test-manager-diagnostics-tests#meta/test-root.cm";
    let mut options = default_run_option();
    options.log_iterator = Some(ftest_manager::LogsIteratorOption::BatchIterator);
    let (_events, logs) = run_test_in_hermetic_test_realm(test_url, options).await.unwrap();

    assert_eq!(
        logs,
        vec!["Started diagnostics publisher".to_owned(), "Finishing through Stop".to_owned()]
    );
}

#[fuchsia::test]
async fn collect_isolated_logs_using_archive_iterator() {
    let test_url = "fuchsia-pkg://fuchsia.com/test-manager-diagnostics-tests#meta/test-root.cm";
    let options = RunOptions {
        log_iterator: Some(ftest_manager::LogsIteratorOption::ArchiveIterator),
        ..default_run_option()
    };
    let (_events, logs) = run_test_in_hermetic_test_realm(test_url, options).await.unwrap();

    assert_eq!(
        logs,
        vec!["Started diagnostics publisher".to_owned(), "Finishing through Stop".to_owned()]
    );
}

#[fuchsia::test]
async fn update_log_severity_for_all_components() {
    let test_url = "fuchsia-pkg://fuchsia.com/test-manager-diagnostics-tests#meta/test-root.cm";
    let options = RunOptions {
        log_iterator: Some(ftest_manager::LogsIteratorOption::ArchiveIterator),
        log_interest: Some(vec![
            selectors::parse_log_interest_selector_or_severity("DEBUG").unwrap()
        ]),
        ..default_run_option()
    };
    let (_events, logs) = run_test_in_hermetic_test_realm(test_url, options).await.unwrap();
    assert_eq!(
        logs,
        vec![
            "I'm a debug log from a test".to_owned(),
            "Started diagnostics publisher".to_owned(),
            "I'm a debug log from the publisher!".to_owned(),
            "Finishing through Stop".to_owned(),
        ]
    );
}

#[fuchsia::test]
async fn debug_data_test() {
    let test_url =
        "fuchsia-pkg://fuchsia.com/test_manager_specified_realm_test#meta/debug_data_write_test.cm";

    let builder = TestBuilder::new(connect_run_builder!().unwrap());
    let realm = connect_realm().unwrap();
    let suite_instance = builder
        .add_suite_in_realm(
            realm,
            &default_event_offers(),
            HERMETIC_TEST_COL,
            test_url,
            default_run_option(),
        )
        .await
        .expect("Cannot create suite instance");
    let (run_events_result, suite_events_result) =
        futures::future::join(builder.run(), collect_suite_events(suite_instance)).await;

    let suite_events = suite_events_result.unwrap().0;
    let expected_events = vec![
        RunEvent::suite_started(),
        RunEvent::case_found("publish_debug_data"),
        RunEvent::case_started("publish_debug_data"),
        RunEvent::case_stopped("publish_debug_data", CaseStatus::Passed),
        RunEvent::case_finished("publish_debug_data"),
        RunEvent::suite_stopped(SuiteStatus::Passed),
    ];

    assert_eq!(
        suite_events.into_iter().group_by_test_case_unordered(),
        expected_events.into_iter().group_by_test_case_unordered(),
    );

    let num_debug_data_events = stream::iter(run_events_result.unwrap())
        .then(|run_event| async move {
            let TestRunEventPayload::DebugData { socket, .. } = run_event.payload;
            let content = collect_string_from_socket(socket).await.unwrap();
            content == "Debug data from test\n"
        })
        .filter(|matches_vmo| futures::future::ready(*matches_vmo))
        .count()
        .await;
    assert_eq!(num_debug_data_events, 1);
}

#[fuchsia::test]
async fn debug_data_accumulate_test() {
    let test_url =
        "fuchsia-pkg://fuchsia.com/test_manager_specified_realm_test#meta/debug_data_write_test.cm";

    // If I run the same test again, also accumulating debug_data, I should see two files
    for iteration in 1usize..3 {
        let builder = TestBuilder::new(connect_run_builder!().unwrap());
        builder.set_scheduling_options(true).expect("set scheduling options");
        let realm = connect_realm().unwrap();
        let suite_instance = builder
            .add_suite_in_realm(
                realm,
                &default_event_offers(),
                HERMETIC_TEST_COL,
                test_url,
                default_run_option(),
            )
            .await
            .expect("Cannot create suite instance");
        let (run_events_result, _) =
            futures::future::join(builder.run(), collect_suite_events(suite_instance)).await;

        let num_debug_data_events = stream::iter(run_events_result.unwrap())
            .then(|run_event| async move {
                let TestRunEventPayload::DebugData { socket, .. } = run_event.payload;
                let content = collect_string_from_socket(socket).await.unwrap();
                content == "Debug data from test\n"
            })
            .filter(|matches_vmo| futures::future::ready(*matches_vmo))
            .count()
            .await;
        assert_eq!(num_debug_data_events, iteration);
    }
}

#[fuchsia::test]
async fn debug_data_isolated_test() {
    let test_url =
        "fuchsia-pkg://fuchsia.com/test_manager_specified_realm_test#meta/debug_data_write_test.cm";
    // By default, when I run the same test twice, debug data is not accumulated.
    for _ in 0..2 {
        let builder = TestBuilder::new(connect_run_builder!().unwrap());
        let realm = connect_realm().unwrap();
        let suite_instance = builder
            .add_suite_in_realm(
                realm,
                &default_event_offers(),
                HERMETIC_TEST_COL,
                test_url,
                default_run_option(),
            )
            .await
            .expect("Cannot create suite instance");
        let (run_events_result, _) =
            futures::future::join(builder.run(), collect_suite_events(suite_instance)).await;

        let num_debug_data_events = stream::iter(run_events_result.unwrap())
            .then(|run_event| async move {
                let TestRunEventPayload::DebugData { socket, .. } = run_event.payload;
                let content = collect_string_from_socket(socket).await.unwrap();
                content == "Debug data from test\n"
            })
            .filter(|matches_vmo| futures::future::ready(*matches_vmo))
            .count()
            .await;
        assert_eq!(num_debug_data_events, 1);
    }
}
