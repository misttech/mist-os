// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use assert_matches::assert_matches;
use fidl::endpoints;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp::{
    self as fnet_dhcp, ClientEvent, ClientExitReason, ClientMarker, ClientProviderMarker,
    NewClientParams,
};
use fidl_fuchsia_net_ext::{self as fnet_ext, IntoExt as _};
use fidl_fuchsia_netemul_network as fnetemul_network;
use futures::{join, FutureExt, TryStreamExt};
use netstack_testing_common::{
    dhcpv4 as dhcpv4_helper,
    realms::{KnownServiceProvider, Netstack, TestSandboxExt as _},
};
use netstack_testing_macros::netstack_test;

const MAC: net_types::ethernet::Mac = net_declare::net_mac!("00:00:00:00:00:01");
const SERVER_MAC: net_types::ethernet::Mac = net_declare::net_mac!("02:02:02:02:02:02");

struct DhcpTestRealm<'a> {
    client_realm: netemul::TestRealm<'a>,
    client_iface: netemul::TestInterface<'a>,
    server_realm: netemul::TestRealm<'a>,
    server_iface: netemul::TestInterface<'a>,
    _network: netemul::TestNetwork<'a>,
}

impl<'a> DhcpTestRealm<'a> {
    async fn start_dhcp_server(&self) {
        let Self { server_realm, server_iface, client_realm: _, client_iface: _, _network: _ } =
            self;

        server_iface
            .add_address_and_subnet_route(
                dhcpv4_helper::DEFAULT_TEST_CONFIG.server_addr_with_prefix().into_ext(),
            )
            .await
            .expect("add address should succeed");

        let server_proxy = server_realm
            .connect_to_protocol::<fnet_dhcp::Server_Marker>()
            .expect("connect to Server_ protocol should succeed");

        dhcpv4_helper::set_server_settings(
            &server_proxy,
            dhcpv4_helper::DEFAULT_TEST_CONFIG.dhcp_parameters().into_iter().chain([
                fnet_dhcp::Parameter::BoundDeviceNames(vec![server_iface
                    .get_interface_name()
                    .await
                    .expect("get interface name should succeed")]),
            ]),
            [],
        )
        .await;

        server_proxy
            .start_serving()
            .await
            .expect("start_serving should not encounter FIDL error")
            .expect("start_serving should succeed");
    }
}

async fn create_test_realm<'a, N: Netstack>(
    sandbox: &'a netemul::TestSandbox,
    name: &'a str,
) -> DhcpTestRealm<'a> {
    let network =
        sandbox.create_network("dhcp-test-network").await.expect("create network should succeed");
    let client_realm: netemul::TestRealm<'_> = sandbox
        .create_netstack_realm_with::<N, _, _>(
            format!("client-realm-{name}"),
            &[KnownServiceProvider::DhcpClient],
        )
        .expect("create realm should succeed");

    let client_iface = client_realm
        .join_network_with(
            &network,
            "clientiface",
            fnetemul_network::EndpointConfig {
                mtu: netemul::DEFAULT_MTU,
                mac: Some(Box::new(fnet_ext::MacAddress { octets: MAC.bytes() }.into())),
            },
            netemul::InterfaceConfig { name: Some("clientiface".into()), metric: None },
        )
        .await
        .expect("join network with realm should succeed");

    let server_realm: netemul::TestRealm<'_> = sandbox
        .create_netstack_realm_with::<netstack_testing_common::realms::Netstack2, _, _>(
            "server-realm",
            &[KnownServiceProvider::DhcpServer { persistent: false }],
        )
        .expect("create realm should succeed");

    let server_iface = server_realm
        .join_network_with(
            &network,
            "serveriface",
            fnetemul_network::EndpointConfig {
                mtu: netemul::DEFAULT_MTU,
                mac: Some(Box::new(fnet_ext::MacAddress { octets: SERVER_MAC.bytes() }.into())),
            },
            netemul::InterfaceConfig { name: Some("serveriface".into()), metric: None },
        )
        .await
        .expect("join network with realm should succeed");

    DhcpTestRealm { client_realm, client_iface, server_realm, server_iface, _network: network }
}

#[netstack_test]
async fn client_provider_two_overlapping_clients_on_same_interface<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();
    let DhcpTestRealm { client_realm, client_iface, server_realm: _, server_iface: _, _network: _ } =
        &create_test_realm::<N>(&sandbox, name).await;

    let proxy = client_realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let client_iface = &client_iface;

    let (client_a, server_end_a) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
    let (client_b, server_end_b) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");

    proxy
        .new_client(
            client_iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end_a,
        )
        .expect("creating new client should succeed");

    proxy
        .new_client(
            client_iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end_b,
        )
        .expect("creating new client should succeed");

    let watch_fut_a = client_a.watch_configuration();
    let watch_fut_b = client_b.watch_configuration();
    let (result_a, result_b) = join!(watch_fut_a, watch_fut_b);

    assert_matches!(result_a, Err(fidl::Error::ClientChannelClosed { .. }));
    assert_matches!(result_b, Err(fidl::Error::ClientChannelClosed { .. }));

    let on_exit = client_b
        .take_event_stream()
        .try_next()
        .now_or_never()
        .expect("event should be already available")
        .expect("event stream should not have ended before yielding exit reason")
        .expect("event stream should not have FIDL error");
    assert_matches!(
        on_exit,
        ClientEvent::OnExit { reason: ClientExitReason::ClientAlreadyExistsOnInterface }
    )
}

#[netstack_test]
async fn client_provider_two_non_overlapping_clients_on_same_interface<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

    let DhcpTestRealm { client_realm, client_iface, server_realm: _, server_iface: _, _network: _ } =
        &create_test_realm::<N>(&sandbox, name).await;

    let proxy = client_realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let client_iface = &client_iface;

    let proxy = &proxy;
    // Executing the following block twice demonstrates that we can run
    // and shutdown DHCP clients on the same interface without running
    // afoul of the multiple-clients-on-same-interface restriction.
    for () in [(), ()] {
        let (client, server_end) =
            endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");

        proxy
            .new_client(
                client_iface.id(),
                NewClientParams {
                    configuration_to_request: None,
                    request_ip_address: Some(true),
                    ..NewClientParams::EMPTY
                },
                server_end,
            )
            .expect("creating new client should succeed");

        client.shutdown().expect("shutdown call should not have FIDL error");
        let watch_result = client.watch_configuration().await;

        assert_matches!(watch_result, Err(fidl::Error::ClientChannelClosed { .. }));

        let on_exit = client
            .take_event_stream()
            .try_next()
            .now_or_never()
            .expect("event should be already available")
            .expect("event stream should not have ended before yielding exit reason")
            .expect("event stream should not have FIDL error");
        assert_matches!(
            on_exit,
            ClientEvent::OnExit { reason: ClientExitReason::GracefulShutdown }
        );
    }
}

#[netstack_test]
async fn client_provider_double_watch<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

    let DhcpTestRealm { client_realm, client_iface, server_realm: _, server_iface: _, _network: _ } =
        &create_test_realm::<N>(&sandbox, name).await;

    let proxy = client_realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let client_iface = &client_iface;

    let (client, server_end) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
    proxy
        .new_client(
            client_iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end,
        )
        .expect("new client");

    let watch_fut_a = client.watch_configuration();
    let watch_fut_b = client.watch_configuration();
    let (result_a, result_b) = join!(watch_fut_a, watch_fut_b);

    assert_matches!(result_a, Err(_));
    assert_matches!(result_b, Err(_));

    let on_exit = client
        .take_event_stream()
        .try_next()
        .now_or_never()
        .expect("event should be already available")
        .expect("event stream should not have ended before yielding exit reason")
        .expect("event stream should not have FIDL error");
    assert_matches!(
        on_exit,
        ClientEvent::OnExit { reason: ClientExitReason::WatchConfigurationAlreadyPending }
    )
}

#[netstack_test]
async fn client_provider_shutdown<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();

    let DhcpTestRealm { client_realm, client_iface, server_realm: _, server_iface: _, _network: _ } =
        &create_test_realm::<N>(&sandbox, name).await;

    let proxy = client_realm.connect_to_protocol::<ClientProviderMarker>().unwrap();
    let client_iface = &client_iface;

    let (client, server_end) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
    proxy
        .new_client(
            client_iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end,
        )
        .expect("creating new client should succeed");

    let watch_fut = client.watch_configuration();
    let shutdown_fut = async {
        futures_lite::future::yield_now().await;
        client.shutdown().expect("shutdown should not have FIDL error");
    };
    let (watch_result, ()) = join!(watch_fut, shutdown_fut);

    assert_matches!(watch_result, Err(_));

    let on_exit = client
        .take_event_stream()
        .try_next()
        .now_or_never()
        .expect("event should be already available")
        .expect("event stream should not have ended before yielding exit reason")
        .expect("event stream should not have FIDL error");
    assert_matches!(on_exit, ClientEvent::OnExit { reason: ClientExitReason::GracefulShutdown })
}

#[netstack_test]
async fn client_provider_watch_configuration_acquires_lease<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();
    let test_realm @ DhcpTestRealm {
        client_realm,
        client_iface,
        server_realm: _,
        server_iface: _,
        _network: _,
    } = &create_test_realm::<N>(&sandbox, name).await;

    test_realm.start_dhcp_server().await;

    let provider =
        client_realm.connect_to_protocol::<ClientProviderMarker>().expect("connect should succeed");
    let (client, server_end) =
        endpoints::create_proxy::<ClientMarker>().expect("create proxy should succeed");
    provider
        .new_client(
            client_iface.id(),
            NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..NewClientParams::EMPTY
            },
            server_end,
        )
        .expect("creating new client should succeed");

    let watch_response =
        client.watch_configuration().await.expect("watch_configuration should succeed");

    let fnet_dhcp::ClientWatchConfigurationResponse { address, dns_servers, routers, .. } =
        watch_response;

    assert_eq!(dns_servers, None);
    assert_eq!(routers, None);

    let fnet_dhcp::Address { address, .. } =
        address.expect("address should be present in response");
    assert_eq!(
        address,
        Some(fnet::Ipv4AddressWithPrefix {
            addr: net_types::ip::Ipv4Addr::from(
                dhcpv4_helper::DEFAULT_TEST_CONFIG.managed_addrs.pool_range_start
            )
            .into_ext(),
            prefix_len: dhcpv4_helper::DEFAULT_TEST_ADDRESS_POOL_PREFIX_LENGTH.get(),
        })
    );
}
