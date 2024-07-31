// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_assertions::{tree_assertion, AnyProperty, NonZeroUintProperty, TreeAssertion};
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_power_broker::{self as fbroker, LeaseStatus};
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon::{self as zx, AsHandleRef, HandleBased};
use futures::channel::mpsc;
use futures::{FutureExt, StreamExt};
use power_broker_client::{basic_update_fn_factory, run_power_element, PowerElementContext};
use realm_proxy_client::RealmProxyClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use {
    fidl_fuchsia_hardware_suspend as fhsuspend, fidl_fuchsia_power_suspend as fsuspend,
    fidl_fuchsia_power_system as fsystem, fidl_test_suspendcontrol as tsc,
    fidl_test_systemactivitygovernor as ftest, fuchsia_async as fasync,
};

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

async fn create_realm() -> Result<(RealmProxyClient, String)> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    let result = realm_factory
        .create_realm(server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    Ok((RealmProxyClient::from(client), result))
}

#[fuchsia::test]
async fn test_stats_returns_default_values() -> Result<()> {
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let es_token = power_elements.execution_state.unwrap().opportunistic_dependency_token.unwrap();
    assert!(!es_token.is_invalid_handle());

    let aa_element = power_elements.application_activity.unwrap();
    let aa_assertive_token = aa_element.assertive_dependency_token.unwrap();
    assert!(!aa_assertive_token.is_invalid_handle());

    let wh_element = power_elements.wake_handling.unwrap();
    let wh_assertive_token = wh_element.assertive_dependency_token.unwrap();
    assert!(!wh_assertive_token.is_invalid_handle());

    let fwh_element = power_elements.full_wake_handling.unwrap();
    let fwh_assertive_token = fwh_element.assertive_dependency_token.unwrap();
    assert!(!fwh_assertive_token.is_invalid_handle());

    let erl_element = power_elements.execution_resume_latency.unwrap();
    let erl_opportunistic_token = erl_element.opportunistic_dependency_token.unwrap();
    assert!(!erl_opportunistic_token.is_invalid_handle());
    let erl_assertive_token = erl_element.assertive_dependency_token.unwrap();
    assert!(!erl_assertive_token.is_invalid_handle());

    Ok(())
}

async fn create_suspend_topology(realm: &RealmProxyClient) -> Result<Arc<PowerElementContext>> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let aa_token = power_elements.application_activity.unwrap().assertive_dependency_token.unwrap();

    let suspend_controller = Arc::new(
        PowerElementContext::builder(&topology, "suspend_controller", &[0, 1])
            .dependencies(vec![fbroker::LevelDependency {
                dependency_type: fbroker::DependencyType::Assertive,
                dependent_level: 1,
                requires_token: aa_token,
                requires_level_by_preference: vec![1],
            }])
            .build()
            .await?,
    );
    let sc_context = suspend_controller.clone();
    fasync::Task::local(async move {
        run_power_element(
            &sc_context.name(),
            &sc_context.required_level,
            0,    /* initial_level */
            None, /* inspect_node */
            basic_update_fn_factory(&sc_context),
        )
        .await;
    })
    .detach();

    Ok(suspend_controller)
}

async fn create_wake_topology(realm: &RealmProxyClient) -> Result<Arc<PowerElementContext>> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let wh_token = power_elements.wake_handling.unwrap().assertive_dependency_token.unwrap();

    let wake_controller = Arc::new(
        PowerElementContext::builder(&topology, "wake_controller", &[0, 1])
            .dependencies(vec![fbroker::LevelDependency {
                dependency_type: fbroker::DependencyType::Assertive,
                dependent_level: 1,
                requires_token: wh_token,
                requires_level_by_preference: vec![1],
            }])
            .build()
            .await?,
    );
    let wc_context = wake_controller.clone();
    fasync::Task::local(async move {
        run_power_element(
            &wc_context.name(),
            &wc_context.required_level,
            0,    /* initial_level */
            None, /* inspect_node */
            basic_update_fn_factory(&wc_context),
        )
        .await;
    })
    .detach();

    Ok(wake_controller)
}

async fn create_full_wake_topology(realm: &RealmProxyClient) -> Result<Arc<PowerElementContext>> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let fwh_token = power_elements.full_wake_handling.unwrap().assertive_dependency_token.unwrap();

    let full_wake_controller = Arc::new(
        PowerElementContext::builder(&topology, "full_wake_controller", &[0, 1])
            .dependencies(vec![fbroker::LevelDependency {
                dependency_type: fbroker::DependencyType::Assertive,
                dependent_level: 1,
                requires_token: fwh_token,
                requires_level_by_preference: vec![1],
            }])
            .build()
            .await?,
    );
    let fwc_context = full_wake_controller.clone();
    fasync::Task::local(async move {
        run_power_element(
            &fwc_context.name(),
            &fwc_context.required_level,
            0,    /* initial_level */
            None, /* inspect_node */
            basic_update_fn_factory(&fwc_context),
        )
        .await;
    })
    .detach();

    Ok(full_wake_controller)
}

async fn create_latency_topology(
    realm: &RealmProxyClient,
    expected_latencies: &Vec<i64>,
) -> Result<Arc<PowerElementContext>> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let erl = power_elements.execution_resume_latency.unwrap();
    let erl_token = erl.assertive_dependency_token.unwrap();
    assert_eq!(*expected_latencies, erl.resume_latencies.unwrap());

    let levels = Vec::from_iter(0..expected_latencies.len().try_into().unwrap());

    let erl_controller = Arc::new(
        PowerElementContext::builder(&topology, "erl_controller", &levels)
            .dependencies(
                levels
                    .iter()
                    .map(|level| fbroker::LevelDependency {
                        dependency_type: fbroker::DependencyType::Assertive,
                        dependent_level: *level,
                        requires_token: erl_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .unwrap(),
                        requires_level_by_preference: vec![*level],
                    })
                    .collect(),
            )
            .build()
            .await?,
    );
    let erlc_context = erl_controller.clone();
    fasync::Task::local(async move {
        run_power_element(
            &erlc_context.name(),
            &erlc_context.required_level,
            0,    /* initial_level */
            None, /* inspect_node */
            basic_update_fn_factory(&erlc_context),
        )
        .await;
    })
    .detach();

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

const MACRO_LOOP_EXIT: bool = false; // useful in development; prevent hangs from inspect mismatch

macro_rules! block_until_inspect_matches {
    ($sag_moniker:expr, $($tree:tt)+) => {{
        let mut reader = ArchiveReader::new();

        reader
            .select_all_for_moniker(&format!("{}/{}", REALM_FACTORY_CHILD_NAME, $sag_moniker))
            .with_minimum_schema_count(1);

        for i in 1.. {
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
                    if MACRO_LOOP_EXIT && i == 50 {  // upper bound, so test terminates on mismatch
                        return Err(error.into())
                    }
                }
            }
        }
    }};
}

#[fuchsia::test]
async fn test_activity_governor_increments_suspend_success_on_application_activity_lease_drop(
) -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
            suspend_events: {
            },
            wake_leases: {},
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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
            },
            wake_leases: {},
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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(3i64),
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
    assert_eq!(Some(3), current_stats.last_time_in_suspend);

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
                last_time_in_suspend: 3u64,
                last_time_in_suspend_operations: 1u64,
            },
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
                "2": {
                    suspended: AnyProperty,
                },
                "3": {
                    resumed: AnyProperty,
                    duration: 3u64,
                },
            },
            wake_leases: {},
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
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(wake_handling_lease_control);
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(3i64),
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
                last_time_in_suspend: 3u64,
                last_time_in_suspend_operations: 1u64,
            },
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
                "2": {
                    suspended: AnyProperty,
                },
                "3": {
                    resumed: AnyProperty,
                    duration: 3u64,
                },
            },
            wake_leases: {},
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
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(wake_handling_lease_control);
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(3i64),
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
                last_time_in_suspend: 3u64,
                last_time_in_suspend_operations: 1u64,
            },
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
                "2": {
                    suspended: AnyProperty,
                },
                "3": {
                    resumed: AnyProperty,
                    duration: 3u64,
                },
            },
            wake_leases: {},
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
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
                suspend_events: {
                },
                wake_leases: {},
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
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
            suspend_events: {
            },
            wake_leases: {},
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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 1000u64,
                },
            },
            wake_leases: {},
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
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
            suspend_events: {
            },
            wake_leases: {},
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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    suspend_failed: AnyProperty,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_suspends_after_listener_hanging_on_resume() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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

    let (on_suspend_started_tx, mut on_suspend_started_rx) = mpsc::channel(1);
    let (on_resume_tx, mut on_resume_rx) = mpsc::channel(1);

    fasync::Task::local(async move {
        let mut on_suspend_started_tx = on_suspend_started_tx;
        let mut on_resume_tx = on_resume_tx;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    fasync::Timer::new(fasync::Duration::from_millis(50)).await;
                    responder.send().unwrap();
                    fasync::Timer::new(fasync::Duration::from_millis(50)).await;
                    on_resume_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    responder.send().unwrap();
                    on_suspend_started_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                    responder.send().unwrap();
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

    // OnSuspendStarted and OnResume should have been called once.
    on_suspend_started_rx.next().await.unwrap();

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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_does_not_suspend_while_on_suspend_started_hangs() -> Result<()> {
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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

    let (on_suspend_started_tx, mut on_suspend_started_rx) = mpsc::channel(1);
    fasync::Task::local(async move {
        let mut on_suspend_started_tx = on_suspend_started_tx;
        let mut _on_suspend_started_responder;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    _on_suspend_started_responder = responder;
                    on_suspend_started_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    // Queue up a callback from `suspend_device`, to let us know when
    // SAG requests to suspend the hardware.
    let await_suspend = suspend_device.await_suspend();

    // Cycle execution_state power level to make SAG start suspending.
    {
        let suspend_controller = create_suspend_topology(&realm).await?;
        lease(&suspend_controller, 1).await?;
    }

    // Wait to receive the OnSuspendStarted() callback.
    on_suspend_started_rx.next().await.unwrap();

    // Give SAG some time to take any further suspend actions.
    fasync::Timer::new(fasync::Duration::from_millis(1000)).await;

    // Verify that SAG did _not_ suspend the hardware (because we did not
    // respond to the callback).
    assert!(await_suspend.now_or_never().is_none());
    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_handles_listener_raising_power_levels() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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

    let (on_suspend_started_tx, mut on_suspend_started_rx) = mpsc::channel(1);
    let (on_resume_tx, mut on_resume_rx) = mpsc::channel(1);

    fasync::Task::local(async move {
        let mut on_suspend_started_tx = on_suspend_started_tx;
        let mut on_resume_tx = on_resume_tx;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    let suspend_lease = lease(&suspend_controller, 1).await.unwrap();
                    on_resume_tx.try_send(suspend_lease).unwrap();
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    responder.send().unwrap();
                    on_suspend_started_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    drop(suspend_lease);

    // OnSuspendStarted should have been called.
    on_suspend_started_rx.next().await.unwrap();

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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Drop the lease and wait for suspend,
    on_resume_rx.next().await.unwrap();

    // OnSuspendStarted should be called again.
    on_suspend_started_rx.next().await.unwrap();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(3i64),
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
                last_time_in_suspend: 3u64,
                last_time_in_suspend_operations: 1u64,
            },
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    resumed: AnyProperty,
                    duration: 2u64,
                },
                "2": {
                    suspended: AnyProperty,
                },
                "3": {
                    resumed: AnyProperty,
                    duration: 3u64,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_handles_suspend_failure() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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

    let (on_suspend_started_tx, mut on_suspend_started_rx) = mpsc::channel(1);
    let (on_suspend_fail_tx, mut on_suspend_fail_rx) = mpsc::channel(1);

    fasync::Task::local(async move {
        let mut on_suspend_started_tx = on_suspend_started_tx;
        let mut on_suspend_fail_tx = on_suspend_fail_tx;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    responder.send().unwrap();
                    on_suspend_started_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                    let suspend_lease = lease(&suspend_controller, 1).await.unwrap();
                    on_suspend_fail_tx.try_send(suspend_lease).unwrap();
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    drop(suspend_lease);

    // OnSuspendStarted should have been called.
    on_suspend_started_rx.next().await.unwrap();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: None,
            suspend_overhead: None,
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
                success_count: 0u64,
                fail_count: 1u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                },
                "1": {
                    suspend_failed: AnyProperty,
                },
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Drop the lease and wait for suspend,
    on_suspend_fail_rx.next().await.unwrap();

    // OnSuspendStarted should be called again.
    on_suspend_started_rx.next().await.unwrap();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_blocks_lease_while_suspend_in_progress() -> Result<()> {
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    // Inform SAG that booting has completed.
    //
    // Note that the resulting lease is dropped immediately. Dropping
    // the lease allows the `await_suspend()` call below to complete.
    let suspend_controller = create_suspend_topology(&realm).await?;
    lease(&suspend_controller, 1).await.unwrap();

    // Register a listener to receive the `OnResume()` callback.
    //
    // This must be done before the `await_suspend()` call, to avoid
    // having the registration race with the invocation of resume
    // callbacks.
    let (listener_client_end, mut listener_stream) =
        fidl::endpoints::create_request_stream().unwrap();
    activity_governor
        .register_listener(fsystem::ActivityGovernorRegisterListenerRequest {
            listener: Some(listener_client_end),
            ..Default::default()
        })
        .await
        .unwrap();

    // Start a task to process callbacks from SAG.
    //
    // This must be done before the `await_suspend()` call. Otherwise:
    // 1. SAG will be stuck waiting for a reply to `OnSuspendStarted()`.
    // 2. The reply will never come, because `await_suspend()` has not
    //    returned.
    let (mut on_resume_tx, mut on_resume_rx) = mpsc::channel(1);
    fasync::Task::local(async move {
        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    on_resume_tx.start_send(()).unwrap();
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());

    let now = Instant::now();
    let (mut on_lease_tx, mut on_lease_rx) = mpsc::channel(1);
    fasync::Task::local(async move {
        {
            // Try to take a lease. This should not be granted until the system resumes.
            lease(&suspend_controller, 1).await.unwrap();
            on_lease_tx.try_send(now.elapsed()).unwrap();
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

    // Note: although there's no lease held here, and SAG could
    // initiate another suspend before `stats.watch()` completes,
    // there's no race regarding the values in `current_stats`.
    //
    // The values in `current_stats` are well-defined, because:
    // 1. SAG cannot update these values until SAG's suspend request
    //    to the suspend HAL completes, and
    // 2. SAG's request to the suspend HAL can't complete until
    //    this test call suspend_device.await_suspend() again.
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
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
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
            suspend_events: {
            },
            wake_leases: {},
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
            suspend_events: {
                "0": {
                    suspended: AnyProperty,
                }
            },
            wake_leases: {},
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_element_info_provider() -> Result<()> {
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;

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

    let status_endpoints: HashMap<String, fbroker::StatusProxy> = element_info_provider
        .get_status_endpoints()
        .await?
        .unwrap()
        .into_iter()
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy().unwrap()))
        .collect();

    let es_status = status_endpoints.get("execution_state").unwrap();
    let aa_status = status_endpoints.get("application_activity").unwrap();
    let fwh_status = status_endpoints.get("full_wake_handling").unwrap();
    let wh_status = status_endpoints.get("wake_handling").unwrap();
    let bc_status = status_endpoints.get("boot_control").unwrap();
    let erl_status = status_endpoints.get("execution_resume_latency").unwrap();

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

// It is not possible to deterministically catch a bad initial state with current APIs.
// Instead, ensure that the simplest connect and assert always passes.
// If the initial state is not correct at least some of the time, this test will flake.
#[fuchsia::test]
async fn test_execution_state_always_starts_at_active_power_level() -> Result<()> {
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let element_info_provider = realm
        .connect_to_service_instance::<fbroker::ElementInfoProviderServiceMarker>(
            &"system_activity_governor",
        )
        .await
        .expect("failed to connect to service ElementInfoProviderService")
        .connect_to_status_provider()
        .expect("failed to connect to protocol ElementInfoProvider");

    let status_endpoints: HashMap<String, fbroker::StatusProxy> = element_info_provider
        .get_status_endpoints()
        .await?
        .unwrap()
        .into_iter()
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy().unwrap()))
        .collect();

    let es_status = status_endpoints.get("execution_state").unwrap();
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);
    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_take_wake_lease_raises_execution_state_to_wake_handling(
) -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let element_info_provider = realm
        .connect_to_service_instance::<fbroker::ElementInfoProviderServiceMarker>(
            &"system_activity_governor",
        )
        .await
        .expect("failed to connect to service ElementInfoProviderService")
        .connect_to_status_provider()
        .expect("failed to connect to protocol ElementInfoProvider");

    let status_endpoints: HashMap<String, fbroker::StatusProxy> = element_info_provider
        .get_status_endpoints()
        .await?
        .unwrap()
        .into_iter()
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy().unwrap()))
        .collect();

    let es_status = status_endpoints.get("execution_state").unwrap();
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let wake_lease_name = "wake_lease";
    let wake_lease = activity_governor.take_wake_lease(wake_lease_name).await?;

    // Trigger "boot complete" signal.
    {
        let suspend_controller = create_suspend_topology(&realm).await?;
        lease(&suspend_controller, 1).await.unwrap();
    }

    assert_eq!(es_status.watch_power_level().await?.unwrap(), 1);

    let server_token_koid = &wake_lease.basic_info().unwrap().related_koid.raw_koid().to_string();

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: contains {
            wake_leases: {
                var server_token_koid: {
                    created_at: NonZeroUintProperty,
                    client_token_koid: wake_lease.get_koid().unwrap().raw_koid(),
                    name: wake_lease_name,
                }
            },
        }
    );

    drop(wake_lease);
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 0);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: contains {
            wake_leases: {},
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_handles_1000_wake_leases() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let mut root = TreeAssertion::new("root", false);
    let mut wake_leases_child = TreeAssertion::new("wake_leases", true);
    let mut wake_leases = Vec::new();

    for i in 0..1000 {
        let wake_lease_name = format!("wake_lease{}", i);
        let wake_lease = activity_governor.take_wake_lease(&wake_lease_name).await?;

        let server_token_koid =
            &wake_lease.basic_info().unwrap().related_koid.raw_koid().to_string();
        let client_token_koid = &wake_lease.get_koid().unwrap().raw_koid();

        let mut wake_lease_child = TreeAssertion::new(server_token_koid, false);
        wake_lease_child.add_property_assertion("created_at", Box::new(NonZeroUintProperty));
        wake_lease_child.add_property_assertion("client_token_koid", Box::new(*client_token_koid));
        wake_lease_child.add_property_assertion("name", Box::new(wake_lease_name));
        wake_leases_child.add_child_assertion(wake_lease_child);

        wake_leases.push(wake_lease);
    }

    root.add_child_assertion(wake_leases_child);

    let mut reader = ArchiveReader::new();

    reader
        .select_all_for_moniker(&format!(
            "{}/{}",
            REALM_FACTORY_CHILD_NAME, &activity_governor_moniker
        ))
        .with_minimum_schema_count(1);

    let inspect = reader
        .snapshot::<Inspect>()
        .await?
        .into_iter()
        .next()
        .and_then(|result| result.payload)
        .ok_or(anyhow::anyhow!("expected one inspect hierarchy"))
        .unwrap();

    root.run(&inspect).unwrap();

    drop(wake_leases);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: contains {
            wake_leases: {},
        }
    );

    Ok(())
}
