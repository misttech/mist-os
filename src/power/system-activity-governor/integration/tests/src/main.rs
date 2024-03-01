// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_assertions::tree_assertion;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_power_broker::{self as fbroker, LeaseStatus};
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fidl_test_systemactivitygovernor as ftest;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon::HandleBased;
use power_broker_client::PowerElementContext;
use realm_proxy_client::RealmProxyClient;

const REALM_FACTORY_CHILD_NAME: &str = "test_realm_factory";

async fn create_realm(options: ftest::RealmOptions) -> Result<(RealmProxyClient, String)> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    let result = realm_factory
        .create_realm(options, server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    Ok((RealmProxyClient::from(client), result))
}

#[fuchsia::test]
async fn test_stats_returns_default_values() -> Result<()> {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let (realm, _) = create_realm(realm_options).await?;

    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);
    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_returns_expected_power_elements() -> Result<()> {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let (realm, _) = create_realm(realm_options).await?;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let es_token = power_elements.execution_state.unwrap().passive_dependency_token.unwrap();
    assert!(!es_token.is_invalid_handle());

    let aa_element = power_elements.application_activity.unwrap();
    let aa_passive_token = aa_element.passive_dependency_token.unwrap();
    assert!(!aa_passive_token.is_invalid_handle());
    let aa_active_token = aa_element.active_dependency_token.unwrap();
    assert!(!aa_active_token.is_invalid_handle());

    Ok(())
}

async fn create_suspend_topology(realm: &RealmProxyClient) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let aa_token = power_elements.application_activity.unwrap().active_dependency_token.unwrap();

    let suspend_controller = PowerElementContext::builder(&topology, "suspend_controller", &[0, 1])
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: 1,
            requires_token: aa_token,
            requires_level: 1,
        }])
        .build()
        .await?;

    Ok(suspend_controller)
}

async fn lease(suspend_controller: &PowerElementContext) -> Result<fbroker::LeaseControlProxy> {
    let lease_control = suspend_controller
        .lessor
        .lease(1)
        .await?
        .map_err(|e| anyhow::anyhow!("{e:?}"))?
        .into_proxy()?;

    let mut lease_status = LeaseStatus::Unknown;
    while lease_status != LeaseStatus::Satisfied {
        lease_status = lease_control.watch_status(lease_status).await.unwrap();
    }

    Ok(lease_control)
}

macro_rules! block_until_inspect_matches {
    ($sag_moniker:expr, $($tree:tt)+) => {{
        let mut reader = ArchiveReader::new();

        reader
            .select_all_for_moniker(&format!("{}/{}", REALM_FACTORY_CHILD_NAME, $sag_moniker))
            .with_minimum_schema_count(1);

        for i in 0.. {
            let Ok(data) = reader
                .snapshot::<Inspect>()
                .await?
                .into_iter()
                .next()
                .and_then(|result| result.payload)
                .ok_or(anyhow::anyhow!("expected one inspect hierarchy")) else {
                continue;
            };

            let tree_assertion = $crate::tree_assertion!($($tree)+);
            match tree_assertion.run(&data) {
                Ok(_) => break,
                Err(error) => {
                    if i == 10 {
                        tracing::warn!(?error, "Still awaiting inspect match after 10 tries");
                    }
                }
            }
        }
    }};
}

#[fuchsia::test]
async fn test_activity_governor_increments_suspend_success_on_lease_drop() -> Result<()> {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let (realm, activity_governor_moniker) = create_realm(realm_options).await?;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    let suspend_controller = create_suspend_topology(&realm).await?;
    let suspend_lease_control = lease(&suspend_controller).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);

    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(0), current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 0u64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Continue incrementing success count on falling edge of Execution State level transitions.
    let suspend_lease_control = lease(&suspend_controller).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 0u64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);

    let current_stats = stats.watch().await?;
    assert_eq!(Some(2), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(0), current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 2u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 0u64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

// TODO(b/306171083): Add more test cases when passive dependencies are supported.
