// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_power_broker::{
    self as fbroker, LeaseStatus, PowerLevel::UserDefined, UserDefinedPowerLevel,
};
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fidl_test_systemactivitygovernor as ftest;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon::HandleBased;
use power_broker_client::PowerElementContext;
use realm_proxy::client::RealmProxyClient;

async fn create_realm(options: ftest::RealmOptions) -> Result<RealmProxyClient> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    realm_factory
        .create_realm(options, server)
        .await?
        .map_err(realm_proxy::Error::OperationError)?;
    Ok(RealmProxyClient::from(client))
}

#[fuchsia::test]
async fn test_stats_returns_default_values() -> Result<()> {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let realm = create_realm(realm_options).await?;

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
    let realm = create_realm(realm_options).await?;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let es_token = power_elements.execution_state.unwrap().token.unwrap();
    assert!(!es_token.is_invalid_handle());

    Ok(())
}

async fn create_suspend_topology(realm: &RealmProxyClient) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let es_token = power_elements.execution_state.unwrap().token.unwrap();

    let suspend_controller = PowerElementContext::new(
        &topology,
        "suspend_controller",
        &UserDefined(UserDefinedPowerLevel { level: 0 }),
        &UserDefined(UserDefinedPowerLevel { level: 0 }),
        vec![fbroker::LevelDependency {
            dependent_level: UserDefined(UserDefinedPowerLevel { level: 1 }),
            requires_token: es_token,
            requires_level: UserDefined(UserDefinedPowerLevel { level: 2 }),
        }],
        Vec::new(),
    )
    .await?;

    Ok(suspend_controller)
}

async fn cycle_execution_state_power_levels(
    suspend_controller: &PowerElementContext,
) -> Result<()> {
    let lease_control = suspend_controller
        .lessor
        .lease(&UserDefined(UserDefinedPowerLevel { level: 1 }))
        .await?
        .map_err(|e| anyhow::anyhow!("{e:?}"))?
        .into_proxy()?;

    let mut lease_status = LeaseStatus::Unknown;
    while lease_status != LeaseStatus::Satisfied {
        lease_status = lease_control.watch_status(lease_status).await.unwrap();
    }

    drop(lease_control);
    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_increments_suspend_success_on_lease_drop() -> Result<()> {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let realm = create_realm(realm_options).await?;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    let suspend_controller = create_suspend_topology(&realm).await?;
    cycle_execution_state_power_levels(&suspend_controller).await?;

    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(0), current_stats.last_time_in_suspend);

    // Continue incrementing success count on falling edge of Execution State level transitions.
    cycle_execution_state_power_levels(&suspend_controller).await?;

    let current_stats = stats.watch().await?;
    assert_eq!(Some(2), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(0), current_stats.last_time_in_suspend);

    Ok(())
}

// TODO(b/306171083): Add more test cases when passive dependencies are supported.
