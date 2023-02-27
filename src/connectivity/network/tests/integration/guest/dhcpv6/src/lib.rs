// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_dhcpv6 as fnet_dhcpv6,
    fidl_fuchsia_net_name as fnet_name, fidl_fuchsia_netemul_network as _,
    net_declare::fidl_ip_v6,
    netstack_testing_common::{
        interfaces,
        realms::{KnownServiceProvider, Netstack2},
        setup_network_with,
    },
    netstack_testing_macros::netstack_test,
};

#[netstack_test]
async fn gets_dns_servers(name: &str) {
    diagnostics_log::init!();
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (network, realm, iface, _): (_, _, _, netemul::TestFakeEndpoint<'_>) =
        setup_network_with::<Netstack2, _>(
            &sandbox,
            name,
            None,
            std::iter::once(&KnownServiceProvider::Dhcpv6Client),
        )
        .await
        .expect("failed to setup test");

    let guest_controller = netemul::guest::Controller::new("guest", &network, None)
        .await
        .expect("failed to create guest controller");

    // Transfer files so that DHCPv6 server is started with known configuration
    // and DNS servers acquired by DHCPv6 client can be asserted on by the
    // test.
    for (local_path, remote_path) in [
        ("/pkg/data/dhcp_setup.sh", "/root/input/dhcp_setup.sh"),
        ("/pkg/data/dhcpd6.conf", "/etc/dhcp/dhcpd6.conf"),
    ] {
        let () = guest_controller.put_file(local_path, remote_path).await.unwrap_or_else(|e| {
            panic!("failed to transfer file at path {} to guest: {}", local_path, e)
        });
    }

    guest_controller
        .exec_with_output_logged("/bin/sh -c '/root/input/dhcp_setup.sh -6'", vec![], None)
        .await
        .expect("exec failed");

    let interface_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces/State");
    let addr = interfaces::wait_for_v6_ll(&interface_state, iface.id())
        .await
        .expect("wait for IPv6 link-local address");

    let client_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::ClientProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/ClientProvider");
    let (dhcpv6_client, server_end) = fidl::endpoints::create_proxy::<fnet_dhcpv6::ClientMarker>()
        .expect("create fuchsia.net.dhcpv6/Client client and server ends");
    let () = client_provider
        .new_client(
            fnet_dhcpv6::NewClientParams {
                interface_id: Some(iface.id()),
                address: Some(fnet::Ipv6SocketAddress {
                    address: fnet::Ipv6Address { addr: addr.ipv6_bytes() },
                    port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
                    zone_index: iface.id(),
                }),
                config: Some(fnet_dhcpv6::ClientConfig {
                    information_config: Some(fnet_dhcpv6::InformationConfig {
                        dns_servers: Some(true),
                        ..fnet_dhcpv6::InformationConfig::EMPTY
                    }),
                    ..fnet_dhcpv6::ClientConfig::EMPTY
                }),
                ..fnet_dhcpv6::NewClientParams::EMPTY
            },
            server_end,
        )
        .expect("call fuchsia.net.dhcpv6/Client.NewClient");

    let got = dhcpv6_client.watch_servers().await.expect("watching DNS servers");
    // The DNS server address the client is expected to acquire is as configured in
    // `//src/connectivity/network/tests/integration/guest/data/dhcpd6.conf`.
    const WANT_ADDR: fnet::Ipv6Address = fidl_ip_v6!("2001:db8:1234::1");
    let want = fnet_name::DnsServer_ {
        address: Some(fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
            address: WANT_ADDR,
            zone_index: 0,
            port: dns_server_watcher::DEFAULT_DNS_PORT,
        })),
        source: Some(fnet_name::DnsServerSource::Dhcpv6(fnet_name::Dhcpv6DnsServerSource {
            source_interface: Some(iface.id()),
            ..fnet_name::Dhcpv6DnsServerSource::EMPTY
        })),
        ..fnet_name::DnsServer_::EMPTY
    };
    assert_eq!(&got, &[want]);
}
