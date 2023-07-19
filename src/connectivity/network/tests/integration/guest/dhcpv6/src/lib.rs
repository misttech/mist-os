// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use {
    assert_matches::assert_matches,
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_dhcpv6 as fnet_dhcpv6,
    fidl_fuchsia_net_dhcpv6_ext as fnet_dhcpv6_ext, fidl_fuchsia_net_name as fnet_name,
    fidl_fuchsia_netemul_network as _,
    net_declare::fidl_ip_v6,
    netstack_testing_common::{
        interfaces,
        realms::{KnownServiceProvider, Netstack2},
        setup_network_with,
    },
    netstack_testing_macros::netstack_test,
    test_case::test_case,
};

#[netstack_test]
async fn gets_dns_servers(name: &str) {
    diagnostics_log::initialize(diagnostics_log::PublishOptions::default()).expect("init logging");
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
            &fnet_dhcpv6_ext::NewClientParams {
                interface_id: iface.id(),
                address: fnet::Ipv6SocketAddress {
                    address: fnet::Ipv6Address { addr: addr.ipv6_bytes() },
                    port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
                    zone_index: iface.id(),
                },
                config: fnet_dhcpv6_ext::ClientConfig {
                    information_config: fnet_dhcpv6::InformationConfig { dns_servers: true },
                    non_temporary_address_config: Default::default(),
                    prefix_delegation_config: None,
                },
            }
            .into(),
            server_end,
        )
        .expect("call fuchsia.net.dhcpv6/ClientProvider.NewClient");

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
            ..Default::default()
        })),
        ..Default::default()
    };
    assert_eq!(&got, &[want]);
}

const SERVER_ID1: u16 = 1;
const SERVER_ID2: u16 = 2;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum PrefixCount {
    One,
    Two,
}

impl Into<u16> for PrefixCount {
    fn into(self) -> u16 {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct Dhcpv6ServerConfig {
    identifier: u16,
    prefix_low: net_types::ip::Subnet<net_types::ip::Ipv6Addr>,
    prefix_high: net_types::ip::Subnet<net_types::ip::Ipv6Addr>,
    subnet: net_types::ip::Subnet<net_types::ip::Ipv6Addr>,
    server_addr: net_types::ip::AddrSubnet<net_types::ip::Ipv6Addr>,
}

impl Dhcpv6ServerConfig {
    const PD_PREFIX_LEN: u8 = 64;
    const SUBNET_PREFIX_LEN: u8 = 48;

    fn new(identifier: u16, prefix_count: PrefixCount) -> Self {
        Self {
            identifier,
            prefix_low: net_types::ip::Subnet::new(
                net_types::ip::Ipv6Addr::new([0x2001, 0xdb8, identifier, 1, 0, 0, 0, 0]),
                Self::PD_PREFIX_LEN,
            )
            .expect("failed to construct subnet"),
            prefix_high: net_types::ip::Subnet::new(
                net_types::ip::Ipv6Addr::new([
                    0x2001,
                    0xdb8,
                    identifier,
                    prefix_count.into(),
                    0,
                    0,
                    0,
                    0,
                ]),
                Self::PD_PREFIX_LEN,
            )
            .expect("failed to construct subnet"),
            subnet: net_types::ip::Subnet::new(
                net_types::ip::Ipv6Addr::new([0x2001, 0xdb8, identifier, 0, 0, 0, 0, 0]),
                Self::SUBNET_PREFIX_LEN,
            )
            .expect("failed to construct subnet"),
            server_addr: {
                let addr = net_types::ip::Ipv6Addr::new([0x2001, 0xdb8, identifier, 0, 0, 0, 0, 1]);
                net_types::ip::AddrSubnet::new(addr, Self::SUBNET_PREFIX_LEN).unwrap_or_else(|e| {
                    panic!(
                        "failed to construct addr-in-subnet from {}/{}: {:?}",
                        addr,
                        Self::SUBNET_PREFIX_LEN,
                        e
                    )
                })
            },
        }
    }
}

const SERVER_SCRIPT_REMOTE: &str = "/root/input/dhcpv6_server.sh";
const SERVER_SCRIPT_LOCAL: &str = "/pkg/data/dhcpv6_server.sh";

async fn start_server(
    guest_controller: &netemul::guest::Controller,
    Dhcpv6ServerConfig {
        identifier,
        prefix_low,
        prefix_high,
        subnet,
        server_addr,
    }: Dhcpv6ServerConfig,
) {
    let cmd = format!(
        "/bin/sh -c '{SERVER_SCRIPT_REMOTE} start \
                --prefix-low {} --prefix-high {} --prefix-len {} {:x} {} {}'",
        prefix_low.network(),
        prefix_high.network(),
        Dhcpv6ServerConfig::PD_PREFIX_LEN,
        identifier,
        subnet,
        server_addr,
    );
    guest_controller.exec_with_output_logged(&cmd, vec![], None).await.expect("exec failed");
}

fn into_fidl_ipv6_address_with_prefix(
    subnet: net_types::ip::Subnet<net_types::ip::Ipv6Addr>,
) -> fnet::Ipv6AddressWithPrefix {
    fnet::Ipv6AddressWithPrefix {
        addr: fnet::Ipv6Address { addr: subnet.network().ipv6_bytes() },
        prefix_len: subnet.prefix(),
    }
}

#[track_caller]
fn get_single_prefix(mut got_prefixes: Vec<fnet_dhcpv6::Prefix>) -> fnet_dhcpv6::Prefix {
    let prefix = got_prefixes.pop().expect("must acquire at least one prefix");
    assert_eq!(&got_prefixes, &[]);
    prefix
}

#[track_caller]
fn assert_prefix(
    mut got_prefixes: Vec<fnet_dhcpv6::Prefix>,
    want_prefix: net_types::ip::Subnet<net_types::ip::Ipv6Addr>,
) -> fnet_dhcpv6::Lifetimes {
    let lifetimes = assert_matches!(
        got_prefixes.pop().expect("must acquire at least one prefix"),
        fnet_dhcpv6::Prefix {
            prefix,
            lifetimes,
        } => {
            assert_eq!(prefix, into_fidl_ipv6_address_with_prefix(want_prefix));
            lifetimes
        }
    );
    assert_eq!(&got_prefixes, &[]);
    lifetimes
}

#[track_caller]
fn start_dhcpv6_client(
    client_provider: &fnet_dhcpv6::ClientProviderProxy,
    interface_id: u64,
    addr: net_types::ip::Ipv6Addr,
    client_config: fnet_dhcpv6_ext::ClientConfig,
) -> fnet_dhcpv6::ClientProxy {
    let (dhcpv6_client, server_end) = fidl::endpoints::create_proxy::<fnet_dhcpv6::ClientMarker>()
        .expect("create fuchsia.net.dhcpv6/Client client and server ends");
    let () = client_provider
        .new_client(
            &fnet_dhcpv6_ext::NewClientParams {
                interface_id: interface_id,
                address: fnet::Ipv6SocketAddress {
                    address: fnet::Ipv6Address { addr: addr.ipv6_bytes() },
                    port: fnet_dhcpv6::DEFAULT_CLIENT_PORT,
                    zone_index: interface_id,
                },
                config: client_config,
            },
            server_end,
        )
        .expect("call fuchsia.net.dhcpv6/ClientProvider.NewClient");
    dhcpv6_client
}

async fn test_setup<'a>(
    name: &'a str,
    sandbox: &'a netemul::TestSandbox,
) -> (
    netemul::TestNetwork<'a>,
    netemul::TestRealm<'a>,
    netemul::TestInterface<'a>,
    netemul::guest::Controller,
    net_types::ip::Ipv6Addr,
    fnet_dhcpv6::ClientProviderProxy,
) {
    tracing::subscriber::set_global_default(diagnostics_log::Publisher::default())
        .expect("init logging");

    let (network, realm, iface, _): (_, _, _, netemul::TestFakeEndpoint<'_>) =
        setup_network_with::<Netstack2, _>(
            sandbox,
            name,
            None,
            std::iter::once(&KnownServiceProvider::Dhcpv6Client),
        )
        .await
        .expect("failed to set up test");

    let guest_controller = netemul::guest::Controller::new("guest", &network, None)
        .await
        .expect("failed to create guest controller");

    let () = guest_controller
        .put_file(SERVER_SCRIPT_LOCAL, SERVER_SCRIPT_REMOTE)
        .await
        .expect("failed to put server control script");

    guest_controller
        .exec_with_output_logged(
            &format!("/bin/sh -c '{SERVER_SCRIPT_REMOTE} setup'"),
            vec![],
            None,
        )
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

    (network, realm, iface, guest_controller, addr, client_provider)
}

#[netstack_test]
async fn stateful_renew(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_network, _realm, iface, guest_controller, addr, client_provider) =
        test_setup(name, &sandbox).await;

    let config = Dhcpv6ServerConfig::new(SERVER_ID1, PrefixCount::One);
    start_server(&guest_controller, config).await;

    let dhcpv6_client = start_dhcpv6_client(
        &client_provider,
        iface.id(),
        addr,
        fnet_dhcpv6_ext::ClientConfig {
            prefix_delegation_config: Some(fnet_dhcpv6::PrefixDelegationConfig::Empty(
                fnet_dhcpv6::Empty,
            )),
            information_config: Default::default(),
            non_temporary_address_config: Default::default(),
        },
    );

    // Assert that DHCPv6 client obtains a delegated prefix.
    let got_prefixes = dhcpv6_client.watch_prefixes().await.expect("watching prefixes");
    let fnet_dhcpv6::Lifetimes { valid_until, preferred_until } =
        assert_prefix(got_prefixes, config.prefix_low);

    // Assert that the DHCPv6 client is able to renew the delegated prefix.
    let got_prefixes = dhcpv6_client.watch_prefixes().await.expect("watching prefixes");
    let fnet_dhcpv6::Lifetimes {
        valid_until: renewed_valid_until,
        preferred_until: renewed_preferred_until,
    } = assert_prefix(got_prefixes, config.prefix_low);

    assert!(renewed_valid_until > valid_until, "{renewed_valid_until} is not after {valid_until}");
    assert!(
        renewed_preferred_until > preferred_until,
        "{renewed_preferred_until} is not after {preferred_until}"
    );
}

#[netstack_test]
async fn stateful_alternative_server_while_rebinding(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_network, _realm, iface, guest_controller, addr, client_provider) =
        test_setup(name, &sandbox).await;

    let config1 = Dhcpv6ServerConfig::new(SERVER_ID1, PrefixCount::One);
    let config2 = Dhcpv6ServerConfig::new(SERVER_ID2, PrefixCount::One);
    futures::join!(
        start_server(&guest_controller, config1),
        start_server(&guest_controller, config2)
    );

    let dhcpv6_client = start_dhcpv6_client(
        &client_provider,
        iface.id(),
        addr,
        fnet_dhcpv6_ext::ClientConfig {
            prefix_delegation_config: Some(fnet_dhcpv6::PrefixDelegationConfig::Empty(
                fnet_dhcpv6::Empty,
            )),
            non_temporary_address_config: Default::default(),
            information_config: Default::default(),
        },
    );

    let fnet_dhcpv6::Prefix { prefix: got_prefix, lifetimes: _ } = {
        let got_prefixes = dhcpv6_client.watch_prefixes().await.expect("watching prefixes");
        get_single_prefix(got_prefixes)
    };

    let (current, backup) = if got_prefix == into_fidl_ipv6_address_with_prefix(config1.prefix_low)
    {
        (config1, config2)
    } else if got_prefix == into_fidl_ipv6_address_with_prefix(config2.prefix_low) {
        (config2, config1)
    } else {
        panic!("got unexpected prefix {:?}", got_prefix);
    };
    {
        let cmd = format!("/bin/sh -c '{} stop {:x}'", SERVER_SCRIPT_REMOTE, current.identifier);

        guest_controller.exec_with_output_logged(&cmd, vec![], None).await.expect("exec failed");
    }

    // Assert that the DHCPv6 client obtains a lease from the other DHCPv6
    // server.
    let got_prefixes = dhcpv6_client.watch_prefixes().await.expect("watching prefixes");
    let _: fnet_dhcpv6::Lifetimes = assert_prefix(got_prefixes, backup.prefix_low);
}

#[netstack_test]
#[test_case(PrefixCount::One)]
#[test_case(PrefixCount::Two)]
async fn stateful_client_restart(name: &str, prefix_count: PrefixCount) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_network, _realm, iface, guest_controller, addr, client_provider) =
        test_setup(name, &sandbox).await;

    let config = Dhcpv6ServerConfig::new(SERVER_ID1, prefix_count);
    start_server(&guest_controller, config).await;

    let want_prefix_after_restart = {
        let dhcpv6_client = start_dhcpv6_client(
            &client_provider,
            iface.id(),
            addr,
            fnet_dhcpv6_ext::ClientConfig {
                prefix_delegation_config: Some(fnet_dhcpv6::PrefixDelegationConfig::Empty(
                    fnet_dhcpv6::Empty,
                )),
                non_temporary_address_config: Default::default(),
                information_config: Default::default(),
            },
        );

        // Assert that DHCPv6 client obtains a delegated prefix.
        let got_prefixes = dhcpv6_client.watch_prefixes().await.expect("watching prefixes");
        match prefix_count {
            PrefixCount::One => {
                let _: fnet_dhcpv6::Lifetimes = assert_prefix(got_prefixes, config.prefix_low);
                config.prefix_low
            }
            PrefixCount::Two => {
                let fnet_dhcpv6::Prefix { prefix, lifetimes: _ } = get_single_prefix(got_prefixes);
                if prefix == into_fidl_ipv6_address_with_prefix(config.prefix_low) {
                    config.prefix_high
                } else if prefix == into_fidl_ipv6_address_with_prefix(config.prefix_high) {
                    config.prefix_low
                } else {
                    panic!("unexpected prefix acquired: {:?}", prefix);
                }
            }
        }
    };

    {
        let dhcpv6_client = start_dhcpv6_client(
            &client_provider,
            iface.id(),
            addr,
            fnet_dhcpv6_ext::ClientConfig {
                prefix_delegation_config: Some(fnet_dhcpv6::PrefixDelegationConfig::Empty(
                    fnet_dhcpv6::Empty,
                )),
                non_temporary_address_config: Default::default(),
                information_config: Default::default(),
            },
        );

        // Assert that DHCPv6 client obtains a delegated prefix.
        let got_prefixes = dhcpv6_client.watch_prefixes().await.expect("watching prefixes");
        let _: fnet_dhcpv6::Lifetimes = assert_prefix(got_prefixes, want_prefix_after_restart);
    }
}
