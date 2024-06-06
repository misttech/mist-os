// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the NAT hooks.

use std::{num::NonZeroU16, ops::RangeInclusive};

use fidl_fuchsia_net_ext::{self as fnet_ext, IntoExt as _};
use fidl_fuchsia_net_filter_ext::{Action, NatHook};
use heck::SnakeCase as _;
use net_types::{ip::IpAddress as _, Witness as _};
use netstack_testing_macros::netstack_test;
use test_case::test_case;

use crate::{
    ip_hooks::{
        Addrs, ExpectedConnectivity, Ports, Realms, SockAddrs, SocketType as _, Subnets, TestIpExt,
        TestNet, TestRealm, LOW_RULE_PRIORITY, MEDIUM_RULE_PRIORITY,
    },
    matchers::{Matcher, Tcp, Udp},
};

// TODO(https://fxbug.dev/341128580): exercise ICMP once it can be NATed
// correctly.
#[netstack_test]
#[test_case(Tcp)]
#[test_case(Udp)]
async fn redirect_ingress_no_assigned_address<I: net_types::ip::Ip + TestIpExt, M: Matcher>(
    name: &str,
    matcher: M,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let network = sandbox.create_network("net").await.expect("create network");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    let mut net = TestNet::new::<I>(
        &sandbox,
        &network,
        &name,
        None, /* ip_hook */
        Some(NatHook::Ingress),
    )
    .await;

    // Send from the client to server and back; assert that we have two-way
    // connectivity when no filtering has been configured.
    //
    // This has the side effect of completing neighbor resolution on the client
    // for the server, so that we can remove the address assigned to the server
    // and still expect traffic from the client to reach it.
    net.run_test::<I, M::SocketType>(ExpectedConnectivity::TwoWay).await;

    // Remove the server's assigned address. Even though we have a Redirect NAT
    // rule installed, the traffic from the client should not reach the server
    // because Redirect drops the packet when there is no address assigned to
    // the incoming interface.
    net.run_test_with::<I, M::SocketType, _>(
        ExpectedConnectivity::None,
        |TestNet { client: _, server }, addrs, ()| {
            Box::pin(async move {
                let removed = server
                    .interface
                    .del_address_and_subnet_route(I::SERVER_ADDR_WITH_PREFIX)
                    .await
                    .expect("remove address");
                assert!(removed);

                server
                    .install_nat_rule_for_incoming_traffic::<I, _>(
                        LOW_RULE_PRIORITY,
                        &matcher,
                        server.incoming_subnets::<I>(),
                        addrs.client_ports(),
                        Action::Redirect { dst_port: None },
                    )
                    .await;
            })
        },
    )
    .await;
}

fn different_ephemeral_port(port: u16) -> u16 {
    // Grab an arbitrary different port by incrementing the current one.
    let new_port = port.wrapping_add(1);

    // Now map that new port into the ephemeral range.
    const EPHEMERAL_RANGE: RangeInclusive<u16> = 49152..=65535;
    let len = u16::try_from(EPHEMERAL_RANGE.len()).unwrap();
    EPHEMERAL_RANGE.start() + (new_port % len)
}

// TODO(https://fxbug.dev/341128580): exercise ICMP once it can be NATed
// correctly.
#[netstack_test]
#[test_case(Tcp, false; "tcp")]
#[test_case(Udp, false; "udp")]
#[test_case(Tcp, true; "tcp to local port")]
#[test_case(Udp, true; "udp to local port")]
async fn redirect_ingress<I: net_types::ip::Ip + TestIpExt, M: Matcher>(
    name: &str,
    matcher: M,
    change_dst_port: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let network = sandbox.create_network("net").await.expect("create network");
    let name = format!("{name}_{}", format!("{matcher:?}").to_snake_case());

    let mut net = TestNet::new::<I>(
        &sandbox,
        &network,
        &name,
        None, /* ip_hook */
        Some(NatHook::Ingress),
    )
    .await;

    // Install a rule to redirect incoming traffic to the primary address of the
    // incoming interface. This should have no effect on connectivity because
    // the incoming traffic is already destined to this address.
    net.run_test_with::<I, M::SocketType, _>(
        ExpectedConnectivity::TwoWay,
        |TestNet { client: _, server }, addrs, ()| {
            Box::pin(async move {
                server
                    .install_nat_rule_for_incoming_traffic::<I, _>(
                        LOW_RULE_PRIORITY,
                        &matcher,
                        server.incoming_subnets::<I>(),
                        addrs.client_ports(),
                        Action::Redirect { dst_port: None },
                    )
                    .await;
            })
        },
    )
    .await;

    // Now run a similar test, but instead of sending from the client socket to
    // the address the server socket is bound to, send to some other address
    // that is not assigned to the server, and optionally send to a different
    // port than the server's socket is bound to as well.
    //
    // This traffic should be redirected to the server's assigned address (and
    // optionally the local port) and received on the server socket, and the
    // traffic should be NATed such that this is transparent to the client.
    let (sockets, sock_addrs) = M::SocketType::bind_sockets(net.realms(), net.addrs()).await;

    // Replace the existing subnet route on the client's interface with a
    // default route through the server to ensure the traffic will be routed
    // there, even though we are sending to a different address.
    net.client
        .interface
        .del_subnet_route(I::CLIENT_ADDR_WITH_PREFIX)
        .await
        .expect("remove subnet route");
    net.client
        .interface
        .add_default_route(I::SERVER_ADDR_WITH_PREFIX.addr)
        .await
        .expect("add default route through server");

    let original_dst = {
        let fnet_ext::IpAddress(addr) = I::OTHER_ADDR_WITH_PREFIX.addr.into();
        let port = if change_dst_port {
            different_ephemeral_port(sock_addrs.server.port())
        } else {
            sock_addrs.server.port()
        };
        std::net::SocketAddr::new(addr, port)
    };
    let server_port = NonZeroU16::new(sock_addrs.server.port()).unwrap();
    net.server
        .install_nat_rule_for_incoming_traffic::<I, _>(
            MEDIUM_RULE_PRIORITY,
            &matcher,
            Subnets {
                src: I::CLIENT_ADDR_WITH_PREFIX,
                dst: I::OTHER_ADDR_WITH_PREFIX,
                other: I::OTHER_SUBNET,
            },
            Ports { src: sock_addrs.client.port(), dst: original_dst.port() },
            Action::Redirect { dst_port: change_dst_port.then_some(server_port..=server_port) },
        )
        .await;

    M::SocketType::run_test::<I>(
        net.realms(),
        sockets,
        SockAddrs { client: sock_addrs.client, server: original_dst },
        ExpectedConnectivity::TwoWay,
    )
    .await;

    // TODO(https://fxbug.dev/338042280): ensure that SO_ORIGINAL_DST returns
    // the pre-NAT destination on the server socket.
}

// TODO(https://fxbug.dev/341128580): exercise ICMP once it can be NATed
// correctly.
#[netstack_test]
#[test_case(Tcp, false; "tcp")]
#[test_case(Udp, false; "udp")]
#[test_case(Tcp, true; "tcp to local port")]
#[test_case(Udp, true; "udp to local port")]
async fn redirect_local_egress<I: net_types::ip::Ip + TestIpExt, M: Matcher>(
    name: &str,
    matcher: M,
    change_dst_port: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let network = sandbox.create_network("net").await.expect("create network");

    let mut netstack = TestRealm::new::<I>(
        &sandbox,
        &network,
        None, /* ip_hook */
        Some(NatHook::LocalEgress),
        name.to_owned(),
        I::CLIENT_ADDR_WITH_PREFIX,
        I::OTHER_ADDR_WITH_PREFIX,
    )
    .await;

    // Create two sockets, a client socket bound to an address assigned on an
    // interface, and a server socket bound to the loopback address. Send from
    // the client to a different, routable address, and optionally to a
    // different port than the server socket is bound to.
    let (sockets, sock_addrs) = M::SocketType::bind_sockets(
        Realms { client: &netstack.realm, server: &netstack.realm },
        Addrs {
            client: I::CLIENT_ADDR_WITH_PREFIX.addr,
            server: I::LOOPBACK_ADDRESS.get().to_ip_addr().into_ext(),
        },
    )
    .await;

    // Install a rule to redirect outgoing traffic to the loopback address (and
    // optional the server socket's local port).
    //
    // This traffic should be redirected to the loopback address and received by
    // the server, and the traffic should be NATed such that this is transparent
    // to the client.
    let original_dst = {
        let fnet_ext::IpAddress(addr) = I::OTHER_ADDR_WITH_PREFIX.addr.into();
        let port = if change_dst_port {
            different_ephemeral_port(sock_addrs.server.port())
        } else {
            sock_addrs.server.port()
        };
        std::net::SocketAddr::new(addr, port)
    };
    let server_port = NonZeroU16::new(sock_addrs.server.port()).unwrap();
    netstack
        .install_nat_rule_for_outgoing_traffic::<I, _>(
            LOW_RULE_PRIORITY,
            &matcher,
            netstack.outgoing_subnets::<I>(),
            Ports { src: sock_addrs.client.port(), dst: original_dst.port() },
            Action::Redirect { dst_port: change_dst_port.then_some(server_port..=server_port) },
        )
        .await;

    M::SocketType::run_test::<I>(
        Realms { client: &netstack.realm, server: &netstack.realm },
        sockets,
        SockAddrs { client: sock_addrs.client, server: original_dst },
        ExpectedConnectivity::TwoWay,
    )
    .await;

    // TODO(https://fxbug.dev/338042280): ensure that SO_ORIGINAL_DST returns
    // the pre-NAT destination on the server socket.
}
