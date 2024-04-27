// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_assertions::tree_assertion;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_hardware_suspend as fhsuspend;
use fidl_fuchsia_power_broker::{self as fbroker, LeaseStatus};
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fidl_test_suspendcontrol as tsc;
use fidl_test_systemactivitygovernor as ftest;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon::{self as zx, HandleBased};
use futures::{channel::mpsc, StreamExt};
use power_broker_client::PowerElementContext;
use realm_proxy_client::RealmProxyClient;
use std::time::Instant;

const REALM_FACTORY_CHILD_NAME: &str = "test_realm_factory";

async fn set_up_default_suspender(device: &tsc::DeviceProxy) {
    device
        .set_suspend_states(&tsc::DeviceSetSuspendStatesRequest {
            suspend_states: Some(vec![fhsuspend::SuspendState {
                resume_latency: Some(0),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap()
}

async fn create_realm() -> Result<(RealmProxyClient, String, tsc::DeviceProxy)> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    let (result, suspend_control_client_end) = realm_factory
        .create_realm(server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    Ok((RealmProxyClient::from(client), result, suspend_control_client_end.into_proxy().unwrap()))
}

#[fuchsia::test]
async fn test_stats_returns_default_values() -> Result<()> {
    let (realm, _, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;

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
    let (realm, _, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let es_token = power_elements.execution_state.unwrap().passive_dependency_token.unwrap();
    assert!(!es_token.is_invalid_handle());

    let aa_element = power_elements.application_activity.unwrap();
    let aa_active_token = aa_element.active_dependency_token.unwrap();
    assert!(!aa_active_token.is_invalid_handle());

    let wh_element = power_elements.wake_handling.unwrap();
    let wh_active_token = wh_element.active_dependency_token.unwrap();
    assert!(!wh_active_token.is_invalid_handle());

    let fwh_element = power_elements.full_wake_handling.unwrap();
    let fwh_active_token = fwh_element.active_dependency_token.unwrap();
    assert!(!fwh_active_token.is_invalid_handle());

    let erl_element = power_elements.execution_resume_latency.unwrap();
    let erl_passive_token = erl_element.passive_dependency_token.unwrap();
    assert!(!erl_passive_token.is_invalid_handle());
    let erl_active_token = erl_element.active_dependency_token.unwrap();
    assert!(!erl_active_token.is_invalid_handle());

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

async fn create_wake_topology(realm: &RealmProxyClient) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let wh_token = power_elements.wake_handling.unwrap().active_dependency_token.unwrap();

    let wake_controller = PowerElementContext::builder(&topology, "wake_controller", &[0, 1])
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: 1,
            requires_token: wh_token,
            requires_level: 1,
        }])
        .build()
        .await?;

    Ok(wake_controller)
}

async fn create_full_wake_topology(realm: &RealmProxyClient) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let fwh_token = power_elements.full_wake_handling.unwrap().active_dependency_token.unwrap();

    let full_wake_controller =
        PowerElementContext::builder(&topology, "full_wake_controller", &[0, 1])
            .dependencies(vec![fbroker::LevelDependency {
                dependency_type: fbroker::DependencyType::Active,
                dependent_level: 1,
                requires_token: fwh_token,
                requires_level: 1,
            }])
            .build()
            .await?;

    Ok(full_wake_controller)
}

async fn create_latency_topology(
    realm: &RealmProxyClient,
    expected_latencies: &Vec<i64>,
) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let erl = power_elements.execution_resume_latency.unwrap();
    let erl_token = erl.active_dependency_token.unwrap();
    assert_eq!(*expected_latencies, erl.resume_latencies.unwrap());

    let levels = Vec::from_iter(0..expected_latencies.len().try_into().unwrap());

    let erl_controller = PowerElementContext::builder(&topology, "erl_controller", &levels)
        .dependencies(
            levels
                .iter()
                .map(|level| fbroker::LevelDependency {
                    dependency_type: fbroker::DependencyType::Active,
                    dependent_level: *level,
                    requires_token: erl_token.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                    requires_level: *level,
                })
                .collect(),
        )
        .build()
        .await?;

    Ok(erl_controller)
}

async fn lease(controller: &PowerElementContext, level: u8) -> Result<fbroker::LeaseControlProxy> {
    let lease_control = controller
        .lessor
        .lease(level)
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
async fn test_activity_governor_increments_suspend_success_on_application_activity_lease_drop(
) -> Result<()> {
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    let suspend_controller = create_suspend_topology(&realm).await?;
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
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

    drop(suspend_lease_control);
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(2), current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Continue incrementing success count on falling edge of Execution State level transitions.
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    let current_stats = stats.watch().await?;
    assert_eq!(Some(2), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(2), current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 2u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_raises_execution_state_power_level_on_wake_handling_claim(
) -> Result<()> {
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    {
        // Trigger "boot complete" logic.
        let suspend_controller = create_suspend_topology(&realm).await?;
        lease(&suspend_controller, 1).await?;
    }
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    let suspend_device2 = suspend_device.clone();

    let wake_controller = create_wake_topology(&realm).await?;

    fasync::Task::local(async move {
        suspend_device2
            .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
                suspend_duration: Some(2i64),
                suspend_overhead: Some(1i64),
                ..Default::default()
            }))
            .await
            .unwrap()
            .unwrap();
    })
    .detach();

    let wake_handling_lease_control = lease(&wake_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 1u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 1u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(wake_handling_lease_control);
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 2u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_raises_execution_state_power_level_on_full_wake_handling_claim(
) -> Result<()> {
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    {
        // Trigger "boot complete" logic.
        let suspend_controller = create_suspend_topology(&realm).await?;
        lease(&suspend_controller, 1).await?;
    }
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    let suspend_device2 = suspend_device.clone();

    let wake_controller = create_full_wake_topology(&realm).await?;

    fasync::Task::local(async move {
        suspend_device2
            .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
                suspend_duration: Some(2i64),
                suspend_overhead: Some(1i64),
                ..Default::default()
            }))
            .await
            .unwrap()
            .unwrap();
    })
    .detach();

    let wake_handling_lease_control = lease(&wake_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 1u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 1u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(wake_handling_lease_control);
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 2u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_shows_resume_latency_in_inspect() -> Result<()> {
    let suspend_states = vec![
        fhsuspend::SuspendState { resume_latency: Some(1000), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(100), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(10), ..Default::default() },
    ];
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    suspend_device
        .set_suspend_states(&tsc::DeviceSetSuspendStatesRequest {
            suspend_states: Some(suspend_states),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    let expected_latencies = vec![1000i64, 100i64, 10i64];
    let erl_controller = create_latency_topology(&realm, &expected_latencies).await?;

    for i in 0..expected_latencies.len() {
        let _erl_lease_control = lease(&erl_controller, i.try_into().unwrap()).await?;

        block_until_inspect_matches!(
            activity_governor_moniker,
            root: {
                booting: true,
                power_elements: {
                    execution_state: {
                        power_level: 2u64,
                    },
                    application_activity: {
                        power_level: 0u64,
                    },
                    full_wake_handling: {
                        power_level: 0u64,
                    },
                    wake_handling: {
                        power_level: 0u64,
                    },
                    execution_resume_latency: {
                        power_level: i as u64,
                        resume_latency: expected_latencies[i] as u64,
                        resume_latencies: expected_latencies.clone(),
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
    }

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_forwards_resume_latency_to_suspender() -> Result<()> {
    let suspend_states = vec![
        fhsuspend::SuspendState { resume_latency: Some(430), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(320), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(21), ..Default::default() },
    ];
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    suspend_device
        .set_suspend_states(&tsc::DeviceSetSuspendStatesRequest {
            suspend_states: Some(suspend_states),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    let suspend_controller = create_suspend_topology(&realm).await?;
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    let expected_latencies = vec![430i64, 320i64, 21i64];
    let expected_index = 1;
    let erl_controller = create_latency_topology(&realm, &expected_latencies).await?;
    let _erl_lease_control = lease(&erl_controller, expected_index).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
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
    assert_eq!(
        expected_index as u64,
        suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap()
    );
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(1000i64),
            suspend_overhead: Some(50i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 1000u64,
                last_time_in_suspend_operations: 50u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_increments_fail_count_on_suspend_error() -> Result<()> {
    let suspend_states = vec![
        fhsuspend::SuspendState { resume_latency: Some(430), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(320), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(21), ..Default::default() },
    ];
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    suspend_device
        .set_suspend_states(&tsc::DeviceSetSuspendStatesRequest {
            suspend_states: Some(suspend_states),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    let suspend_controller = create_suspend_topology(&realm).await?;
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    let expected_latencies = vec![430i64, 320i64, 21i64];
    let expected_index = 1;
    let erl_controller = create_latency_topology(&realm, &expected_latencies).await?;
    let _erl_lease_control = lease(&erl_controller, expected_index).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
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
    assert_eq!(
        expected_index as u64,
        suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap()
    );
    suspend_device.resume(&tsc::DeviceResumeRequest::Error(7)).await.unwrap().unwrap();

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 1u64,
                last_failed_error: 7u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_suspends_after_listener_hanging_on_resume() -> Result<()> {
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;

    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    let (listener_client_end, mut listener_stream) =
        fidl::endpoints::create_request_stream().unwrap();
    activity_governor
        .register_listener(fsystem::ActivityGovernorRegisterListenerRequest {
            listener: Some(listener_client_end),
            ..Default::default()
        })
        .await
        .unwrap();

    let (on_suspend_tx, mut on_suspend_rx) = mpsc::channel(1);
    let (on_resume_tx, mut on_resume_rx) = mpsc::channel(1);

    fasync::Task::local(async move {
        let mut on_suspend_tx = on_suspend_tx;
        let mut on_resume_tx = on_resume_tx;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    fasync::Timer::new(fasync::Duration::from_millis(50)).await;
                    responder.send().unwrap();
                    fasync::Timer::new(fasync::Duration::from_millis(50)).await;
                    on_resume_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspend { .. } => {
                    on_suspend_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    {
        let suspend_controller = create_suspend_topology(&realm).await?;
        // Cycle execution_state power level to trigger a suspend/resume cycle.
        lease(&suspend_controller, 1).await?;
    }

    // OnSuspend and OnResume should have been called once.
    on_suspend_rx.next().await.unwrap();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    on_resume_rx.next().await.unwrap();

    // Should only have been 1 suspend after all listener handling.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(2), current_stats.last_time_in_suspend);

    // Await SAG's power elements to drop their power levels.
    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_handles_listener_raising_power_levels() -> Result<()> {
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;

    let suspend_controller = create_suspend_topology(&realm).await?;
    // Trigger "boot complete" logic.
    let suspend_lease = lease(&suspend_controller, 1).await.unwrap();

    let (listener_client_end, mut listener_stream) =
        fidl::endpoints::create_request_stream().unwrap();
    activity_governor
        .register_listener(fsystem::ActivityGovernorRegisterListenerRequest {
            listener: Some(listener_client_end),
            ..Default::default()
        })
        .await
        .unwrap();

    let (on_suspend_tx, mut on_suspend_rx) = mpsc::channel(1);
    let (on_resume_tx, mut on_resume_rx) = mpsc::channel(1);

    fasync::Task::local(async move {
        let mut on_suspend_tx = on_suspend_tx;
        let mut on_resume_tx = on_resume_tx;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    let suspend_lease = lease(&suspend_controller, 1).await.unwrap();
                    on_resume_tx.try_send(suspend_lease).unwrap();
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspend { .. } => {
                    on_suspend_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    drop(suspend_lease);

    // OnSuspend should have been called.
    on_suspend_rx.next().await.unwrap();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    // At this point, the listener should have raised the execution_state power level to 2.
    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Drop the lease and wait for suspend,
    on_resume_rx.next().await.unwrap();

    // OnSuspend should be called again.
    on_suspend_rx.next().await.unwrap();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    // At this point, the listener should have raised the execution_state power level to 2 again.
    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 2u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_blocks_lease_while_suspend_in_progress() -> Result<()> {
    let (realm, _, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;

    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    let suspend_controller = create_suspend_topology(&realm).await?;
    lease(&suspend_controller, 1).await.unwrap();

    let (listener_client_end, mut listener_stream) =
        fidl::endpoints::create_request_stream().unwrap();
    activity_governor
        .register_listener(fsystem::ActivityGovernorRegisterListenerRequest {
            listener: Some(listener_client_end),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());

    let now = Instant::now();

    let (mut on_resume_tx, mut on_resume_rx) = mpsc::channel(1);
    let (mut on_lease_tx, mut on_lease_rx) = mpsc::channel(1);
    fasync::Task::local(async move {
        {
            // Try to take a lease before handling listener requests.
            lease(&suspend_controller, 1).await.unwrap();
            on_lease_tx.try_send(now.elapsed()).unwrap();
        }

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    on_resume_tx.start_send(lease(&suspend_controller, 1).await.unwrap()).unwrap();
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspend { .. } => {}
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    fasync::Task::local(async move {
        // Wait some time before resuming.
        fasync::Timer::new(fasync::Duration::from_millis(100)).await;
        let resume_time = now.elapsed();
        suspend_device
            .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
                suspend_duration: Some(49i64),
                suspend_overhead: Some(51i64),
                ..Default::default()
            }))
            .await
            .unwrap()
            .unwrap();
        assert!(resume_time < on_lease_rx.next().await.unwrap(), "Lease was granted too early");
    })
    .detach();

    // Wait for OnResume to be called.
    on_resume_rx.next().await.unwrap();

    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(49), current_stats.last_time_in_suspend);
    assert_eq!(Some(51), current_stats.last_time_in_suspend_operations);

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_handles_boot_signal() -> Result<()> {
    let (realm, activity_governor_moniker, suspend_device) = create_realm().await?;
    set_up_default_suspender(&suspend_device).await;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    // Initial state should show execution_state is active and booting is true.
    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: true,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
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

    // Trigger "boot complete" signal.
    let suspend_controller = create_suspend_topology(&realm).await?;
    lease(&suspend_controller, 1).await.unwrap();

    // Now execution_state should have dropped and booting is false.
    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
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

    Ok(())
}

#[fuchsia::test]
async fn test_element_info_provider() -> Result<()> {
    let (realm, _, suspend_device) = create_realm().await?;

    let suspend_states = vec![
        fhsuspend::SuspendState { resume_latency: Some(1000), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(100), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(10), ..Default::default() },
    ];

    suspend_device
        .set_suspend_states(&tsc::DeviceSetSuspendStatesRequest {
            suspend_states: Some(suspend_states),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    let full_wake_controller = create_full_wake_topology(&realm).await?;
    let wake_controller = create_wake_topology(&realm).await?;
    let suspend_controller = create_suspend_topology(&realm).await?;
    let expected_latencies = vec![1000i64, 100i64, 10i64];
    let erl_controller = create_latency_topology(&realm, &expected_latencies).await?;

    let element_info_provider = realm
        .connect_to_service_instance::<fbroker::ElementInfoProviderServiceMarker>(
            &"system_activity_governor",
        )
        .await
        .expect("failed to connect to service ElementInfoProviderService")
        .connect_to_status_provider()
        .expect("failed to connect to protocol ElementInfoProvider");

    assert_eq!(
        [
            fbroker::ElementPowerLevelNames {
                identifier: Some("execution_state".into()),
                levels: Some(vec![
                    fbroker::PowerLevelName {
                        level: Some(0),
                        name: Some("Inactive".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(1),
                        name: Some("WakeHandling".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(2),
                        name: Some("Active".into()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            fbroker::ElementPowerLevelNames {
                identifier: Some("application_activity".into()),
                levels: Some(vec![
                    fbroker::PowerLevelName {
                        level: Some(0),
                        name: Some("Inactive".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(1),
                        name: Some("Active".into()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            fbroker::ElementPowerLevelNames {
                identifier: Some("full_wake_handling".into()),
                levels: Some(vec![
                    fbroker::PowerLevelName {
                        level: Some(0),
                        name: Some("Inactive".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(1),
                        name: Some("Active".into()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            fbroker::ElementPowerLevelNames {
                identifier: Some("wake_handling".into()),
                levels: Some(vec![
                    fbroker::PowerLevelName {
                        level: Some(0),
                        name: Some("Inactive".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(1),
                        name: Some("Active".into()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            fbroker::ElementPowerLevelNames {
                identifier: Some("boot_control".into()),
                levels: Some(vec![
                    fbroker::PowerLevelName {
                        level: Some(0),
                        name: Some("Inactive".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(1),
                        name: Some("Active".into()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            fbroker::ElementPowerLevelNames {
                identifier: Some("execution_resume_latency".into()),
                levels: Some(vec![
                    fbroker::PowerLevelName {
                        level: Some(0),
                        name: Some("1000 ns".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(1),
                        name: Some("100 ns".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(2),
                        name: Some("10 ns".into()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
        ],
        TryInto::<[fbroker::ElementPowerLevelNames; 6]>::try_into(
            element_info_provider.get_element_power_level_names().await?.unwrap()
        )
        .unwrap()
    );

    let status_endpoints: std::collections::HashMap<String, fbroker::StatusProxy> =
        element_info_provider
            .get_status_endpoints()
            .await?
            .unwrap()
            .into_iter()
            .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy().unwrap()))
            .collect();

    let es_status = status_endpoints.get("execution_state".into()).unwrap();
    let aa_status = status_endpoints.get("application_activity".into()).unwrap();
    let fwh_status = status_endpoints.get("full_wake_handling".into()).unwrap();
    let wh_status = status_endpoints.get("wake_handling".into()).unwrap();
    let bc_status = status_endpoints.get("boot_control".into()).unwrap();
    let erl_status = status_endpoints.get("execution_resume_latency".into()).unwrap();

    // First watch should return immediately with default values.
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);
    assert_eq!(aa_status.watch_power_level().await?.unwrap(), 0);
    assert_eq!(fwh_status.watch_power_level().await?.unwrap(), 0);
    assert_eq!(wh_status.watch_power_level().await?.unwrap(), 0);
    assert_eq!(bc_status.watch_power_level().await?.unwrap(), 1);
    assert_eq!(erl_status.watch_power_level().await?.unwrap(), 0);

    // Trigger "boot complete" logic.
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    assert_eq!(aa_status.watch_power_level().await?.unwrap(), 1);
    assert_eq!(bc_status.watch_power_level().await?.unwrap(), 0);

    drop(suspend_lease_control);

    assert_eq!(es_status.watch_power_level().await?.unwrap(), 0);
    assert_eq!(aa_status.watch_power_level().await?.unwrap(), 0);

    // Check suspend is triggered and resume.
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    // Test full wake handling state (full_wake_handling ->1, execution_state -> 1)
    let full_wake_handling_lease_control = lease(&full_wake_controller, 1).await?;

    assert_eq!(es_status.watch_power_level().await?.unwrap(), 1);
    assert_eq!(fwh_status.watch_power_level().await?.unwrap(), 1);

    drop(full_wake_handling_lease_control);

    assert_eq!(es_status.watch_power_level().await?.unwrap(), 0);
    assert_eq!(fwh_status.watch_power_level().await?.unwrap(), 0);

    // Check suspend is triggered and resume.
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    // Test wake handling state (wake_handling ->1, execution_state -> 1)
    let wake_handling_lease_control = lease(&wake_controller, 1).await?;

    assert_eq!(es_status.watch_power_level().await?.unwrap(), 1);
    assert_eq!(wh_status.watch_power_level().await?.unwrap(), 1);

    drop(wake_handling_lease_control);

    assert_eq!(es_status.watch_power_level().await?.unwrap(), 0);
    assert_eq!(wh_status.watch_power_level().await?.unwrap(), 0);

    // Check suspend is triggered and resume.
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    // Test execution resume latency power levels
    for i in 1..3 {
        let erl_lease_control = lease(&erl_controller, i).await?;
        assert_eq!(erl_status.watch_power_level().await?.unwrap(), i);
        drop(erl_lease_control);
        assert_eq!(erl_status.watch_power_level().await?.unwrap(), 0);
    }

    Ok(())
}
