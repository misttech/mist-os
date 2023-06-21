// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl_fuchsia_netemul_network as _;
use net_declare::fidl_subnet;
use netemul::InStack;
use netstack_testing_common::{interfaces, realms::Netstack2, setup_network};
use netstack_testing_macros::netstack_test;

#[netstack_test]
async fn acquires_address(name: &str) {
    diagnostics_log::initialize(diagnostics_log::PublishOptions::default())
        .expect("initialize logging");
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (network, realm, iface, _): (_, _, _, netemul::TestFakeEndpoint<'_>) =
        setup_network::<Netstack2>(&sandbox, name, None).await.expect("failed to setup test");

    let guest_controller = netemul::guest::Controller::new("guest", &network, None)
        .await
        .expect("failed to create guest controller");

    // Transfer files so that DHCPv4 server is started with known configuration
    // and the address acquired by DHCPv4 client can be asserted on by the
    // test.
    for (local_path, remote_path) in [
        ("/pkg/data/dhcp_setup.sh", "/root/input/dhcp_setup.sh"),
        ("/pkg/data/dhcpd.conf", "/etc/dhcp/dhcpd.conf"),
    ] {
        let () = guest_controller.put_file(local_path, remote_path).await.unwrap_or_else(|e| {
            panic!("failed to transfer file at path {} to guest: {}", local_path, e)
        });
    }

    guest_controller
        .exec_with_output_logged("/bin/sh -c '/root/input/dhcp_setup.sh -4'", vec![], None)
        .await
        .expect("exec failed");

    let () = iface.start_dhcp::<InStack>().await.expect("failed to start DHCP");

    // The IPv4 address the client is expected to acquire is the only address
    // the DHCPv4 server assigns to clients as found in
    // `//src/connectivity/network/tests/integration/guest/data/dhcpd.conf`.
    const WANT_ADDR: fidl_fuchsia_net::Subnet = fidl_subnet!("192.0.2.10/24");
    let interface_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to fuchsia.net.interfaces/State");
    interfaces::wait_for_addresses(&interface_state, iface.id(), |addresses| {
        addresses
            .iter()
            .any(
                |&fidl_fuchsia_net_interfaces_ext::Address {
                     addr,
                     valid_until: _,
                     assignment_state: _,
                 }| { addr == WANT_ADDR },
            )
            .then_some(())
    })
    .await
    .expect("failed to find expected IPv4 address acquired by client");
}
