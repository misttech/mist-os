// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![cfg(test)]

use assert_matches::assert_matches;
use fuchsia_async::{Duration, DurationExt, TimeoutExt};
use futures_util::{AsyncReadExt as _, AsyncWriteExt as _, FutureExt, SinkExt, StreamExt};
use net_declare::{fidl_ip, fidl_subnet};
use net_types::ip::{Ipv4, Ipv6};
use netemul::{RealmTcpListener as _, RealmTcpStream as _, RealmUdpSocket};
use netstack_testing_common::interfaces::TestInterfaceExt;
use netstack_testing_common::realms::{Netstack, Netstack3, TestSandboxExt as _};
use netstack_testing_common::{
    ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use ping::PingError;
use std::num::NonZeroU64;
use test_case::test_case;
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_filter as fnet_filter,
    fidl_fuchsia_net_filter_ext as fnet_filter_ext,
    fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext,
};

enum ForwardingConfig {
    BothDisabled,
    ClientSideEnabled,
    ServerSideEnabled,
    BothEnabled,
}

impl ForwardingConfig {
    fn is_client_enabled(&self) -> bool {
        match self {
            ForwardingConfig::BothDisabled => false,
            ForwardingConfig::ClientSideEnabled => true,
            ForwardingConfig::ServerSideEnabled => false,
            ForwardingConfig::BothEnabled => true,
        }
    }

    fn is_server_enabled(&self) -> bool {
        match self {
            ForwardingConfig::BothDisabled => false,
            ForwardingConfig::ClientSideEnabled => false,
            ForwardingConfig::ServerSideEnabled => true,
            ForwardingConfig::BothEnabled => true,
        }
    }
}

struct SetupConfig {
    client_subnet: fnet::Subnet,
    client_gateway: fnet::IpAddress,
    server_subnet: fnet::Subnet,
    server_gateway: fnet::IpAddress,
    router_client_ip: fnet::Subnet,
    router_server_ip: fnet::Subnet,
    router_client_if_config: fnet_interfaces_admin::Configuration,
    router_server_if_config: fnet_interfaces_admin::Configuration,
}

impl SetupConfig {
    fn ipv4(forwarding: ForwardingConfig) -> SetupConfig {
        let disabled_config = || fnet_interfaces_admin::Configuration::default();
        let enabled_config = || fnet_interfaces_admin::Configuration {
            ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                unicast_forwarding: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let router_client_if_config =
            forwarding.is_client_enabled().then_some(enabled_config()).unwrap_or(disabled_config());
        let router_server_if_config =
            forwarding.is_server_enabled().then_some(enabled_config()).unwrap_or(disabled_config());

        SetupConfig {
            client_subnet: fidl_subnet!("192.168.1.2/24"),
            client_gateway: fidl_ip!("192.168.1.1"),
            server_subnet: fidl_subnet!("192.168.0.2/24"),
            server_gateway: fidl_ip!("192.168.0.1"),
            router_client_ip: fidl_subnet!("192.168.1.1/24"),
            router_server_ip: fidl_subnet!("192.168.0.1/24"),
            router_client_if_config,
            router_server_if_config,
        }
    }

    fn ipv6(forwarding: ForwardingConfig) -> SetupConfig {
        let disabled_config = || fnet_interfaces_admin::Configuration::default();
        let enabled_config = || fnet_interfaces_admin::Configuration {
            ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                unicast_forwarding: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let router_client_if_config =
            forwarding.is_client_enabled().then_some(enabled_config()).unwrap_or(disabled_config());
        let router_server_if_config =
            forwarding.is_server_enabled().then_some(enabled_config()).unwrap_or(disabled_config());

        SetupConfig {
            client_subnet: fidl_subnet!("fd00:0:0:1::2/64"),
            client_gateway: fidl_ip!("fd00:0:0:1::1"),
            server_subnet: fidl_subnet!("fd00:0:0:2::2/64"),
            server_gateway: fidl_ip!("fd00:0:0:2::1"),
            router_client_ip: fidl_subnet!("fd00:0:0:1::1/64"),
            router_server_ip: fidl_subnet!("fd00:0:0:2::1/64"),
            router_client_if_config,
            router_server_if_config,
        }
    }

    // Set up two networks, connected by a router.
    async fn build<'a, N: Netstack>(
        self,
        name: &str,
        sandbox: &'a netemul::TestSandbox,
    ) -> Setup<'a> {
        let SetupConfig {
            client_subnet,
            client_gateway,
            server_subnet,
            server_gateway,
            router_client_ip,
            router_server_ip,
            router_client_if_config,
            router_server_if_config,
        } = self;

        let client_net = sandbox.create_network("client").await.expect("create network");
        let server_net = sandbox.create_network("server").await.expect("create network");
        let client = sandbox
            .create_netstack_realm::<N, _>(format!("{}_client", name))
            .expect("create realm");
        let server = sandbox
            .create_netstack_realm::<N, _>(format!("{}_server", name))
            .expect("create realm");
        let router = sandbox
            .create_netstack_realm::<N, _>(format!("{}_router", name))
            .expect("create realm");

        let client_iface = client
            .join_network(&client_net, "client-ep")
            .await
            .expect("install interface in client netstack");
        client_iface.add_address_and_subnet_route(client_subnet).await.expect("configure address");
        client_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
        let server_iface = server
            .join_network(&server_net, "server-ep")
            .await
            .expect("install interface in server netstack");
        server_iface.add_address_and_subnet_route(server_subnet).await.expect("configure address");
        server_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
        let router_client_iface = router
            .join_network(&client_net, "router-client-ep")
            .await
            .expect("install interface in router netstack");
        router_client_iface
            .add_address_and_subnet_route(router_client_ip)
            .await
            .expect("configure address");
        router_client_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
        let router_server_iface = router
            .join_network(&server_net, "router-server-ep")
            .await
            .expect("install interface in router netstack");
        router_server_iface
            .add_address_and_subnet_route(router_server_ip)
            .await
            .expect("configure address");
        router_server_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");

        client_iface.add_default_route(client_gateway).await.expect("add default route");
        server_iface.add_default_route(server_gateway).await.expect("add default route");

        async fn configure_forwarding(
            interface: &fnet_interfaces_ext::admin::Control,
            config: &fnet_interfaces_admin::Configuration,
        ) {
            let _prev_config: fnet_interfaces_admin::Configuration = interface
                .set_configuration(config)
                .await
                .expect("call set configuration")
                .expect("set interface configuration");
        }
        configure_forwarding(router_client_iface.control(), &router_client_if_config).await;
        configure_forwarding(router_server_iface.control(), &router_server_if_config).await;
        Setup {
            _client_net: client_net,
            _server_net: server_net,
            client,
            server,
            router,
            _client_iface: client_iface,
            _server_iface: server_iface,
            router_client_iface,
            router_server_iface,
        }
    }
}

/// An instantiated test setup based on [`SetupConfig`].
struct Setup<'a> {
    _client_net: netemul::TestNetwork<'a>,
    client: netemul::TestRealm<'a>,
    _client_iface: netemul::TestInterface<'a>,
    _server_net: netemul::TestNetwork<'a>,
    server: netemul::TestRealm<'a>,
    _server_iface: netemul::TestInterface<'a>,
    router: netemul::TestRealm<'a>,
    router_client_iface: netemul::TestInterface<'a>,
    router_server_iface: netemul::TestInterface<'a>,
}

const PORT: u16 = 8080;
const REQUEST: &str = "hello from client";
const RESPONSE: &str = "hello from server";

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(SetupConfig::ipv4(ForwardingConfig::BothEnabled); "ipv4")]
#[test_case(SetupConfig::ipv6(ForwardingConfig::BothEnabled); "ipv6")]
async fn forwarding<N: Netstack>(name: &str, setup_config: SetupConfig) {
    let server_ip = fidl_fuchsia_net_ext::IpAddress::from(setup_config.server_subnet.addr).0;
    let client_ip = fidl_fuchsia_net_ext::IpAddress::from(setup_config.client_subnet.addr).0;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let setup = setup_config.build::<N>(name, &sandbox).await;

    let sockaddr = std::net::SocketAddr::from((server_ip, PORT));

    let client = async {
        let mut stream = fuchsia_async::net::TcpStream::connect_in_realm(&setup.client, sockaddr)
            .await
            .expect("connect to server");
        let request = REQUEST.as_bytes();
        assert_eq!(stream.write(request).await.expect("write to stream"), request.len());
        stream.flush().await.expect("flush stream");

        let mut buffer = [0; 512];
        let read = stream.read(&mut buffer).await.expect("read from stream");
        let response = String::from_utf8_lossy(&buffer[0..read]);
        assert_eq!(response, RESPONSE, "got unexpected response from server: {}", response);
    };

    let listener = fuchsia_async::net::TcpListener::listen_in_realm(&setup.server, sockaddr)
        .await
        .expect("bind to address");
    let server = async {
        let (_listener, mut stream, remote) =
            listener.accept().await.expect("accept incoming connection");
        assert_eq!(remote.ip(), client_ip);
        let mut buffer = [0; 512];
        let read = stream.read(&mut buffer).await.expect("read from stream");
        let request = String::from_utf8_lossy(&buffer[0..read]);
        assert_eq!(request, REQUEST, "got unexpected request from client: {}", request);

        let response = RESPONSE.as_bytes();
        assert_eq!(stream.write(response).await.expect("write to stream"), response.len());
        stream.flush().await.expect("flush stream");
    };

    futures_util::future::join(client, server).await;
}

async fn send_ping_and_wait_response(
    source_realm: &netemul::TestRealm<'_>,
    addr: fnet::IpAddress,
    timeout: Duration,
) -> Option<Result<(), PingError>> {
    async fn inner<I: ping::FuchsiaIpExt>(
        source_realm: &netemul::TestRealm<'_>,
        addr: I::SockAddr,
        timeout: Duration,
    ) -> Option<Result<(), PingError>> {
        const PAYLOAD: [u8; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        const SEQ: u16 = 1;

        let icmp_sock =
            source_realm.icmp_socket::<I>().await.expect("failed to create ICMP socket");
        let (mut sink, mut stream) = ping::new_unicast_sink_and_stream::<I, _, { u16::MAX as usize }>(
            &icmp_sock, &addr, &PAYLOAD,
        );
        sink.send(SEQ).await.expect("failed to send ping");
        stream
            .next()
            .on_timeout(timeout.after_now(), || None)
            .await
            .map(|r| r.map(|seq| assert_eq!(seq, SEQ)))
    }

    const PING_PORT: u16 = 0;
    let sockaddr =
        std::net::SocketAddr::from((fidl_fuchsia_net_ext::IpAddress::from(addr).0, PING_PORT));
    match sockaddr {
        std::net::SocketAddr::V4(a) => Box::pin(inner::<Ipv4>(source_realm, a, timeout)).await,
        std::net::SocketAddr::V6(a) => Box::pin(inner::<Ipv6>(source_realm, a, timeout)).await,
    }
}

/// Sends a single ICMP echo request from `source_realm` to `addr`, expecting it
/// to succeed.
async fn expect_successful_ping(
    source_realm: &netemul::TestRealm<'_>,
    addr: fnet::IpAddress,
    msg: &str,
) {
    assert_matches!(
        send_ping_and_wait_response(source_realm, addr, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT).await,
        Some(Ok(())),
        "{msg}",
    );
}

/// Sends a single ICMP echo request from `source_realm` to `addr`, expecting it
/// to fail.
async fn expect_failed_ping(
    source_realm: &netemul::TestRealm<'_>,
    addr: fnet::IpAddress,
    msg: &str,
) {
    assert_matches!(
        send_ping_and_wait_response(source_realm, addr, ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,).await,
        None,
        "{msg}",
    );
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(SetupConfig::ipv4(ForwardingConfig::BothEnabled), true; "ipv4_with_forwarding")]
#[test_case(SetupConfig::ipv4(ForwardingConfig::BothDisabled), false; "ipv4_without_forwarding")]
#[test_case(SetupConfig::ipv6(ForwardingConfig::BothEnabled), true; "ipv6_with_forwarding")]
#[test_case(SetupConfig::ipv6(ForwardingConfig::BothDisabled), false; "ipv6_without_forwarding")]
async fn ping_other_router_addr<N: Netstack>(
    name: &str,
    setup_config: SetupConfig,
    expect_success: bool,
) {
    let router_client_ip = setup_config.router_client_ip;
    let router_server_ip = setup_config.router_server_ip;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let setup = setup_config.build::<N>(name, &sandbox).await;

    // Each side should be able to ping the router IP in its own network,
    // regardless of if forwarding is enabled.
    expect_successful_ping(
        &setup.client,
        router_client_ip.addr,
        "client ping router's client interface",
    )
    .await;
    expect_successful_ping(
        &setup.server,
        router_server_ip.addr,
        "server ping router's server interface",
    )
    .await;

    // Each side should be able to ping the router IP in the other network, only
    // if forwarding is enabled.
    //
    // Essentially, this verifies that the netstack operates as a weak host when
    // forwarding is enabled, and a strong host otherwise. See
    // https://en.wikipedia.org/wiki/Host_model
    if expect_success {
        expect_successful_ping(
            &setup.client,
            router_server_ip.addr,
            "client ping router's server interface",
        )
        .await;
        expect_successful_ping(
            &setup.server,
            router_client_ip.addr,
            "server ping router's client interface",
        )
        .await;
    } else {
        expect_failed_ping(
            &setup.client,
            router_server_ip.addr,
            "client ping router's server interface",
        )
        .await;
        expect_failed_ping(
            &setup.server,
            router_client_ip.addr,
            "server ping router's client interface",
        )
        .await;
    }
}

#[derive(Debug)]
enum ProbeError {
    SendFailed { _err: std::io::Error },
    RecvTimedOut,
}

/// Returns Ok(() if the sender is able to send a UDP packet to the receiver
/// within the given timeout.
async fn probe_connectivity_with_udp(
    sender: &netemul::TestRealm<'_>,
    send_addr: &fnet::Subnet,
    receiver: &netemul::TestRealm<'_>,
    receive_addr: &fnet::Subnet,
    timeout: Duration,
) -> Result<(), ProbeError> {
    const PORT: u16 = 12345;

    // Create a pair of sender/receiver sockets.
    let send_addr =
        std::net::SocketAddr::from((fidl_fuchsia_net_ext::IpAddress::from(send_addr.addr).0, PORT));
    let send_sock = fuchsia_async::net::UdpSocket::bind_in_realm(sender, send_addr)
        .await
        .expect("bind send sock");
    let receive_addr = std::net::SocketAddr::from((
        fidl_fuchsia_net_ext::IpAddress::from(receive_addr.addr).0,
        PORT,
    ));
    let receive_sock = fuchsia_async::net::UdpSocket::bind_in_realm(receiver, receive_addr)
        .await
        .expect("bind receive sock");

    // Send data and wait for a response.
    const PAYLOAD: [u8; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    match send_sock.send_to(&PAYLOAD, receive_addr).await {
        Ok(written) => assert_eq!(written, PAYLOAD.len()),
        Err(e) => return Err(ProbeError::SendFailed { _err: e }),
    };

    let mut buf = [0u8; 10];
    let result = receive_sock
        .recv_from(&mut buf[..])
        .map(Option::Some)
        .on_timeout(timeout.after_now(), || None)
        .await;
    let (read, from) = match result {
        None => return Err(ProbeError::RecvTimedOut),
        Some(result) => result.expect("receive should succeed"),
    };
    assert_eq!(read, PAYLOAD.len());
    assert_eq!(&buf[..read], &PAYLOAD);
    assert_eq!(from, send_addr);
    Ok(())
}

/// Install a drop filter on the forwarding hook.
async fn install_forwarding_filter(
    controller: &mut fnet_filter_ext::Controller,
    ingress_if: &netemul::TestInterface<'_>,
    egress_if: &netemul::TestInterface<'_>,
    name: &str,
) {
    let namespace = fnet_filter_ext::NamespaceId(name.to_owned());
    let routine =
        fnet_filter_ext::RoutineId { namespace: namespace.clone(), name: name.to_owned() };

    let matchers = fnet_filter_ext::Matchers {
        in_interface: Some(fnet_filter_ext::InterfaceMatcher::Id(
            NonZeroU64::new(ingress_if.id()).unwrap(),
        )),
        out_interface: Some(fnet_filter_ext::InterfaceMatcher::Id(
            NonZeroU64::new(egress_if.id()).unwrap(),
        )),
        ..Default::default()
    };

    controller
        .push_changes(vec![
            fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Namespace(
                fnet_filter_ext::Namespace {
                    id: namespace.clone(),
                    domain: fnet_filter_ext::Domain::AllIp,
                },
            )),
            fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Routine(
                fnet_filter_ext::Routine {
                    id: routine.clone(),
                    routine_type: fnet_filter_ext::RoutineType::Ip(Some(
                        fnet_filter_ext::InstalledIpRoutine {
                            hook: fnet_filter_ext::IpHook::Forwarding,
                            priority: 0,
                        },
                    )),
                },
            )),
            fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Rule(
                fnet_filter_ext::Rule {
                    id: fnet_filter_ext::RuleId { routine: routine.clone(), index: 0 },
                    matchers,
                    action: fnet_filter_ext::Action::Drop,
                },
            )),
        ])
        .await
        .expect("push changes");
    controller.commit().await.expect("commit changes");
}

/// Verify the Weak Host "internal forwarding" behavior for traffic ingressing
/// the netstack.
#[test_case(SetupConfig::ipv4(ForwardingConfig::ClientSideEnabled); "ipv4")]
#[test_case(SetupConfig::ipv6(ForwardingConfig::ClientSideEnabled); "ipv6")]
#[fuchsia_async::run_singlethreaded(test)]
async fn internal_forwarding_ingress(setup_config: SetupConfig) {
    let client_ip = setup_config.client_subnet;
    let server_ip = setup_config.server_subnet;
    let router_client_ip = setup_config.router_client_ip;
    let router_server_ip = setup_config.router_server_ip;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // NB: This test relies on fuchsia.net.filter, which is only implemented
    // for Netstack3.
    let setup = setup_config.build::<Netstack3>("internal_forwarding_ingress", &sandbox).await;

    // The client should be able to send traffic to the router's server side IP
    // but the server should not be able to send traffic to the router's client
    // side IP. This is because only the client side interface has forwarding
    // enabled.
    probe_connectivity_with_udp(
        &setup.client,
        &client_ip,
        &setup.router,
        &router_server_ip,
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
    )
    .await
    .expect("probe should succeed");
    assert_matches!(
        probe_connectivity_with_udp(
            &setup.server,
            &server_ip,
            &setup.router,
            &router_client_ip,
            ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT
        )
        .await,
        Err(ProbeError::RecvTimedOut)
    );

    // A filter dropping traffic forwarded from `router_server_if` to
    // `router_client_if` should have no effect.
    let control = setup
        .router
        .connect_to_protocol::<fnet_filter::ControlMarker>()
        .expect("connect to protocol");
    let mut controller = fnet_filter_ext::Controller::new(
        &control,
        &fnet_filter_ext::ControllerId("internal_forwarding_ingress".to_owned()),
    )
    .await
    .expect("create controller");
    install_forwarding_filter(
        &mut controller,
        &setup.router_server_iface,
        &setup.router_client_iface,
        "wrong_filter",
    )
    .await;
    probe_connectivity_with_udp(
        &setup.client,
        &client_ip,
        &setup.router,
        &router_server_ip,
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
    )
    .await
    .expect("probe should succeed");

    // A filter dropping traffic forwarded from `router_client_if` to
    // `router_server_if` should cause the router to drop the traffic.
    install_forwarding_filter(
        &mut controller,
        &setup.router_client_iface,
        &setup.router_server_iface,
        "right_filter",
    )
    .await;
    assert_matches!(
        probe_connectivity_with_udp(
            &setup.client,
            &client_ip,
            &setup.router,
            &router_server_ip,
            ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT
        )
        .await,
        Err(ProbeError::RecvTimedOut)
    );
}

/// Verify the Weak Host "internal forwarding" behavior for traffic egressing
/// the netstack.
#[test_case(SetupConfig::ipv4(ForwardingConfig::ServerSideEnabled); "ipv4")]
#[test_case(SetupConfig::ipv6(ForwardingConfig::ServerSideEnabled); "ipv6")]
#[fuchsia_async::run_singlethreaded(test)]
async fn internal_forwarding_egress(setup_config: SetupConfig) {
    let client_ip = setup_config.client_subnet;
    let server_ip = setup_config.server_subnet;
    let router_client_ip = setup_config.router_client_ip;
    let router_server_ip = setup_config.router_server_ip;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // NB: This test relies on fuchsia.net.filter, which is only implemented
    // for Netstack3.
    let setup = setup_config.build::<Netstack3>("internal_forwarding_egress", &sandbox).await;

    // The router should be able to send traffic from its server side IP to the
    // client, but the router should not be able to send traffic from its
    // client side IP to the server. This is because only the server side
    // interface has forwarding enabled.
    probe_connectivity_with_udp(
        &setup.router,
        &router_server_ip,
        &setup.client,
        &client_ip,
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
    )
    .await
    .expect("probe should succeed");
    assert_matches!(
        probe_connectivity_with_udp(
            &setup.router,
            &router_client_ip,
            &setup.server,
            &server_ip,
            ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
        )
        .await,
        Err(ProbeError::SendFailed { _err: _ })
    );

    // A filter dropping traffic forwarded from `router_client_if` to
    // `router_server_if` should have no effect.
    let control = setup
        .router
        .connect_to_protocol::<fnet_filter::ControlMarker>()
        .expect("connect to protocol");
    let mut controller = fnet_filter_ext::Controller::new(
        &control,
        &fnet_filter_ext::ControllerId("internal_forwarding_egress".to_owned()),
    )
    .await
    .expect("create controller");
    install_forwarding_filter(
        &mut controller,
        &setup.router_client_iface,
        &setup.router_server_iface,
        "wrong_filter",
    )
    .await;
    probe_connectivity_with_udp(
        &setup.router,
        &router_server_ip,
        &setup.client,
        &client_ip,
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
    )
    .await
    .expect("probe should succeed");

    // A filter dropping traffic forwarded from `router_server_if` to
    // `router_client_if` should cause the router to drop the traffic.
    install_forwarding_filter(
        &mut controller,
        &setup.router_server_iface,
        &setup.router_client_iface,
        "right_filter",
    )
    .await;
    assert_matches!(
        probe_connectivity_with_udp(
            &setup.router,
            &router_server_ip,
            &setup.client,
            &client_ip,
            ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
        )
        .await,
        Err(ProbeError::RecvTimedOut)
    );
}
