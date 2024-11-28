// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::puppet::PuppetProxyExt;
use crate::{test_topology, utils};
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_diagnostics::Severity;
use futures::StreamExt;
use tracing::warn;

#[fuchsia::test]
async fn logs_from_crashing_component() -> Result<(), anyhow::Error> {
    const REALM_NAME: &str = "logs_from_crashing_component";
    const PUPPET_NAME: &str = "puppet";
    const PUPPET_CRASH_MESSAGE: &str = "this is an expected panic";
    const LOG_MESSAGE: &str = "logged before crashing";

    // Create the test realm.
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        realm_name: Some(REALM_NAME.to_string()),
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new(PUPPET_NAME).into()]),
        ..Default::default()
    })
    .await
    .expect("create test topology");

    let stop_watcher = realm_proxy
        .connect_to_protocol::<ftest::StopWatcherMarker>()
        .await
        .expect("connect to stop watcher");
    let stop_waiter = stop_watcher
        .watch_component(PUPPET_NAME, ftest::ExitStatus::Crash)
        .await
        .expect("subscribe to component crash")
        .into_proxy();

    // Connect to the puppet, tell it to log some messages and then crash itself.
    let puppet = test_topology::connect_to_puppet(&realm_proxy, PUPPET_NAME)
        .await
        .expect("connect to puppet");

    puppet.wait_for_interest_change().await.unwrap(); // Wait for initial interest.
    puppet.log_messages(vec![(Severity::Info, LOG_MESSAGE)]).await;

    // Ensure the puppet logged before crashing.
    // The crash's stacktrace and panic message appear in the log output, so check for the
    // subsequence of expected logs rather than a contiguous list of matching lines.
    let mut log_buf: Vec<String> = vec![];
    let mut found_log_message = false;
    let mut found_crash_message = false;

    let mut logs = utils::snapshot_and_stream_logs(&realm_proxy).await;

    // Check for the log message.
    while let Some(Ok(logs_data)) = logs.next().await {
        log_buf.push(logs_data.msg().unwrap().to_string().clone());
        if logs_data.moniker.to_string() == PUPPET_NAME
            && logs_data.msg().unwrap() == LOG_MESSAGE
            && logs_data.metadata.severity == Severity::Info
        {
            found_log_message = true;
            break;
        }
    }

    // Only print the logs on failure to avoid spam.
    if !found_log_message {
        dump_logs_and_fail(log_buf, format!("Did not find log '{LOG_MESSAGE}'"));
        return Ok(());
    }

    puppet.crash(PUPPET_CRASH_MESSAGE)?;
    stop_waiter.wait().await.expect("puppet crashes");
    drop(realm_proxy); // Closes the puppet's log stream so we don't loop forever.

    // Check for the panic message.
    while let Some(Ok(logs_data)) = logs.next().await {
        log_buf.push(logs_data.msg().unwrap().to_string().clone());
        let payload = logs_data.payload.clone().unwrap();
        if logs_data.moniker.to_string() == PUPPET_NAME
            && logs_data.msg().unwrap() == "PANIC"
            && payload
                .get_property_by_path(&["keys", "info"])
                .unwrap()
                .string()
                .unwrap()
                .contains(PUPPET_CRASH_MESSAGE)
        {
            found_crash_message = true;
            break;
        }
    }

    // Only print the logs on failure to avoid spam.
    if !found_crash_message {
        dump_logs_and_fail(log_buf, format!("Did not find log '{PUPPET_CRASH_MESSAGE}'"));
    }

    Ok(())
}

fn dump_logs_and_fail(logs: Vec<String>, message: String) {
    warn!("Failing this test. It received these logs:");
    for (line, message) in logs.iter().enumerate() {
        warn!("{line}: {message}");
    }
    panic!("ERROR: {message}");
}
