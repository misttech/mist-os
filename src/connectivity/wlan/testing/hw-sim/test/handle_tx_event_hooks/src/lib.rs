// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::format_err,
    fidl_fuchsia_wlan_policy as fidl_policy, fidl_fuchsia_wlan_tap as fidl_tap,
    fuchsia_zircon::prelude::*,
    futures::{channel::oneshot, join},
    ieee80211::{Bssid, Ssid},
    pin_utils::pin_mut,
    wlan_common::{
        bss::Protection,
        channel::{Cbw, Channel},
        ie::rsn::cipher::CIPHER_CCMP_128,
    },
    wlan_hw_sim::{
        event::{
            action::{self, AuthenticationControl, AuthenticationTap},
            branch, Handler,
        },
        *,
    },
    wlan_rsn::{self, rsna::UpdateSink},
};

fn scan_and_connect<'h>(
    phy: &'h fidl_tap::WlantapPhyProxy,
    ssid: &'h Ssid,
    bssid: &'h Bssid,
    channel: &'h Channel,
    protection: &'h Protection,
    control: &'h mut AuthenticationControl,
    trace: &'h mut UpdateSink,
    sender: oneshot::Sender<()>,
) -> impl Handler<(), fidl_tap::WlantapPhyEvent> + 'h {
    let beacons = [Beacon {
        ssid: ssid.clone(),
        bssid: *bssid,
        channel: *channel,
        protection: *protection,
        rssi_dbm: -30,
    }];
    let mut sender = Some(sender);
    let tap = AuthenticationTap {
        control,
        handler: branch::try_and((
            event::matched(|control: &mut AuthenticationControl, _| {
                // Copy updates into the trace buffer.
                Ok(trace.extend(control.updates.iter().cloned()))
            }),
            action::authenticate_with_control_state(),
            event::matched(move |control: &mut AuthenticationControl, _| {
                use wlan_rsn::rsna::{SecAssocStatus::EssSaEstablished, SecAssocUpdate::Status};

                // Clear the update sink to prevent copying spurious updates into the trace.
                // Retaining updates is not required for the WPA2 EAPOL exchange, because
                // `TxEapolFrame` updates only occur after association.
                let mut result = Ok(());
                if control.updates.iter().any(|update| matches!(update, Status(EssSaEstablished))) {
                    // Defer errors so that updates are always cleared.
                    result = sender
                        .take()
                        .map_or(Ok(()), |sender| sender.send(()))
                        .map_err(|_| format_err!("failed to send association signal"));
                }
                control.updates.clear();
                result
            }),
        )),
    };
    branch::or((
        event::on_scan(action::send_advertisements_and_scan_completion(phy, beacons)),
        event::on_transmit(action::connect_with_authentication_tap(
            phy, ssid, bssid, channel, protection, tap,
        )),
    ))
    .expect("failed to scan and connect")
}

/// Test a client can connect to a network protected by WPA2-PSK by simulating an AP that
/// authenticates, associates, as well as initiating and completing EAPOL exchange.
/// In this test, no data is being sent after the link becomes up.
#[fuchsia_async::run_singlethreaded(test)]
async fn handle_tx_event_hooks() {
    init_syslog();

    const BSSID: Bssid = Bssid(*b"wpa2ok");
    const PASSWORD: &str = "wpa2good";
    const PROTECTION: Protection = Protection::Wpa2Personal;

    let mut helper = test_utils::TestHelper::begin_test(default_wlantap_config_client()).await;
    let () = loop_until_iface_is_found(&mut helper).await;

    let phy = helper.proxy();
    let (sender, receiver) = oneshot::channel();
    let mut control = AuthenticationControl {
        updates: UpdateSink::new(),
        authenticator: create_authenticator(
            &BSSID,
            &AP_SSID,
            &PASSWORD,
            CIPHER_CCMP_128,
            Protection::Wpa2Personal,
            Protection::Wpa2Personal,
        ),
    };
    let mut trace = UpdateSink::new();

    // Run Policy and wait for the client to connect and EssSa to establish.
    let run_policy_future = async {
        join!(
            save_network_and_wait_until_connected(
                &AP_SSID,
                fidl_policy::SecurityType::Wpa2,
                password_or_psk_to_policy_credential(Some(PASSWORD))
            ),
            async { receiver.await.expect("failed to receive association signal") }
        )
    };
    pin_mut!(run_policy_future);
    helper
        .run_until_complete_or_timeout(
            30.seconds(),
            format!("connecting to {} ({:02X?})", AP_SSID.to_string_not_redactable(), BSSID),
            scan_and_connect(
                &phy,
                &AP_SSID,
                &BSSID,
                &Channel::new(1, Cbw::Cbw20),
                &PROTECTION,
                &mut control,
                &mut trace,
                sender,
            ),
            run_policy_future,
        )
        .await;

    // The `scan_and_connect` event handler accumulates updates from the authentication update
    // sink into `trace`. If association succeeds, then the last six updates must be in the order
    // asserted here.
    use wlan_rsn::{
        key::exchange::Key::{Gtk, Ptk},
        rsna::{
            SecAssocStatus::{EssSaEstablished, PmkSaEstablished},
            SecAssocUpdate::{Key, Status, TxEapolKeyFrame},
        },
    };
    let n = trace.len();
    assert!(
        n >= 6,
        "expected six or more authentication updates but observed fewer:\n{:#?}",
        trace
    );
    assert!(matches!(trace[n - 6], Status(PmkSaEstablished)));
    assert!(matches!(trace[n - 5], TxEapolKeyFrame { .. }));
    assert!(matches!(trace[n - 4], TxEapolKeyFrame { .. }));
    assert!(matches!(trace[n - 3], Key(Ptk(..))));
    assert!(matches!(trace[n - 2], Key(Gtk(..))));
    assert!(matches!(trace[n - 1], Status(EssSaEstablished)));

    helper.stop().await;
}
