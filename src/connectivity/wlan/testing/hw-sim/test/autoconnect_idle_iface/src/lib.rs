// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_policy as fidl_policy,
    fuchsia_zircon::prelude::*,
    tracing::info,
    wlan_common::{assert_variant, bss::Protection},
    wlan_hw_sim::*,
};

/// Tests that an idle interface is automatically connected to a saved network, if present and
/// available, when client connections are enabled.
#[fuchsia_async::run_singlethreaded(test)]
async fn autoconnect_idle_iface() {
    init_syslog();
    let (client_controller, mut client_state_update_stream) = get_client_controller().await;

    wait_until_client_state(&mut client_state_update_stream, |update| {
        if update.state == Some(fidl_policy::WlanClientState::ConnectionsDisabled) {
            return true;
        } else {
            info!("Awaiting client state ConnectionsDisabled, got {:?}", update.state);
            return false;
        }
    })
    .await;

    let network_id = fidl_policy::NetworkIdentifier {
        ssid: AP_SSID.clone().into(),
        type_: fidl_policy::SecurityType::None,
    };

    save_network(
        &client_controller,
        &AP_SSID,
        fidl_policy::SecurityType::None,
        fidl_policy::Credential::None(fidl_policy::Empty),
    )
    .await;

    assert_variant!(
        client_controller.start_client_connections().await,
        Ok(fidl_common::RequestStatus::Acknowledged)
    );

    // Drop client provider controller to allow another to be created in the test setup.
    drop(client_controller);

    let mut helper = test_utils::TestHelper::begin_test(default_wlantap_config_client()).await;
    let () = loop_until_iface_is_found(&mut helper).await;

    let (_client_controller, mut client_state_update_stream) = init_client_controller().await;

    let wait_for_connect =
        Box::pin(wait_until_client_state(&mut client_state_update_stream, |update| {
            has_id_and_state(update, &network_id, fidl_policy::ConnectionState::Connected)
        }));

    connect_or_timeout_with(
        &mut helper,
        30.seconds(),
        &AP_SSID,
        &AP_MAC_ADDR,
        &Protection::Open,
        None,
        wait_for_connect,
    )
    .await;

    helper.stop().await;
}
