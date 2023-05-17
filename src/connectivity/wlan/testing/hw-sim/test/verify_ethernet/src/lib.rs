// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuchsia_zircon::DurationNum,
    wlan_hw_sim::{
        default_wlantap_config_client, init_syslog, loop_until_iface_is_found, netdevice_helper,
        test_utils, CLIENT_MAC_ADDR,
    },
};

/// Test ethernet device is created successfully by verifying a ethernet client with specified
/// MAC address can be created succesfully.
#[fuchsia_async::run_singlethreaded(test)]
async fn verify_ethernet() {
    init_syslog();
    test_utils::TestHelper::start_driver_test_realm()
        .await
        .expect("Failed to start driver test realm");

    // Make sure there is no existing ethernet device.
    let client = netdevice_helper::create_client(fidl_fuchsia_net::MacAddress {
        octets: CLIENT_MAC_ADDR.clone(),
    })
    .await;
    assert!(client.is_none());

    // Create wlan_tap device which will in turn create ethernet device.
    let mut helper = test_utils::TestHelper::begin_test(default_wlantap_config_client()).await;
    let () = loop_until_iface_is_found(&mut helper).await;

    let mut retry = test_utils::RetryWithBackoff::infinite_with_max_interval(5.seconds());
    loop {
        let client = netdevice_helper::create_client(fidl_fuchsia_net::MacAddress {
            octets: CLIENT_MAC_ADDR.clone(),
        })
        .await;
        if client.is_some() {
            break;
        }
        retry.sleep_unless_after_deadline().await.unwrap_or_else(|_| {
            panic!("No netdevice client with mac_addr {:?} found in time", &CLIENT_MAC_ADDR)
        });
    }
    helper.stop().await;
}
