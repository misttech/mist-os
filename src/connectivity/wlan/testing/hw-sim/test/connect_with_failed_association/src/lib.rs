// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_policy as fidl_policy,
    fidl_fuchsia_wlan_tap as fidl_tap,
    fuchsia_zircon::DurationNum,
    ieee80211::{Bssid, Ssid},
    pin_utils::pin_mut,
    wlan_common::{
        bss::Protection,
        channel::{Cbw, Channel},
    },
    wlan_hw_sim::{
        event::{action, branch, Handler},
        *,
    },
};

fn scan_and_associate<'h>(
    phy: &'h fidl_tap::WlantapPhyProxy,
    ssid: &'h Ssid,
    bssid: &'h Bssid,
    channel: &'h Channel,
) -> impl Handler<(), fidl_tap::WlantapPhyEvent> + 'h {
    let beacons = [Beacon {
        channel: *channel,
        bssid: *bssid,
        ssid: ssid.clone(),
        protection: Protection::Wpa2Personal,
        rssi_dbm: -30,
    }];
    branch::or((
        event::on_scan(action::send_advertisements_and_scan_completion(phy, beacons)),
        event::on_transmit(branch::or((
            action::send_open_authentication(
                phy,
                bssid,
                channel,
                fidl_ieee80211::StatusCode::Success,
            ),
            action::send_association_response(
                phy,
                bssid,
                channel,
                fidl_ieee80211::StatusCode::RefusedTemporarily,
            ),
        ))),
    ))
    .expect("failed to scan and associate")
}

async fn save_network_and_await_failed_connection(
    client_controller: &mut fidl_policy::ClientControllerProxy,
    client_state_update_stream: &mut fidl_policy::ClientStateUpdatesRequestStream,
) {
    save_network(
        client_controller,
        &AP_SSID,
        fidl_policy::SecurityType::None,
        password_or_psk_to_policy_credential::<String>(None),
    )
    .await;
    let network_identifier = fidl_policy::NetworkIdentifier {
        ssid: AP_SSID.to_vec(),
        type_: fidl_policy::SecurityType::None,
    };
    await_failed(
        client_state_update_stream,
        network_identifier.clone(),
        fidl_policy::DisconnectStatus::ConnectionFailed,
    )
    .await;
}

/// Test a client connect attempt fails if the association response contains a status code that is
/// not success.
#[fuchsia_async::run_singlethreaded(test)]
async fn connect_with_failed_association() {
    const BSSID: Bssid = Bssid([0x62, 0x73, 0x73, 0x66, 0x6f, 0x6f]);

    init_syslog();

    let mut helper = test_utils::TestHelper::begin_test(default_wlantap_config_client()).await;
    let () = loop_until_iface_is_found(&mut helper).await;

    let (mut client_controller, mut client_state_update_stream) =
        wlan_hw_sim::init_client_controller().await;
    let save_network_fut = save_network_and_await_failed_connection(
        &mut client_controller,
        &mut client_state_update_stream,
    );
    pin_mut!(save_network_fut);

    let phy = helper.proxy();
    let () = helper
        .run_until_complete_or_timeout(
            240.seconds(),
            format!("connecting to {} ({:02X?})", AP_SSID.to_string_not_redactable(), BSSID),
            scan_and_associate(&phy, &AP_SSID, &BSSID, &Channel::new(1, Cbw::Cbw20)),
            save_network_fut,
        )
        .await;

    helper.stop().await;
}
