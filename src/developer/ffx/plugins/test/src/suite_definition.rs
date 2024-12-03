// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use fidl_fuchsia_diagnostics::{ComponentSelector, Interest, LogInterestSelector, StringSelector};
use fidl_fuchsia_sys2 as fsys;
use run_test_suite_lib::TestParams;
use std::io::Read;
use test_list::{ExecutionEntry, FuchsiaComponentExecutionEntry, TestList, TestListEntry};

#[derive(Default)]
pub struct TestParamsOptions {
    // If set, filter out all tests that cannot be executed by this binary.
    // If not set, such tests result in an error.
    pub ignore_test_without_known_execution: bool,
}

pub async fn test_params_from_reader<R: Read>(
    reader: R,
    lifecycle_controller: &fsys::LifecycleControllerProxy,
    realm_query: &fsys::RealmQueryProxy,
    options: TestParamsOptions,
) -> Result<Vec<TestParams>> {
    let test_list: TestList = serde_json::from_reader(reader).map_err(anyhow::Error::from)?;
    let TestList::Experimental { data } = test_list;
    let mut result = vec![];
    for entry in data {
        let TestListEntry { tags, execution, name, .. } = entry;
        match execution {
            Some(ExecutionEntry::FuchsiaComponent(component_execution)) => {
                let FuchsiaComponentExecutionEntry {
                    component_url,
                    test_args,
                    timeout_seconds,
                    test_filters,
                    also_run_disabled_tests,
                    parallel,
                    max_severity_logs,
                    min_severity_logs,
                    realm,
                    create_no_exception_channel,
                } = component_execution;
                let mut provided_realm = None;
                if let Some(realm_str) = &realm {
                    provided_realm = Some(
                        run_test_suite_lib::parse_provided_realm(
                            &lifecycle_controller,
                            &realm_query,
                            &realm_str,
                        )
                        .await
                        .map_err(|e| {
                            errors::ffx_error!("Error parsing realm '{}': {}", realm_str, e)
                        })?,
                    );
                }
                let mut min_severity_selectors = vec![];
                if let Some(min_severity) = min_severity_logs {
                    min_severity_selectors.push(LogInterestSelector {
                        selector: ComponentSelector {
                            moniker_segments: Some(vec![StringSelector::StringPattern(
                                "**".into(),
                            )]),
                            ..Default::default()
                        },
                        interest: Interest {
                            min_severity: Some(min_severity.into()),
                            ..Default::default()
                        },
                    });
                }
                result.push(TestParams {
                    test_url: component_url,
                    realm: provided_realm.into(),
                    test_args,
                    timeout_seconds,
                    test_filters,
                    also_run_disabled_tests,
                    parallel,
                    max_severity_logs,
                    min_severity_logs: min_severity_selectors,
                    tags,
                    break_on_failure: false,
                    no_exception_channel: create_no_exception_channel,
                });
            }
            _ => {
                if !options.ignore_test_without_known_execution {
                    return Err(format_err!(
            "Cannot execute {name}, only \"fuchsia_component\" test execution is supported."));
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::create_proxy;

    const VALID_JSON: &'static str = r#"
    {
        "schema_id": "experimental",
        "data": [
            {
                "name": "test",
                "labels": [],
                "tags": [],
                "execution": {
                    "type": "fuchsia_component",
                    "component_url": "fuchsia.com"
                }
            }
        ]
    }
    "#;

    const CONTAINS_VALID_AND_INVALID: &'static str = r#"
    {
        "schema_id": "experimental",
        "data": [
            {
                "name": "test",
                "labels": [],
                "tags": [],
                "execution": {
                    "type": "fuchsia_component",
                    "component_url": "fuchsia.com"
                }
            },
            {
                "name": "test3",
                "labels": [],
                "tags": []
            }
        ]
    }
    "#;

    #[fuchsia::test]
    async fn test_params_from_reader_valid() {
        let reader = VALID_JSON.as_bytes();
        let (lifecycle_controller, _server_end1) =
            create_proxy::<fsys::LifecycleControllerMarker>();
        let (realm_query, _server_end2) = create_proxy::<fsys::RealmQueryMarker>();
        let test_params = test_params_from_reader(
            reader,
            &lifecycle_controller,
            &realm_query,
            TestParamsOptions { ignore_test_without_known_execution: false },
        )
        .await
        .expect("read file");
        assert_eq!(1, test_params.len());
    }

    #[fuchsia::test]
    async fn test_params_from_reader_invalid() {
        let reader = CONTAINS_VALID_AND_INVALID.as_bytes();
        let (lifecycle_controller, _server_end1) =
            create_proxy::<fsys::LifecycleControllerMarker>();
        let (realm_query, _server_end2) = create_proxy::<fsys::RealmQueryMarker>();
        let test_params = test_params_from_reader(
            reader,
            &lifecycle_controller,
            &realm_query,
            TestParamsOptions { ignore_test_without_known_execution: false },
        )
        .await;
        assert!(test_params.is_err());
    }

    #[fuchsia::test]
    async fn test_params_from_reader_invalid_skipped() {
        let reader = CONTAINS_VALID_AND_INVALID.as_bytes();
        let (lifecycle_controller, _server_end1) =
            create_proxy::<fsys::LifecycleControllerMarker>();
        let (realm_query, _server_end2) = create_proxy::<fsys::RealmQueryMarker>();
        let test_params = test_params_from_reader(
            reader,
            &lifecycle_controller,
            &realm_query,
            TestParamsOptions { ignore_test_without_known_execution: true },
        )
        .await
        .expect("should have ignored errors");
        assert_eq!(1, test_params.len());
    }
}
