// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use power_manager_integration_test_lib::client_connectors::{
    DeprecatedRebootWatcherClient, RebootWatcherClient, ThermalClient,
};
use power_manager_integration_test_lib::TestEnvBuilder;
use {fidl_fuchsia_hardware_power_statecontrol as fpower, fuchsia_async as fasync};

/// Integration test for Power Manager to verify correct behavior of the thermal client service.
#[fuchsia::test]
async fn thermal_client_service_test() {
    let mut env = TestEnvBuilder::new()
        .power_manager_node_config_path(
            &"/pkg/thermal_client_service_test/power_manager_node_config.json5",
        )
        .build()
        .await;

    // Check the device has finished enumerating before proceeding with the test
    env.wait_for_device("/dev/sys/platform/soc_thermal").await;

    // The client name here ('client0') must match the name of the client from the thermal
    // configuration file (../config_files/thermal_config.json5)
    let client0 = ThermalClient::new(&env, "client0");

    // Verify initial thermal state is 0
    assert_eq!(client0.get_thermal_state().await.unwrap(), 0);

    // Set temperature to 80 which is above the configured "onset" temperature of 50 (see the
    // `temperature_input_configs` section in ../config_files/power_manager_node_config.json5),
    // causing thermal load to be nonzero
    env.set_temperature("/dev/sys/platform/soc_thermal", 80.0).await;

    // Verify thermal state for client0 is now 1
    assert_eq!(client0.get_thermal_state().await.unwrap(), 1);

    // Set temperature back below the onset threshold
    env.set_temperature("/dev/sys/platform/soc_thermal", 40.0).await;

    // Verify client0 thermal state goes back to 0
    assert_eq!(client0.get_thermal_state().await.unwrap(), 0);

    env.destroy().await;
}

/// Verifies that `fuchsia.hardware.power.statecontrol/Admin.Reboot` requests
/// sent to Power Manager are handled as expected:
///     1) forward the reboot reason to connected RebootWatcher clients
///     2) update Driver Manager with the appropriate termination SystemPowerState
///     3) send a shutdown request to the system controller
// TODO(https://fxbug.dev/385742868): Delete this test once the API is removed.
#[fuchsia::test]
async fn shutdown_test() {
    let mut env = TestEnvBuilder::new()
        .power_manager_node_config_path(&"/pkg/shutdown_test/power_manager_node_config.json5")
        .build()
        .await;

    // Check the device has finished enumerating before proceeding with the test
    env.wait_for_device("/dev/sys/platform/soc_thermal").await;

    // Verify both the "current" and "deprecated" watcher APIs.
    let mut reboot_watcher = RebootWatcherClient::new(&env).await;
    let mut deprecated_reboot_watcher = DeprecatedRebootWatcherClient::new(&env).await;

    // Send a reboot request to the Power Manager (in a separate Task because it never returns)
    let shutdown_client = env.connect_to_protocol::<fpower::AdminMarker>();
    let _shutdown_task =
        fasync::Task::local(shutdown_client.reboot(fpower::RebootReason::SystemUpdate));

    // Verify Power Manager forwards the reboot reason to the watcher client
    let reasons = assert_matches!(
        reboot_watcher.get_reboot_options().await,
        fpower::RebootOptions{ reasons: Some(reasons), ..} => reasons
    );
    assert_eq!(&reasons[..], [fpower::RebootReason2::SystemUpdate]);
    assert_eq!(
        deprecated_reboot_watcher.get_reboot_reason().await,
        fpower::RebootReason::SystemUpdate
    );

    // Verify the system controller service gets the shutdown request from Power Manager
    env.mocks.system_controller_service.wait_for_shutdown_request().await;

    // TODO(https://fxbug.dev/42071033): Update integration tests to ensure interaction between driver
    // manager and shutdown shim works as intended. This likely requires using shutdown shim in
    // this integration test.

    env.destroy().await;
}

// TODO(https://fxbug.dev/385312336): Add integration tests for `PerformReboot`.
