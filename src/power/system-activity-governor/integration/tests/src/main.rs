// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_assertions::{
    tree_assertion, AnyProperty, AnyStringProperty, NonZeroUintProperty, TreeAssertion,
};
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::create_endpoints;
use fidl::{AsHandleRef, HandleBased};
use fidl_fuchsia_power_broker::{self as fbroker, LeaseStatus};
use fidl_test_systemactivitygovernor::RealmOptions;
use fuchsia_component::client::connect_to_protocol;
use futures::channel::mpsc;
use futures::{FutureExt, StreamExt};
use power_broker_client::{basic_update_fn_factory, run_power_element, PowerElementContext};
use realm_proxy_client::RealmProxyClient;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
use {
    fidl_fuchsia_hardware_suspend as fhsuspend, fidl_fuchsia_power_observability as fobs,
    fidl_fuchsia_power_suspend as fsuspend, fidl_fuchsia_power_system as fsystem,
    fidl_test_suspendcontrol as tsc, fidl_test_systemactivitygovernor as ftest,
    fuchsia_async as fasync,
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
    create_realm_ext(RealmOptions { use_suspender: Some(true), ..Default::default() }).await
}

async fn create_realm_ext(options: ftest::RealmOptions) -> Result<(RealmProxyClient, String)> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    let result = realm_factory
        .create_realm_ext(options, server)
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

async fn lease(controller: &PowerElementContext, level: u8) -> Result<fbroker::LeaseControlProxy> {
    let lease_control =
        controller.lessor.lease(level).await?.map_err(|e| anyhow::anyhow!("{e:?}"))?.into_proxy();

    let mut lease_status = LeaseStatus::Unknown;
    while lease_status != LeaseStatus::Satisfied {
        lease_status = lease_control.watch_status(lease_status).await.unwrap();
    }

    Ok(lease_control)
}

// Report prolonged match delay after this many loops.
const DELAY_NOTIFICATION: usize = 10;

// Spend no more than this many loop turns before giving up for the inspect to match.
const MAX_LOOPS_COUNT: usize = 20;

const RESTART_DELAY: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(1);

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
                    if i == DELAY_NOTIFICATION {
                        log::warn!(error:?; "Still awaiting inspect match after {} tries", DELAY_NOTIFICATION);
                    }
                    if  i >= MAX_LOOPS_COUNT {  // upper bound, so test terminates on mismatch
                        // Print the actual, so we know why the match failed if it does.
                        return Err(anyhow::anyhow!("err: {}: last observed:\n{}", error, serde_json::to_string_pretty(&data).unwrap()));
                    }
                }
            }
            fasync::Timer::new(fasync::MonotonicInstant::after(RESTART_DELAY)).await;
        }
    }};
}

#[fuchsia::test]
async fn test_activity_governor_with_no_suspender_returns_not_supported_after_suspend_attempt(
) -> Result<()> {
    let (realm, activity_governor_moniker) =
        create_realm_ext(ftest::RealmOptions { use_suspender: Some(false), ..Default::default() })
            .await?;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: contains {
            config: contains {
                use_suspender: false,
            }
        }
    );

    // Indicate that the boot has complete
    let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");

    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(1), current_stats.fail_count);
    assert_eq!(Some(zx::Status::NOT_SUPPORTED.into_raw()), current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);
    Ok(())
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
    let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");

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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
               ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
               ref fobs::SUSPEND_FAIL_COUNT: 0u64,
               ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
               ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
               ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
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
                    power_level: 1u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 1u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                ref fobs::SUSPEND_LAST_DURATION: 1u64,
            },
            suspend_events: {
                "0": {
                    ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_LAST_TIMESTAMP: AnyProperty,
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 1u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                ref fobs::SUSPEND_LAST_DURATION: 1u64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
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
                    power_level: 1u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 2u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: 3u64,
                ref fobs::SUSPEND_LAST_DURATION: 1u64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                },
                "2": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "3": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 3u64,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
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
    let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");

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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
                ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);
    assert_eq!(0u64, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device.resume(&tsc::DeviceResumeRequest::Error(7)).await.unwrap().unwrap();

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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
                ref fobs::SUSPEND_FAIL_COUNT: 1u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 7u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
                ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_FAILED_AT: AnyProperty,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_suspends_successfully_after_failure() -> Result<()> {
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
    let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");

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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
                ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);
    assert_eq!(0u64, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
    suspend_device.resume(&tsc::DeviceResumeRequest::Error(7)).await.unwrap().unwrap();

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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
                ref fobs::SUSPEND_FAIL_COUNT: 1u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 7u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
                ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_FAILED_AT: AnyProperty,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Lease and drop to allow suspend again.
    lease(&suspend_controller, 1).await?;

    assert_eq!(0u64, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());
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
        root: contains {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 1u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 1u64,
                ref fobs::SUSPEND_FAIL_COUNT: 1u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 7u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                ref fobs::SUSPEND_LAST_DURATION: 1u64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_FAILED_AT: AnyProperty,
                },
                "2": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "3": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                },
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

    let (listener_client_end, mut listener_stream) = fidl::endpoints::create_request_stream();
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
                fsystem::ActivityGovernorListenerRequest::OnResume { .. } => {
                    // OnResume never responds.
                    // Check SAG state after resume to confirm SAG doesn't block on the OnResume.
                    on_resume_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    responder.send().unwrap();
                    on_suspend_started_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
    }

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
                cpu: {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
                ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
                "0": {
                    ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // OnSuspendStarted should have been called once.
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

    // Should only have been 1 suspend after all listener handling.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(2), current_stats.last_time_in_suspend);

    // OnResume should have been called once.
    on_resume_rx.next().await.unwrap();

    // OnResume does not block. SAG raises ExecutionState to Suspending state.
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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 1u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                ref fobs::SUSPEND_LAST_DURATION: 1u64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_blocks_for_on_suspend_started() -> Result<()> {
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

    let (listener_client_end, mut listener_stream) = fidl::endpoints::create_request_stream();
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

    // Call SetBootComplete to allow SAG to start suspending.
    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
    }

    // Wait to receive the OnSuspendStarted() callback.
    on_suspend_started_rx.next().await.unwrap();

    // Give SAG some time to take any further suspend actions.
    fasync::Timer::new(fasync::MonotonicDuration::from_millis(1000)).await;

    // Verify that SAG did _not_ suspend the hardware (because we did not
    // respond to the callback).
    assert!(await_suspend.now_or_never().is_none());

    Ok(())
}

#[fuchsia::test]
async fn test_acquire_wake_lease_doesnt_deadlock_in_on_suspend_started() -> Result<()> {
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;

    let (listener_client_end, mut listener_stream) = fidl::endpoints::create_request_stream();
    activity_governor
        .register_listener(fsystem::ActivityGovernorRegisterListenerRequest {
            listener: Some(listener_client_end),
            ..Default::default()
        })
        .await
        .unwrap();

    // Define the listener such that:
    //  - OnResume notifies on_resume_tx.
    //  - OnSuspendStarted calls AcquireWakeLease and passes the lease to on_suspend_started_rx.
    let (on_resume_tx, mut on_resume_rx) = mpsc::channel(1);
    let (on_suspend_started_tx, mut on_suspend_started_rx) = mpsc::channel(1);
    fasync::Task::local(async move {
        let mut on_resume_tx = on_resume_tx;
        let mut on_suspend_started_tx = on_suspend_started_tx;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    log::info!("Running OnResume");
                    on_resume_tx.try_send(()).unwrap();
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    log::info!("Running OnSuspendStarted");
                    let lease = activity_governor
                        .acquire_wake_lease("on_suspend_started_wake_lease")
                        .await
                        .unwrap()
                        .unwrap();
                    on_suspend_started_tx.try_send(lease).unwrap();
                    responder.send().unwrap()
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    // Call SetBootComplete to allow SAG to start suspending.
    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
    }

    // Wait to receive the wake lease from OnSuspendStarted.
    let _wake_lease = on_suspend_started_rx.next().await.unwrap();

    // Verify that SAG did not call Suspender.Suspend due to the existence of the wake lease.
    assert!(suspend_device.await_suspend().now_or_never().is_none());

    // Now wait for the resume callback resulting from OnSuspendStarted's wake lease.
    on_resume_rx.next().await.unwrap();

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_handles_listener_raising_power_levels() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;

    let suspend_controller = create_suspend_topology(&realm).await?;

    let (listener_client_end, mut listener_stream) = fidl::endpoints::create_request_stream();
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
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    // Trigger "boot complete" logic.
    let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");

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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 1u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                ref fobs::SUSPEND_LAST_DURATION: 1u64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 2u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: 3u64,
                ref fobs::SUSPEND_LAST_DURATION: 1u64,
            },
            suspend_events: {
                "0": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "1": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 2u64,
                },
                "2": {
                   ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                },
                "3": {
                    ref fobs::SUSPEND_RESUMED_AT: AnyProperty,
                    ref fobs::SUSPEND_LAST_TIMESTAMP: 3u64,
                },
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

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
                cpu: {
                    power_level: 1u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
                ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Trigger "boot complete" signal.
    let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");

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
                cpu: {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                ref fobs::SUSPEND_SUCCESS_COUNT: 0u64,
                ref fobs::SUSPEND_FAIL_COUNT: 0u64,
                ref fobs::SUSPEND_LAST_FAILED_ERROR: 0u64,
                ref fobs::SUSPEND_LAST_TIMESTAMP: -1i64,
                ref fobs::SUSPEND_LAST_DURATION: -1i64,
            },
            suspend_events: {
                "0": {
                    ref fobs::SUSPEND_ATTEMPTED_AT: AnyProperty,
                }
            },
            wake_leases: {},
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
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

    let suspend_controller = create_suspend_topology(&realm).await?;

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
                identifier: Some("cpu".into()),
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
                identifier: Some("execution_state".into()),
                levels: Some(vec![
                    fbroker::PowerLevelName {
                        level: Some(0),
                        name: Some("Inactive".into()),
                        ..Default::default()
                    },
                    fbroker::PowerLevelName {
                        level: Some(1),
                        name: Some("Suspending".into()),
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
        ],
        TryInto::<[fbroker::ElementPowerLevelNames; 4]>::try_into(
            element_info_provider.get_element_power_level_names().await?.unwrap()
        )
        .unwrap()
    );

    let status_endpoints: HashMap<String, fbroker::StatusProxy> = element_info_provider
        .get_status_endpoints()
        .await?
        .unwrap()
        .into_iter()
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy()))
        .collect();

    let es_status = status_endpoints.get("execution_state").unwrap();
    let aa_status = status_endpoints.get("application_activity").unwrap();
    let bc_status = status_endpoints.get("boot_control").unwrap();

    // First watch should return immediately with default values.
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);
    assert_eq!(aa_status.watch_power_level().await?.unwrap(), 0);
    assert_eq!(bc_status.watch_power_level().await?.unwrap(), 1);

    // Trigger "boot complete" logic.
    let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    assert_eq!(aa_status.watch_power_level().await?.unwrap(), 1);
    let () = boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
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

    // Raise Execution State to Active then drop to trigger a suspend.
    let suspend_lease_control = lease(&suspend_controller, 1).await?;
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);
    drop(suspend_lease_control);
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 0);

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

    // Raise Execution State to Active then drop to trigger a suspend.
    let suspend_lease_control = lease(&suspend_controller, 1).await?;
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);
    drop(suspend_lease_control);
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 0);

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
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy()))
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
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy()))
        .collect();

    let es_status = status_endpoints.get("execution_state").unwrap();
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let wake_lease_name = "wake_lease";
    let wake_lease = activity_governor.take_wake_lease(wake_lease_name).await?;

    // Trigger "boot complete" signal.
    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
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
                    type: AnyStringProperty,
                    status: "Satisfied",
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
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
        }
    );

    // Confirm that the device is called after the wake lease is dropped. In particular, this
    // guarantees that SAG's internal suspend-blocking logic does not prevent suspension.
    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_acquire_wake_lease_raises_execution_state_to_suspending(
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
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy()))
        .collect();

    let es_status = status_endpoints.get("execution_state").unwrap();
    assert_eq!(es_status.watch_power_level().await?.unwrap(), 2);

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let wake_lease_name = "wake_lease";
    let wake_lease = activity_governor.acquire_wake_lease(wake_lease_name).await.unwrap().unwrap();

    // Trigger "boot complete" signal.
    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
    }

    // Execution State should be at the "Suspending" power level, 1.
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
                    type: AnyStringProperty,
                    status: "Satisfied",
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
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_take_application_activity_lease() -> Result<()> {
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
        .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy()))
        .collect();

    let aa_status = status_endpoints.get("application_activity").unwrap();
    assert_eq!(
        aa_status.watch_power_level().await?.unwrap(),
        0 /* ApplicationActivityLevel::Inactive */
    );

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let application_activity_lease_name = "application_activity_lease";
    let application_activity_lease =
        activity_governor.take_application_activity_lease(application_activity_lease_name).await?;

    // Trigger "boot complete" signal.
    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
    }

    assert_eq!(
        aa_status.watch_power_level().await?.unwrap(),
        1 /* ApplicationActivityLevel::Active */
    );

    let server_token_koid =
        &application_activity_lease.basic_info().unwrap().related_koid.raw_koid().to_string();

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: contains {
            wake_leases: {
                var server_token_koid: {
                    created_at: NonZeroUintProperty,
                    client_token_koid: application_activity_lease.get_koid().unwrap().raw_koid(),
                    name: application_activity_lease_name,
                    type: AnyStringProperty,
                }
            },
        }
    );

    drop(application_activity_lease);
    assert_eq!(aa_status.watch_power_level().await?.unwrap(), 0);

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
        wake_lease_child.add_property_assertion("type", Box::new(AnyStringProperty));
        wake_lease_child.add_property_assertion("status", Box::new(AnyStringProperty));
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
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_handles_1000_acquired_wake_leases() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let mut root = TreeAssertion::new("root", false);
    let mut wake_leases_child = TreeAssertion::new("wake_leases", true);
    let mut wake_leases = Vec::new();

    for i in 0..1000 {
        let wake_lease_name = format!("wake_lease{}", i);
        let wake_lease = activity_governor.acquire_wake_lease(&wake_lease_name).await?.unwrap();

        let server_token_koid =
            &wake_lease.basic_info().unwrap().related_koid.raw_koid().to_string();
        let client_token_koid = &wake_lease.get_koid().unwrap().raw_koid();

        let mut wake_lease_child = TreeAssertion::new(server_token_koid, false);
        wake_lease_child.add_property_assertion("created_at", Box::new(NonZeroUintProperty));
        wake_lease_child.add_property_assertion("client_token_koid", Box::new(*client_token_koid));
        wake_lease_child.add_property_assertion("name", Box::new(wake_lease_name));
        wake_lease_child.add_property_assertion("type", Box::new(AnyStringProperty));
        wake_lease_child.add_property_assertion("status", Box::new(AnyStringProperty));
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
            config: {
                use_suspender: true,
                wait_for_suspending_token: false,
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_acquire_wake_lease_returns_error_on_empty_name() -> Result<()> {
    let (realm, _activity_governor_moniker) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    assert_eq!(
        fsystem::AcquireWakeLeaseError::InvalidName,
        activity_governor.acquire_wake_lease("").await?.unwrap_err()
    );

    // Second call should succeed.
    activity_governor.acquire_wake_lease("test").await.unwrap().unwrap();
    Ok(())
}

async fn create_cpu_driver_topology(
    realm: &RealmProxyClient,
) -> Result<(Arc<PowerElementContext>, Arc<Cell<fbroker::PowerLevel>>)> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let cpu_element_manager =
        realm.connect_to_protocol::<fsystem::CpuElementManagerMarker>().await?;
    let cpu_element_token = cpu_element_manager
        .get_cpu_dependency_token()
        .await
        .unwrap()
        .assertive_dependency_token
        .unwrap();

    let cpu_driver_controller = Arc::new(
        PowerElementContext::builder(&topology, "cpu_driver", &[0, 1])
            .dependencies(vec![fbroker::LevelDependency {
                dependency_type: fbroker::DependencyType::Assertive,
                dependent_level: 1,
                requires_token: cpu_element_token,
                requires_level_by_preference: vec![1],
            }])
            .build()
            .await?,
    );

    let cpu_driver_context = cpu_driver_controller.clone();
    let cpu_driver_power_level = Arc::new(Cell::new(0));
    let cpu_driver_power_level2 = cpu_driver_power_level.clone();

    fasync::Task::local(async move {
        let update_fn = Arc::new(basic_update_fn_factory(&cpu_driver_context));
        let cpu_driver_power_level = cpu_driver_power_level2.clone();

        run_power_element(
            &cpu_driver_context.name(),
            &cpu_driver_context.required_level,
            0,    /* initial_level */
            None, /* inspect_node */
            Box::new(move |new_power_level: fbroker::PowerLevel| {
                let update_fn = update_fn.clone();
                let cpu_driver_power_level = cpu_driver_power_level.clone();

                async move {
                    cpu_driver_power_level.set(new_power_level);
                    update_fn(new_power_level).await;
                }
                .boxed_local()
            }),
        )
        .await;
    })
    .detach();

    Ok((cpu_driver_controller, cpu_driver_power_level))
}

#[fuchsia::test]
async fn test_activity_governor_cpu_element_and_execution_state_interaction() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm_ext(ftest::RealmOptions {
        wait_for_suspending_token: Some(true),
        ..Default::default()
    })
    .await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let cpu_element_manager =
        realm.connect_to_protocol::<fsystem::CpuElementManagerMarker>().await?;
    let (cpu_driver_controller, cpu_driver_power_level) =
        create_cpu_driver_topology(&realm).await.unwrap();

    fasync::Task::local(async move {
        cpu_element_manager
            .add_execution_state_dependency(
                fsystem::CpuElementManagerAddExecutionStateDependencyRequest {
                    dependency_token: Some(
                        cpu_driver_controller.assertive_dependency_token().unwrap(),
                    ),
                    power_level: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
    })
    .detach();

    // This call should not be processed until the topology is set up.
    let _wake_lease = activity_governor.take_wake_lease("wake_lease").await?;

    // Trigger "boot complete" signal.
    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
    }

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: contains {
            booting: false,
            power_elements: contains {
                execution_state: {
                    power_level: 1u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                cpu: {
                    power_level: 1u64,
                },
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    assert_eq!(1u8, cpu_driver_power_level.get());

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_cpu_element_returns_invalid_args() -> Result<()> {
    let (realm, _) = create_realm_ext(ftest::RealmOptions {
        wait_for_suspending_token: Some(true),
        ..Default::default()
    })
    .await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let cpu_element_manager =
        realm.connect_to_protocol::<fsystem::CpuElementManagerMarker>().await?;

    // Empty request should return InvalidArgs.
    assert_eq!(
        fsystem::AddExecutionStateDependencyError::InvalidArgs,
        cpu_element_manager
            .add_execution_state_dependency(
                fsystem::CpuElementManagerAddExecutionStateDependencyRequest {
                    dependency_token: None,
                    power_level: None,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap_err()
    );

    // Missing token should return InvalidArgs.
    assert_eq!(
        fsystem::AddExecutionStateDependencyError::InvalidArgs,
        cpu_element_manager
            .add_execution_state_dependency(
                fsystem::CpuElementManagerAddExecutionStateDependencyRequest {
                    dependency_token: None,
                    power_level: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap_err()
    );

    // Missing power level should return InvalidArgs.
    assert_eq!(
        fsystem::AddExecutionStateDependencyError::InvalidArgs,
        cpu_element_manager
            .add_execution_state_dependency(
                fsystem::CpuElementManagerAddExecutionStateDependencyRequest {
                    dependency_token: Some(zx::Event::create()),
                    power_level: None,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap_err()
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_cpu_element_returns_bad_state() -> Result<()> {
    let (realm, _) = create_realm_ext(ftest::RealmOptions {
        wait_for_suspending_token: Some(true),
        ..Default::default()
    })
    .await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let cpu_element_manager =
        realm.connect_to_protocol::<fsystem::CpuElementManagerMarker>().await?;
    let (cpu_driver_controller, _) = create_cpu_driver_topology(&realm).await.unwrap();

    cpu_element_manager
        .add_execution_state_dependency(
            fsystem::CpuElementManagerAddExecutionStateDependencyRequest {
                dependency_token: Some(cpu_driver_controller.assertive_dependency_token().unwrap()),
                power_level: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        fsystem::AddExecutionStateDependencyError::BadState,
        cpu_element_manager
            .add_execution_state_dependency(
                fsystem::CpuElementManagerAddExecutionStateDependencyRequest {
                    dependency_token: Some(
                        cpu_driver_controller.assertive_dependency_token().unwrap()
                    ),
                    power_level: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap_err()
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_cpu_element_allows_leases_during_boot() -> Result<()> {
    let (realm, activity_governor_moniker) = create_realm_ext(ftest::RealmOptions {
        wait_for_suspending_token: Some(true),
        ..Default::default()
    })
    .await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let (cpu_driver_controller, cpu_driver_power_level) =
        create_cpu_driver_topology(&realm).await.unwrap();

    // The CPU power element should be powered up on boot.
    block_until_inspect_matches!(
        activity_governor_moniker,
        root: contains {
            power_elements: {
                cpu: {
                    power_level: 1u64,
                },
            },
        }
    );

    assert_eq!(0u8, cpu_driver_power_level.get());
    let _cpu_lease = lease(&cpu_driver_controller, 1).await.unwrap();
    assert_eq!(1u8, cpu_driver_power_level.get());
    Ok(())
}

#[fuchsia::test]
async fn test_acquire_wake_lease_blocks_during_suspend() -> Result<()> {
    let (realm, _) = create_realm().await?;
    let suspend_device = realm.connect_to_protocol::<tsc::DeviceMarker>().await?;
    set_up_default_suspender(&suspend_device).await;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;

    // Call SetBootComplete to allow SAG to start suspending.
    {
        let boot_control = realm.connect_to_protocol::<fsystem::BootControlMarker>().await?;
        let () =
            boot_control.set_boot_complete().await.expect("SetBootComplete should have succeeded");
    }

    assert_eq!(0, suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap());

    // Spawn an AcquireWakeLease request, and ensure that it's still blocked after a brief wait.
    let mut task = fasync::Task::local(async move {
        activity_governor.acquire_wake_lease("some_wake_lease").await
    });
    fasync::Timer::new(fasync::MonotonicDuration::from_seconds(1)).await;
    assert!(futures::poll!(&mut task).is_pending());

    // Allow the system to resume and confirm that AcquireWakeLease returns.
    suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult::default()))
        .await
        .unwrap()
        .unwrap();
    let _ = task.await;

    Ok(())
}
