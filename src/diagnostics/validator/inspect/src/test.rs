// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_diagnostics_validate::{
    TestFailure, TestSuccess, ValidateResult, ValidateResultsIteratorGetNextResponse,
    ValidateResultsIteratorProxy, ValidatorMarker,
};
use fuchsia_component::client;
use tracing::info;

const PUPPET_MONIKER: &str = "./puppet";

async fn shutdown_puppet() {
    let lifecycle_controller =
        client::connect_to_protocol::<fidl_fuchsia_sys2::LifecycleControllerMarker>().unwrap();
    lifecycle_controller.stop_instance(PUPPET_MONIKER).await.unwrap().unwrap();
}

async fn connect_to_validator() -> ValidateResultsIteratorProxy {
    let validator = client::connect_to_protocol::<ValidatorMarker>().expect("connect to validator");
    let (result_iterator, server_end) = fidl::endpoints::create_proxy().unwrap();
    validator.validate(server_end).expect("call validate");
    result_iterator
}

#[fuchsia::test]
async fn validate_inspect() {
    let result_iterator = connect_to_validator().await;

    let mut failed_results = vec![];
    let mut tests_executed = 0;
    while let Ok(response) = result_iterator.get_next().await {
        shutdown_puppet().await;
        let ValidateResultsIteratorGetNextResponse { result, .. } = response;
        let Some(result) = result else {
            break;
        };
        tests_executed += 1;
        match result {
            ValidateResult::Success(TestSuccess { test_name }) => {
                info!("Test {test_name} passed.");
            }
            ValidateResult::Failure(TestFailure { test_name, reason }) => {
                failed_results.push(format!("Test {test_name} failed: {reason}"));
            }
        }
    }

    assert!(tests_executed > 0);
    info!("Ran: {tests_executed} tests.");
    if !failed_results.is_empty() {
        panic!("{}", failed_results.join("\n"));
    }
}
