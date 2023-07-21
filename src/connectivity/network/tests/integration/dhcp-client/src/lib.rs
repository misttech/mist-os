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
use fidl_fuchsia_net_dhcp_ext::{self as fnet_dhcp_ext, ClientProviderExt as _};
use fidl_fuchsia_net_ext::{self as fnet_ext, IntoExt as _};
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_netemul_network as fnetemul_network;
use fnet_dhcp_ext::ClientExt;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{join, pin_mut, FutureExt, StreamExt, TryStreamExt};
use netemul::RealmUdpSocket as _;
use netstack_testing_common::{
    annotate, dhcpv4 as dhcpv4_helper,
    realms::{KnownServiceProvider, Netstack, TestSandboxExt as _},
};
use netstack_testing_macros::netstack_test;
use test_case::test_case;

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
        self.start_dhcp_server_with_options([], []).await
    }

    async fn start_dhcp_server_with_options(
        &self,
        parameters: impl IntoIterator<Item = fnet_dhcp::Parameter>,
        options: impl IntoIterator<Item = fnet_dhcp::Option_>,
    ) {
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
            dhcpv4_helper::DEFAULT_TEST_CONFIG
                .dhcp_parameters()
                .into_iter()
                .chain([fnet_dhcp::Parameter::BoundDeviceNames(vec![server_iface
                    .get_interface_name()
                    .await
                    .expect("get interface name should succeed")])])
                .chain(parameters),
            options,
        )
        .await;

        server_proxy
            .start_serving()
            .await
            .expect("start_serving should not encounter FIDL error")
            .expect("start_serving should succeed");
    }

    async fn stop_dhcp_server(&self) {
        let Self { server_realm, server_iface: _, client_realm: _, client_iface: _, _network: _ } =
            self;
        let server_proxy = server_realm
            .connect_to_protocol::<fnet_dhcp::Server_Marker>()
            .expect("connect to Server_ protocol should succeed");
        server_proxy.stop_serving().await.expect("stop_serving should not encounter FIDL error");
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
        .create_netstack_realm_with::<N, _, _>(
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
            &NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..Default::default()
            },
            server_end_a,
        )
        .expect("creating new client should succeed");

    proxy
        .new_client(
            client_iface.id(),
            &NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..Default::default()
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
                &NewClientParams {
                    configuration_to_request: None,
                    request_ip_address: Some(true),
                    ..Default::default()
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
            &NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..Default::default()
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
            &NewClientParams {
                configuration_to_request: None,
                request_ip_address: Some(true),
                ..Default::default()
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

async fn assert_client_shutdown(
    client: fnet_dhcp::ClientProxy,
    address_state_provider: fidl::endpoints::ServerEnd<
        fnet_interfaces_admin::AddressStateProviderMarker,
    >,
) {
    let asp_server_fut = async move {
        let request_stream =
            address_state_provider.into_stream().expect("into_stream should succeed");
        pin_mut!(request_stream);

        let control_handle = assert_matches!(
            request_stream.try_next().await.expect("should succeed").expect("should not have ended"),
            fnet_interfaces_admin::AddressStateProviderRequest::Remove { control_handle } => control_handle,
            "client should explicitly remove address on shutdown"
        );
        control_handle
            .send_on_address_removed(fnet_interfaces_admin::AddressRemovalReason::UserRemoved)
            .expect("should succeed");
    };
    let client_fut = async move {
        // Shut down the client so it won't complain about us dropping the AddressStateProvider
        // without sending a terminal event.
        client.shutdown_ext(client.take_event_stream()).await.expect("shutdown should succeed");
    };

    let ((), ()) = join!(asp_server_fut, client_fut);
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

    let client = provider.new_client_ext(
        client_iface.id().try_into().expect("should be nonzero"),
        fnet_dhcp_ext::default_new_client_params(),
    );

    let config_stream = fnet_dhcp_ext::configuration_stream(client.clone()).fuse();
    pin_mut!(config_stream);

    let fnet_dhcp_ext::Configuration { address, dns_servers, routers } = config_stream
        .try_next()
        .await
        .expect("watch configuration should succeed")
        .expect("configuration stream should not have ended");

    assert_eq!(dns_servers, Vec::new());
    assert_eq!(routers, Vec::new());

    let fnet_dhcp_ext::Address { address, address_parameters: _, address_state_provider } =
        address.expect("address should be present in response");
    assert_eq!(
        address,
        fnet::Ipv4AddressWithPrefix {
            addr: net_types::ip::Ipv4Addr::from(
                dhcpv4_helper::DEFAULT_TEST_CONFIG.managed_addrs.pool_range_start
            )
            .into_ext(),
            prefix_len: dhcpv4_helper::DEFAULT_TEST_ADDRESS_POOL_PREFIX_LENGTH.get(),
        }
    );

    assert_client_shutdown(client, address_state_provider).await;
}

#[netstack_test]
async fn client_explicitly_removes_address_when_lease_expires<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();
    let test_realm @ DhcpTestRealm {
        client_realm,
        client_iface,
        server_realm: _,
        server_iface: _,
        _network: _,
    } = &create_test_realm::<N>(&sandbox, name).await;

    test_realm
        .start_dhcp_server_with_options(
            [fnet_dhcp::Parameter::Lease(fnet_dhcp::LeaseLength {
                default: Some(5), // short enough to expire during this test
                ..Default::default()
            })],
            [],
        )
        .await;

    let provider =
        client_realm.connect_to_protocol::<ClientProviderMarker>().expect("connect should succeed");

    let client = provider.new_client_ext(
        client_iface.id().try_into().expect("should be nonzero"),
        fnet_dhcp_ext::default_new_client_params(),
    );

    let config_stream = fnet_dhcp_ext::configuration_stream(client.clone()).fuse();
    pin_mut!(config_stream);

    let fnet_dhcp_ext::Configuration { address, dns_servers, routers } = config_stream
        .try_next()
        .await
        .expect("watch configuration should succeed")
        .expect("configuration stream should not have ended");

    assert_eq!(dns_servers, Vec::new());
    assert_eq!(routers, Vec::new());

    let fnet_dhcp_ext::Address { address, address_parameters: _, address_state_provider } =
        address.expect("address should be present in response");
    assert_eq!(
        address,
        fnet::Ipv4AddressWithPrefix {
            addr: net_types::ip::Ipv4Addr::from(
                dhcpv4_helper::DEFAULT_TEST_CONFIG.managed_addrs.pool_range_start
            )
            .into_ext(),
            prefix_len: dhcpv4_helper::DEFAULT_TEST_ADDRESS_POOL_PREFIX_LENGTH.get(),
        }
    );

    // Install the address so that the client doesn't error while trying to renew.
    client_iface
        .add_address_and_subnet_route(fnet::Subnet {
            addr: fnet::IpAddress::Ipv4(address.addr),
            prefix_len: address.prefix_len,
        })
        .await
        .expect("should succeed");

    // Stop the DHCP server to prevent it from renewing the lease.
    test_realm.stop_dhcp_server().await;

    // The client should fail to renew and have the lease expire, causing it to
    // remove the address.
    let request_stream = address_state_provider.into_stream().expect("should succeed");
    pin_mut!(request_stream);

    let control_handle = assert_matches!(
        request_stream.try_next().await.expect("should succeed").expect("should not have ended"),
        fnet_interfaces_admin::AddressStateProviderRequest::Remove { control_handle } => control_handle,
        "client should explicitly remove address on shutdown"
    );
    control_handle
        .send_on_address_removed(fnet_interfaces_admin::AddressRemovalReason::UserRemoved)
        .expect("should succeed");
}

const DEBUG_PRINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

#[netstack_test]
async fn watch_configuration_handles_interface_removal<N: Netstack>(name: &str) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();
    let DhcpTestRealm {
        client_realm,
        client_iface,
        server_realm,
        server_iface: _server_iface,
        _network,
    } = create_test_realm::<N>(&sandbox, name).await;

    let provider =
        client_realm.connect_to_protocol::<ClientProviderMarker>().expect("connect should succeed");

    let client = provider.new_client_ext(
        client_iface.id().try_into().expect("should be nonzero"),
        fnet_dhcp_ext::default_new_client_params(),
    );

    let client_fut = async {
        let config_stream = fnet_dhcp_ext::configuration_stream(client.clone()).fuse();
        pin_mut!(config_stream);

        let watch_config_result =
            annotate(config_stream.try_next(), DEBUG_PRINT_INTERVAL, "watch_configuration").await;
        assert_matches!(
            watch_config_result,
            Err(fnet_dhcp_ext::Error::Fidl(fidl::Error::ClientChannelClosed {
                status: zx::Status::PEER_CLOSED,
                protocol_name: _
            }))
        );

        let fnet_dhcp::ClientEvent::OnExit { reason } = client
            .take_event_stream()
            .try_next()
            .await
            .expect("event stream should not have FIDL error")
            .expect("event stream should not have ended");
        assert_eq!(reason, fnet_dhcp::ClientExitReason::InvalidInterface);
    }
    .fuse();

    let sock = fasync::net::UdpSocket::bind_in_realm(
        &server_realm,
        std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            dhcp_protocol::SERVER_PORT.into(),
        )),
    )
    .await
    .expect("bind_in_realm should succeed");
    sock.set_broadcast(true).expect("set_broadcast should succeed");

    let interface_removal_fut = async move {
        // Wait until we see one message from the client before removing the interface.
        let mut buf = [0u8; 1500];
        let (_, client_addr): (usize, std::net::SocketAddr) =
            annotate(sock.recv_from(&mut buf), DEBUG_PRINT_INTERVAL, "recv_from")
                .await
                .expect("recv_from should succeed");

        // The client message will be from the unspecified address.
        assert_eq!(
            client_addr,
            std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                std::net::Ipv4Addr::UNSPECIFIED,
                dhcp_protocol::CLIENT_PORT.into()
            ))
        );

        let _ = client_iface.remove_device();
    }
    .fuse();

    pin_mut!(client_fut, interface_removal_fut);

    let ((), ()) = join!(client_fut, interface_removal_fut);
}

#[netstack_test]
#[test_case(
    Some(fnet_interfaces_admin::AddressRemovalReason::AlreadyAssigned),
    None;
    "should decline and restart if already assigned"
)]
#[test_case(
    Some(fnet_interfaces_admin::AddressRemovalReason::DadFailed),
    None;
    "should decline and restart if duplicate address detected"
)]
#[test_case(
    Some(fnet_interfaces_admin::AddressRemovalReason::UserRemoved),
    Some(fnet_dhcp::ClientExitReason::AddressRemovedByUser);
    "should stop client if user removed"
)]
#[test_case(
    Some(fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved),
    Some(fnet_dhcp::ClientExitReason::InvalidInterface);
    "should stop client if interface removed"
)]
#[test_case(
    None,
    Some(fnet_dhcp::ClientExitReason::AddressStateProviderError);
    "should stop client if address removed with no terminal event"
)]
async fn client_handles_address_removal<N: Netstack>(
    name: &str,
    removal_reason: Option<fnet_interfaces_admin::AddressRemovalReason>,
    expected_exit_reason: Option<fnet_dhcp::ClientExitReason>,
) {
    let sandbox: netemul::TestSandbox = netemul::TestSandbox::new().unwrap();
    let test_realm = create_test_realm::<N>(&sandbox, name).await;

    test_realm.start_dhcp_server().await;

    let DhcpTestRealm {
        client_realm,
        client_iface,
        server_realm: _server_realm,
        server_iface: _server_iface,
        _network,
    } = test_realm;

    let provider =
        client_realm.connect_to_protocol::<ClientProviderMarker>().expect("connect should succeed");

    let client = provider.new_client_ext(
        client_iface.id().try_into().expect("should be nonzero"),
        fnet_dhcp_ext::default_new_client_params(),
    );

    let config_stream = fnet_dhcp_ext::configuration_stream(client.clone()).fuse();
    pin_mut!(config_stream);

    let fnet_dhcp_ext::Configuration { address, dns_servers, routers } = config_stream
        .try_next()
        .await
        .expect("watch configuration should succeed")
        .expect("configuration stream should not have ended");

    assert_eq!(dns_servers, Vec::new());
    assert_eq!(routers, Vec::new());

    let fnet_dhcp_ext::Address { address, address_parameters: _, address_state_provider } =
        address.expect("address should be present in response");
    assert_eq!(
        address,
        fnet::Ipv4AddressWithPrefix {
            addr: net_types::ip::Ipv4Addr::from(
                dhcpv4_helper::DEFAULT_TEST_CONFIG.managed_addrs.pool_range_start
            )
            .into_ext(),
            prefix_len: dhcpv4_helper::DEFAULT_TEST_ADDRESS_POOL_PREFIX_LENGTH.get(),
        }
    );

    {
        let (_asp_request_stream, asp_control_handle) = address_state_provider
            .into_stream_and_control_handle()
            .expect("into stream should succeed");
        // NB: We're acting as the AddressStateProvider server_end.
        // Send the removal reason and close the channel to indicate the address
        // is being removed.
        if let Some(removal_reason) = removal_reason {
            asp_control_handle
                .send_on_address_removed(removal_reason)
                .expect("send removed event should succeed");

            if removal_reason == fnet_interfaces_admin::AddressRemovalReason::InterfaceRemoved {
                let _: (
                    fnetemul_network::EndpointProxy,
                    Option<fnet_interfaces_admin::DeviceControlProxy>,
                ) = client_iface.remove().await.expect("interface removal should succeed");
            }
        }
    }

    if let Some(want_reason) = expected_exit_reason {
        assert_matches!(
            config_stream.try_next().await,
            Err(fnet_dhcp_ext::Error::Fidl(fidl::Error::ClientChannelClosed {
                status: zx::Status::PEER_CLOSED,
                protocol_name: _,
            }))
        );
        let terminal_event = client
            .take_event_stream()
            .try_next()
            .await
            .expect("should not have error on event stream")
            .expect("event stream should not have ended");
        let got_reason = assert_matches!(
            terminal_event,
            fnet_dhcp::ClientEvent::OnExit {
                reason
            } => reason
        );
        assert_eq!(got_reason, want_reason);
        return;
    }

    // After restarting, if the client has not stopped, we should get a new lease.
    let fnet_dhcp_ext::Configuration { address, dns_servers: _, routers: _ } = config_stream
        .try_next()
        .await
        .expect("watch configuration should succeed")
        .expect("configuration stream should not have ended");

    let fnet_dhcp_ext::Address { address, address_parameters: _, address_state_provider } =
        address.expect("address should be present in response");
    assert_eq!(
        address,
        fnet::Ipv4AddressWithPrefix {
            addr: net_types::ip::Ipv4Addr::from(std::net::Ipv4Addr::from(
                u32::from(dhcpv4_helper::DEFAULT_TEST_CONFIG.managed_addrs.pool_range_start) + 1
            ))
            .into_ext(),
            prefix_len: dhcpv4_helper::DEFAULT_TEST_ADDRESS_POOL_PREFIX_LENGTH.get(),
        }
    );

    assert_client_shutdown(client, address_state_provider).await;
}
