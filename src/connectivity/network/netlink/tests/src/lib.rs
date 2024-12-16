// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tests for the integration between the netlink worker and netstack FIDL APIs, via a hermetic
//! netemul realm.

use std::collections::HashSet;

use assert_matches::assert_matches;
use fidl::endpoints::Proxy as _;
use fuchsia_async::{self as fasync, TimeoutExt};
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt as _, StreamExt as _};
use ip_test_macro::ip_test;
use linux_uapi::{
    rt_class_t_RT_TABLE_MAIN, rtnetlink_groups_RTNLGRP_IPV4_ROUTE,
    rtnetlink_groups_RTNLGRP_IPV6_ROUTE,
};
use net_types::ip::{Ip, IpVersion, Ipv4, Ipv6};
use netemul::{RealmUdpSocket as _, TestRealm};
use netlink::multicast_groups::ModernGroup;
use netlink_packet_core::{NetlinkMessage, NetlinkPayload, NetlinkSerializable};
use netlink_packet_route::route::{
    RouteAttribute, RouteFlags, RouteHeader, RouteMessage, RouteProtocol, RouteScope, RouteType,
};
use netlink_packet_route::rule::{RuleAction, RuleAttribute, RuleFlags, RuleHeader, RuleMessage};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use netstack_testing_common::realms::{Netstack3, TestSandboxExt};
use netstack_testing_common::{
    ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use test_case::test_matrix;

use fidl_fuchsia_net_routes_ext::admin::FidlRouteAdminIpExt;
use fidl_fuchsia_net_routes_ext::rules::FidlRuleIpExt;
use fidl_fuchsia_net_routes_ext::FidlRouteIpExt;
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_ext as fnet_ext,
    fidl_fuchsia_net_interfaces as fnet_interfaces, fidl_fuchsia_net_root as fnet_root,
    fidl_fuchsia_net_routes as fnet_routes, fidl_fuchsia_net_routes_ext as fnet_routes_ext,
    fidl_fuchsia_posix_socket as fposix_socket,
};

fn connect_to_netlink_protocols_in_realm(
    realm: &TestRealm<'_>,
) -> netlink::NetlinkWorkerDiscoverableProtocols {
    let root_interfaces = realm
        .connect_to_protocol::<fnet_root::InterfacesMarker>()
        .expect("connect to fuchsia.net.root.Interfaces");
    let interfaces_state = realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces");
    let v4_routes_state = realm
        .connect_to_protocol::<<Ipv4 as fnet_routes_ext::FidlRouteIpExt>::StateMarker>()
        .expect("connect to fuchsia.net.routes");
    let v6_routes_state = realm
        .connect_to_protocol::<<Ipv6 as fnet_routes_ext::FidlRouteIpExt>::StateMarker>()
        .expect("connect to fuchsia.net.routes");
    let v4_main_route_table = realm.connect_to_protocol::<
        <Ipv4 as fnet_routes_ext::admin::FidlRouteAdminIpExt>::RouteTableMarker,
    >()
    .expect("connect to fuchsia.net.routes.admin");
    let v6_main_route_table = realm.connect_to_protocol::<
        <Ipv6 as fnet_routes_ext::admin::FidlRouteAdminIpExt>::RouteTableMarker,
    >()
    .expect("connect to fuchsia.net.routes.admin");
    let v4_route_table_provider = realm.connect_to_protocol::<
        <Ipv4 as fnet_routes_ext::admin::FidlRouteAdminIpExt>::RouteTableProviderMarker,
    >()
    .expect("connect to fuchsia.net.routes.admin");
    let v6_route_table_provider = realm.connect_to_protocol::<
        <Ipv6 as fnet_routes_ext::admin::FidlRouteAdminIpExt>::RouteTableProviderMarker,
    >()
    .expect("connect to fuchsia.net.routes.admin");
    let v4_rule_table = realm.connect_to_protocol::<
        <Ipv4 as fnet_routes_ext::rules::FidlRuleAdminIpExt>::RuleTableMarker,
    >()
    .expect("connect to fuchsia.net.routes.admin");
    let v6_rule_table = realm.connect_to_protocol::<
        <Ipv6 as fnet_routes_ext::rules::FidlRuleAdminIpExt>::RuleTableMarker,
    >()
    .expect("connect to fuchsia.net.routes.admin");
    netlink::NetlinkWorkerDiscoverableProtocols {
        root_interfaces,
        interfaces_state,
        v4_routes_state,
        v6_routes_state,
        v4_main_route_table,
        v6_main_route_table,
        v4_route_table_provider,
        v6_route_table_provider,
        v4_rule_table,
        v6_rule_table,
    }
}

struct NoopInterfacesHandler;

impl netlink::interfaces::InterfacesHandler for NoopInterfacesHandler {
    fn handle_new_link(&mut self, _name: &str) {}

    fn handle_deleted_link(&mut self, _name: &str) {}
}

#[derive(Clone, Debug)]
struct SentNetlinkMessage<M> {
    message: NetlinkMessage<M>,
    group: Option<ModernGroup>,
}

#[derive(Clone)]
struct Sender<M>(mpsc::UnboundedSender<M>);

impl<M> Sender<M> {
    fn new_pair() -> (Self, Receiver<M>) {
        let (sender, receiver) = mpsc::unbounded();
        (Self(sender), receiver)
    }
}

impl<M: Clone + Send + Sync + 'static> netlink::messaging::Sender<M>
    for Sender<SentNetlinkMessage<M>>
{
    fn send(&mut self, message: NetlinkMessage<M>, group: Option<ModernGroup>) {
        self.0
            .unbounded_send(SentNetlinkMessage { message, group })
            .expect("should not be disconnected")
    }
}

type Receiver<M> = mpsc::UnboundedReceiver<M>;

enum SenderReceiverProvider {}

impl netlink::messaging::SenderReceiverProvider for SenderReceiverProvider {
    type Sender<M: Clone + NetlinkSerializable + Send + Sync + 'static> =
        Sender<SentNetlinkMessage<M>>;
    type Receiver<M: Send + 'static> = Receiver<NetlinkMessage<M>>;
}

struct NetlinkClient {
    client: netlink::protocol_family::route::NetlinkRouteClient,

    // `sender` and `receiver` do not contain the same types because the netlink worker's `Sender`
    // trait wants to be able to specify which group it's sending to, but its `Receiver` trait
    // just wants to receive messages with no group specification.
    sender: Sender<NetlinkMessage<RouteNetlinkMessage>>,
    receiver: Receiver<SentNetlinkMessage<RouteNetlinkMessage>>,
}

fn add_route_client(netlink: &netlink::Netlink<SenderReceiverProvider>) -> NetlinkClient {
    let (server_sender, client_receiver) = Sender::new_pair();
    let (client_sender, server_receiver) = Sender::new_pair();
    let client = netlink
        .new_route_client(server_sender, server_receiver)
        .expect("should create new client successfully");
    NetlinkClient { client, sender: client_sender, receiver: client_receiver }
}

struct TestPeer<'a> {
    _network: netemul::TestNetwork<'a>,
    main_interface: netemul::TestInterface<'a>,
    peer_realm: netemul::TestRealm<'a>,
    _peer_interface: netemul::TestInterface<'a>,
}

impl<'a> TestPeer<'a> {
    async fn create(
        sandbox: &'a netemul::TestSandbox,
        main_realm: &netemul::TestRealm<'a>,
        name_suffix: &str,
        main_address: fnet::Subnet,
        peer_address: fnet::Subnet,
    ) -> TestPeer<'a> {
        let network = sandbox
            .create_network(format!("network{}", name_suffix))
            .await
            .expect("create network");
        let main_interface = main_realm
            .join_network(&network, format!("ep{}", name_suffix))
            .await
            .expect("join network");
        main_interface.add_address(main_address).await.expect("add address to main");
        let peer_realm = sandbox
            .create_netstack_realm::<Netstack3, _>(format!("netstack{}", name_suffix))
            .expect("create netstack realm");
        let peer_interface = peer_realm
            .join_network(&network, format!("peerep{}", name_suffix))
            .await
            .expect("join network");
        peer_interface
            .add_address_and_subnet_route(peer_address)
            .await
            .expect("add address to peer");
        TestPeer { _network: network, main_interface, peer_realm, _peer_interface: peer_interface }
    }
}

/// Identifies one of multiple possible subnets used during a test. Each subnet has a "main" address
/// (for the local/sending netstack against which we are exercising the netlink worker) and a "peer"
/// address (for the remote/receiving netstack).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestSubnet {
    A,
    B,
    C,
}

const TEST_SUBNET_LENGTH: u8 = 24;
const TEST_SUBNETS: [TestSubnet; 3] = [TestSubnet::A, TestSubnet::B, TestSubnet::C];

impl TestSubnet {
    fn peer_subnet_byte(&self) -> u8 {
        match self {
            Self::A => 1,
            Self::B => 2,
            Self::C => 3,
        }
    }

    fn table_index(&self) -> u8 {
        self.peer_subnet_byte()
    }

    fn mark(&self) -> u32 {
        self.peer_subnet_byte().into()
    }

    fn subnet<I: Ip>(&self) -> fnet::IpAddress {
        match I::VERSION {
            IpVersion::V4 => fnet::IpAddress::Ipv4(fnet::Ipv4Address {
                addr: [192, 168, self.peer_subnet_byte(), 0],
            }),
            IpVersion::V6 => fnet::IpAddress::Ipv6(fnet::Ipv6Address {
                addr: [0x20, 0, self.peer_subnet_byte(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            }),
        }
    }

    fn main_address<I: Ip>(&self) -> fnet::IpAddress {
        match I::VERSION {
            IpVersion::V4 => fnet::IpAddress::Ipv4(fnet::Ipv4Address {
                addr: [192, 168, self.peer_subnet_byte(), 1],
            }),
            IpVersion::V6 => fnet::IpAddress::Ipv6(fnet::Ipv6Address {
                addr: [0x20, 0, self.peer_subnet_byte(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            }),
        }
    }

    fn peer_address<I: Ip>(&self) -> fnet::IpAddress {
        match I::VERSION {
            IpVersion::V4 => fnet::IpAddress::Ipv4(fnet::Ipv4Address {
                addr: [192, 168, self.peer_subnet_byte(), 2],
            }),
            IpVersion::V6 => fnet::IpAddress::Ipv6(fnet::Ipv6Address {
                addr: [0x20, 0, self.peer_subnet_byte(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            }),
        }
    }
}

fn address_family<I: Ip>() -> AddressFamily {
    match I::VERSION {
        IpVersion::V4 => AddressFamily::Inet,
        IpVersion::V6 => AddressFamily::Inet6,
    }
}

fn route_group<I: Ip>() -> ModernGroup {
    ModernGroup(match I::VERSION {
        IpVersion::V4 => rtnetlink_groups_RTNLGRP_IPV4_ROUTE,
        IpVersion::V6 => rtnetlink_groups_RTNLGRP_IPV6_ROUTE,
    })
}

fn create_route_in_table<I: Ip>(
    table: u8,
    test_subnet: TestSubnet,
    interface_id: u32,
) -> RouteMessage {
    let mut route_message = RouteMessage::default();
    route_message.header = RouteHeader {
        address_family: address_family::<I>(),
        destination_prefix_length: TEST_SUBNET_LENGTH,
        source_prefix_length: 0,
        tos: 0,
        table,
        protocol: RouteProtocol::Kernel,
        scope: RouteScope::Universe,
        kind: RouteType::Unicast,
        flags: RouteFlags::empty(),
    };

    let fnet_ext::IpAddress(destination) = test_subnet.subnet::<I>().into();

    route_message.attributes.extend([
        RouteAttribute::Destination(destination.into()),
        RouteAttribute::Oif(interface_id),
        RouteAttribute::Priority(1),
    ]);
    route_message
}

async fn add_route_and_await_installed<I: Ip>(
    client: &mut NetlinkClient,
    test_subnet: TestSubnet,
    interface_id: u32,
) {
    let new_route_message = RouteNetlinkMessage::NewRoute(create_route_in_table::<I>(
        test_subnet.table_index(),
        test_subnet,
        interface_id,
    ));
    let mut message: NetlinkMessage<RouteNetlinkMessage> = new_route_message.into();
    message.finalize();
    client.sender.0.unbounded_send(message).expect("should not be disconnected");

    // We first receive notification of the route being added to the main table
    // (as a temporary hack while PBR support is being rolled out).
    // TODO(https://fxbug.dev/358649849): Remove this once PBR is completely supported.
    let SentNetlinkMessage { message: received_msg, group } =
        client.receiver.next().await.expect("should not be disconnected");
    assert_eq!(group, Some(route_group::<I>()));
    let received_route_message =
        assert_matches!(received_msg.payload, NetlinkPayload::InnerMessage(message) => message);

    assert_eq!(
        received_route_message,
        RouteNetlinkMessage::NewRoute(create_route_in_table::<I>(
            rt_class_t_RT_TABLE_MAIN as u8,
            test_subnet,
            interface_id
        ))
    );

    // We then receive notification of the route being added to the table we actually requested.
    let SentNetlinkMessage { message: received_msg, group } =
        client.receiver.next().await.expect("should not be disconnected");
    assert_eq!(group, Some(route_group::<I>()));
    let received_route_message =
        assert_matches!(received_msg.payload, NetlinkPayload::InnerMessage(message) => message);

    assert_eq!(
        received_route_message,
        RouteNetlinkMessage::NewRoute(create_route_in_table::<I>(
            test_subnet.table_index(),
            test_subnet,
            interface_id
        ))
    );
}

#[ip_test(I)]
#[fuchsia::test]
async fn rules_select_correct_table_for_marked_socket<I: Ip>() {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let main_realm = sandbox
        .create_netstack_realm::<Netstack3, _>(format!("main-netstack"))
        .expect("create realm");

    let protocols = connect_to_netlink_protocols_in_realm(&main_realm);
    let (on_initialized, initialized) = oneshot::channel();
    let (netlink, worker_fut) =
        netlink::Netlink::<SenderReceiverProvider>::new_from_protocol_connections(
            NoopInterfacesHandler,
            protocols,
            on_initialized,
        );
    let _join_handle = fasync::Task::spawn(worker_fut);
    initialized.await.expect("should not be dropped");

    let create_peer = |test_subnet: TestSubnet| {
        let sandbox = &sandbox;
        let main_realm = &main_realm;
        async move {
            let name_suffix = format!("{test_subnet:?}");
            let peer = TestPeer::create(
                sandbox,
                main_realm,
                name_suffix.as_str(),
                fnet::Subnet {
                    prefix_len: TEST_SUBNET_LENGTH,
                    addr: test_subnet.main_address::<I>(),
                },
                fnet::Subnet {
                    prefix_len: TEST_SUBNET_LENGTH,
                    addr: test_subnet.peer_address::<I>(),
                },
            )
            .await;
            peer
        }
    };

    let test_peers = futures::stream::iter(TEST_SUBNETS)
        .then(|test_subnet| create_peer(test_subnet).map(move |peer| (test_subnet, peer)))
        .collect::<Vec<_>>()
        .await;

    let mut client = add_route_client(&netlink);
    client.client.add_membership(route_group::<I>()).expect("should add membership successfully");

    for &(test_subnet, ref peer) in &test_peers {
        add_route_and_await_installed::<I>(
            &mut client,
            test_subnet,
            peer.main_interface.id().try_into().unwrap(),
        )
        .await;
    }

    // We install a default rule to drop all traffic, which helps us prove we only see traffic if
    // sockets are correctly marked.
    let new_rule_message = RouteNetlinkMessage::NewRule({
        let mut rule_message = RuleMessage::default();
        rule_message.header = RuleHeader {
            family: address_family::<I>(),
            dst_len: 0,
            src_len: 0,
            tos: 0,
            table: 0,
            action: RuleAction::Unreachable,
            flags: RuleFlags::empty(),
        };
        rule_message.attributes.extend([
            RuleAttribute::Priority(10),
            RuleAttribute::FwMark(0),
            RuleAttribute::FwMask(0),
        ]);
        rule_message
    });
    let mut message: NetlinkMessage<RouteNetlinkMessage> = new_rule_message.into();
    message.finalize();
    client.sender.0.unbounded_send(message).expect("should not be disconnected");

    for &(test_subnet, ref _peer) in &test_peers {
        let new_rule_message = RouteNetlinkMessage::NewRule({
            let mut rule_message = RuleMessage::default();
            rule_message.header = RuleHeader {
                family: address_family::<I>(),
                dst_len: 0,
                src_len: 0,
                tos: 0,
                table: test_subnet.table_index(),
                action: RuleAction::ToTable,
                flags: RuleFlags::empty(),
            };
            rule_message.attributes.extend([
                RuleAttribute::Priority(9),
                RuleAttribute::FwMark(test_subnet.mark()),
                RuleAttribute::FwMask(u32::MAX),
            ]);
            rule_message
        });
        let mut message: NetlinkMessage<RouteNetlinkMessage> = new_rule_message.into();
        message.finalize();
        client.sender.0.unbounded_send(message).expect("should not be disconnected");
    }

    let provider = main_realm
        .connect_to_protocol::<fposix_socket::ProviderMarker>()
        .expect("connect to fuchsia.posix.socket.Provider");

    let create_socket_with_fwmark = |fwmark: Option<u32>| {
        let provider = &provider;
        async move {
            let response = provider
                .datagram_socket(
                    match I::VERSION {
                        IpVersion::V4 => fposix_socket::Domain::Ipv4,
                        IpVersion::V6 => fposix_socket::Domain::Ipv6,
                    },
                    fposix_socket::DatagramSocketProtocol::Udp,
                )
                .await
                .expect("should not have FIDL error")
                .expect("should not get error");
            let socket_proxy = match response {
                fposix_socket::ProviderDatagramSocketResponse::SynchronousDatagramSocket(
                    client_end,
                ) => client_end.into_proxy(),
                _ => unreachable!("netstack3 does not implement fast UDP yet"),
            };

            if let Some(fwmark) = fwmark {
                socket_proxy
                    .set_mark(
                        fposix_socket::MarkDomain::Mark1,
                        &fposix_socket::OptionalUint32::Value(fwmark),
                    )
                    .await
                    .expect("should not get FIDL error")
                    .expect("should not get error");
            }

            let socket = socket2::Socket::from(
                fdio::create_fd(
                    socket_proxy
                        .into_client_end()
                        .expect("should successfully get back client end")
                        .into(),
                )
                .expect("create fd should succeed"),
            );
            fasync::net::UdpSocket::from_datagram(
                fasync::net::DatagramSocket::new_from_socket(socket)
                    .expect("should successfully create async socket from socket2 socket"),
            )
            .expect("should successfully create UDP socket")
        }
    };

    let peer_sockets = futures::stream::iter(test_peers.iter())
        .then(|&(test_subnet, ref peer)| async move {
            let fnet_ext::IpAddress(peer_addr) = test_subnet.peer_address::<I>().into();
            let socket_addr = std::net::SocketAddr::new(peer_addr, 1234);
            let socket = fasync::net::UdpSocket::bind_in_realm(&peer.peer_realm, socket_addr)
                .await
                .expect("should successfully bind UDP socket");
            (test_subnet, socket_addr, socket)
        })
        .collect::<Vec<_>>()
        .await;

    // A 0-marked socket should not be able to reach any of the peers.
    let socket = create_socket_with_fwmark(Some(0)).await;

    for &(_test_subnet, peer_addr, ref _peer_socket) in &peer_sockets {
        let message = format!("hello {peer_addr:?}");
        let result = socket.send_to(message.as_bytes(), peer_addr).await;
        let error = assert_matches!(result, Err(e) => e);
        assert_matches!(error.kind(), std::io::ErrorKind::NetworkUnreachable);
    }

    // A socket with the corresponding mark should be able to reach each of the peers, but not the
    // others. Also, a socket with no mark should be able to reach all of the peers, due to the
    // main-table hack.
    // TODO(https://fxbug.dev/358649849): Stop expecting success in the no-mark case once PBR is
    // fully supported.
    let mut buf = [0u8; 64];
    for &(test_subnet, peer_addr, ref peer_socket) in &peer_sockets {
        for mark in TEST_SUBNETS.into_iter().map(|test_subnet| Some(test_subnet.mark())).chain(None)
        {
            let socket = create_socket_with_fwmark(mark).await;

            let expect_success = mark.map(|mark| mark == test_subnet.mark()).unwrap_or(true);

            let message = format!("hello {peer_addr:?} from mark {mark:?}");
            let result = socket.send_to(message.as_bytes(), peer_addr).await;
            if expect_success {
                let (n, _from_addr) =
                    peer_socket.recv_from(&mut buf).await.expect("should successfully receive");
                assert_eq!(&buf[..n], message.as_bytes());
            } else {
                let error = assert_matches!(result, Err(e) => e);
                assert_matches!(error.kind(), std::io::ErrorKind::NetworkUnreachable);
            }
        }
    }
}

#[ip_test(I)]
#[fuchsia::test]
async fn successfully_installs_rule_referencing_main_table<
    I: Ip + FidlRuleIpExt + FidlRouteIpExt + FidlRouteAdminIpExt,
>() {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let main_realm = sandbox
        .create_netstack_realm::<Netstack3, _>(format!("main-netstack"))
        .expect("create realm");

    let protocols = connect_to_netlink_protocols_in_realm(&main_realm);
    let (on_initialized, initialized) = oneshot::channel();
    let (netlink, worker_fut) =
        netlink::Netlink::<SenderReceiverProvider>::new_from_protocol_connections(
            NoopInterfacesHandler,
            protocols,
            on_initialized,
        );
    let _join_handle = fasync::Task::spawn(worker_fut);
    initialized.await.expect("should not be dropped");

    let client = add_route_client(&netlink);

    let state = &main_realm.connect_to_protocol::<I::StateMarker>().expect("connect to protocol");
    let mut rules_event_stream =
        std::pin::pin!(fnet_routes_ext::rules::rule_event_stream_from_state::<I>(state)
            .expect("get rule watcher"));
    let _preexisting_rules = fnet_routes_ext::rules::collect_rules_until_idle::<I, HashSet<_>>(
        rules_event_stream.by_ref(),
    )
    .await
    .expect("collect rules until idle");

    fn address_family<I: Ip>() -> AddressFamily {
        match I::VERSION {
            IpVersion::V4 => AddressFamily::Inet,
            IpVersion::V6 => AddressFamily::Inet6,
        }
    }

    const MARK: u32 = 1234;
    const PRIORITY: u32 = 5678;

    let create_rule_to_main_table = || {
        let mut rule_message = RuleMessage::default();
        rule_message.header = RuleHeader {
            family: address_family::<I>(),
            dst_len: 0,
            src_len: 0,
            tos: 0,
            table: rt_class_t_RT_TABLE_MAIN as u8,
            action: RuleAction::ToTable,
            flags: RuleFlags::empty(),
        };
        rule_message.attributes.extend([
            RuleAttribute::Priority(PRIORITY),
            RuleAttribute::FwMark(MARK),
            RuleAttribute::FwMask(u32::MAX),
        ]);
        rule_message
    };

    // Install one rule referencing the table.
    let mut new_rule_message: NetlinkMessage<_> =
        RouteNetlinkMessage::NewRule(create_rule_to_main_table()).into();
    new_rule_message.finalize();
    client.sender.0.unbounded_send(new_rule_message).expect("should not be disconnected");

    // Await the rule's installation.
    let rule_event: fnet_routes_ext::rules::RuleEvent<I> = rules_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");

    let added_rule =
        assert_matches!(rule_event, fnet_routes_ext::rules::RuleEvent::Added(rule) => rule);
    // Assume that if the marks match, this is the corresponding rule.
    let (mark_range, table) = assert_matches!(added_rule,
        fnet_routes_ext::rules::InstalledRule {
            matcher: fnet_routes_ext::rules::RuleMatcher {
                mark_1: Some(
                    fnet_routes_ext::rules::MarkMatcher::Marked { mask: u32::MAX, between }
                ),
                ..
            },
            action: fnet_routes_ext::rules::RuleAction::Lookup(table),
            ..
    } => (between, table));

    assert_eq!(mark_range, MARK..=MARK);

    // Check that the rule does actually target the main table.
    let main_table_proxy =
        main_realm.connect_to_protocol::<I::RouteTableMarker>().expect("connect to protocol");
    let main_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&main_table_proxy).await.expect("should succeed");
    assert_eq!(main_table_id, table);
}

async fn await_disappearance_of_table(
    routes_state: &fnet_routes::StateProxy,
    table: fnet_routes_ext::TableId,
    table_name: &str,
) {
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

    loop {
        let get_name_result = routes_state
            .get_route_table_name(table.get())
            .await
            .expect("should not get FIDL error");

        match get_name_result {
            Ok(name) => {
                assert_eq!(&name, table_name);
                fasync::Timer::new(POLL_INTERVAL).await;
            }
            Err(fnet_routes::StateGetRouteTableNameError::NoTable) => break,
        }
    }
}

#[ip_test(I)]
#[fuchsia::test]
async fn route_table_kept_alive_by_rules<I: Ip + FidlRuleIpExt + FidlRouteIpExt>() {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let main_realm = sandbox
        .create_netstack_realm::<Netstack3, _>(format!("main-netstack"))
        .expect("create realm");

    let protocols = connect_to_netlink_protocols_in_realm(&main_realm);
    let (on_initialized, initialized) = oneshot::channel();
    let (netlink, worker_fut) =
        netlink::Netlink::<SenderReceiverProvider>::new_from_protocol_connections(
            NoopInterfacesHandler,
            protocols,
            on_initialized,
        );
    let _join_handle = fasync::Task::spawn(worker_fut);
    initialized.await.expect("should not be dropped");

    let client = add_route_client(&netlink);

    let state = &main_realm.connect_to_protocol::<I::StateMarker>().expect("connect to protocol");
    let mut rules_event_stream =
        std::pin::pin!(fnet_routes_ext::rules::rule_event_stream_from_state::<I>(state)
            .expect("get rule watcher"));
    let _preexisting_rules = fnet_routes_ext::rules::collect_rules_until_idle::<I, HashSet<_>>(
        rules_event_stream.by_ref(),
    )
    .await
    .expect("collect rules until idle");

    fn address_family<I: Ip>() -> AddressFamily {
        match I::VERSION {
            IpVersion::V4 => AddressFamily::Inet,
            IpVersion::V6 => AddressFamily::Inet6,
        }
    }

    const TABLE: u8 = 42;
    const MARK: u32 = 1234;
    const PRIORITIES: [u32; 2] = [11, 12];

    let create_rule_to_table = |priority| {
        let mut rule_message = RuleMessage::default();
        rule_message.header = RuleHeader {
            family: address_family::<I>(),
            dst_len: 0,
            src_len: 0,
            tos: 0,
            table: TABLE,
            action: RuleAction::ToTable,
            flags: RuleFlags::empty(),
        };
        rule_message.attributes.extend([
            RuleAttribute::Priority(priority),
            RuleAttribute::FwMark(MARK),
            RuleAttribute::FwMask(u32::MAX),
        ]);
        rule_message
    };

    // Install one rule referencing the table.
    let mut new_rule_message: NetlinkMessage<_> =
        RouteNetlinkMessage::NewRule(create_rule_to_table(PRIORITIES[0])).into();
    new_rule_message.finalize();
    client.sender.0.unbounded_send(new_rule_message).expect("should not be disconnected");

    // Await the rule's installation.
    let rule_event: fnet_routes_ext::rules::RuleEvent<I> = rules_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");

    let added_rule =
        assert_matches!(rule_event, fnet_routes_ext::rules::RuleEvent::Added(rule) => rule);
    // Assume that if the marks match, this is the corresponding rule.
    let (mark_range, table) = assert_matches!(added_rule, fnet_routes_ext::rules::InstalledRule {
        matcher: fnet_routes_ext::rules::RuleMatcher {
            mark_1: Some(fnet_routes_ext::rules::MarkMatcher::Marked { mask: u32::MAX, between }),
            ..
        },
        action: fnet_routes_ext::rules::RuleAction::Lookup(table),
        ..
    } => (between, table));

    assert_eq!(mark_range, MARK..=MARK);

    // Install another rule referencing the table.
    let mut new_rule_message: NetlinkMessage<_> =
        RouteNetlinkMessage::NewRule(create_rule_to_table(PRIORITIES[1])).into();
    new_rule_message.finalize();
    client.sender.0.unbounded_send(new_rule_message).expect("should not be disconnected");

    // Await the rule's installation.
    let rule_event: fnet_routes_ext::rules::RuleEvent<I> = rules_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");

    let added_rule =
        assert_matches!(rule_event, fnet_routes_ext::rules::RuleEvent::Added(rule) => rule);

    let (mark_range, other_table) = assert_matches!(added_rule,
        fnet_routes_ext::rules::InstalledRule {
            matcher: fnet_routes_ext::rules::RuleMatcher {
                mark_1: Some(
                    fnet_routes_ext::rules::MarkMatcher::Marked { mask: u32::MAX, between }
                ),
                ..
            },
            action: fnet_routes_ext::rules::RuleAction::Lookup(table),
            ..
        } => (between, table)
    );

    assert_eq!(mark_range, MARK..=MARK);
    assert_eq!(table, other_table);

    let routes_state = main_realm
        .connect_to_protocol::<fnet_routes::StateMarker>()
        .expect("connect to fuchsia.net.routes.State");
    let routes_state = &routes_state;

    let netlink_table_name = routes_state
        .get_route_table_name(table.get())
        .await
        .expect("should not get FIDL error")
        .expect("table should be present");
    let netlink_table_name = &netlink_table_name;

    // Check that removing one of the rules does not cause the table to be removed.
    let mut del_rule_message: NetlinkMessage<_> =
        RouteNetlinkMessage::DelRule(create_rule_to_table(PRIORITIES[0])).into();
    del_rule_message.finalize();
    client.sender.0.unbounded_send(del_rule_message).expect("should not be disconnected");

    // Await the rule's removal.
    let rule_event: fnet_routes_ext::rules::RuleEvent<I> = rules_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");

    let removed_rule =
        assert_matches!(rule_event, fnet_routes_ext::rules::RuleEvent::Removed(rule) => rule);
    let (mark_range, removal_referenced_table) = assert_matches!(
        removed_rule,
        fnet_routes_ext::rules::InstalledRule {
            matcher: fnet_routes_ext::rules::RuleMatcher {
                mark_1: Some(
                    fnet_routes_ext::rules::MarkMatcher::Marked { mask: u32::MAX, between }
                ),
                ..
            },
            action: fnet_routes_ext::rules::RuleAction::Lookup(table),
            ..
        } => (between, table)
    );

    assert_eq!(mark_range, MARK..=MARK);
    assert_eq!(removal_referenced_table, table);

    await_disappearance_of_table(routes_state, table, netlink_table_name)
        .map(|()| panic!("table should not be removed"))
        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || ())
        .await;

    // Now if we remove the second rule, the table should be removed.
    let mut del_rule_message: NetlinkMessage<_> =
        RouteNetlinkMessage::DelRule(create_rule_to_table(PRIORITIES[1])).into();
    del_rule_message.finalize();
    client.sender.0.unbounded_send(del_rule_message).expect("should not be disconnected");
    let rule_event: fnet_routes_ext::rules::RuleEvent<I> = rules_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");

    let removed_rule =
        assert_matches!(rule_event, fnet_routes_ext::rules::RuleEvent::Removed(rule) => rule);
    let (mark_range, removal_referenced_table) = assert_matches!(
        removed_rule,
        fnet_routes_ext::rules::InstalledRule {
            matcher: fnet_routes_ext::rules::RuleMatcher {
                mark_1: Some(
                    fnet_routes_ext::rules::MarkMatcher::Marked { mask: u32::MAX, between }
                ),
                ..
            },
            action: fnet_routes_ext::rules::RuleAction::Lookup(table),
            ..
        } => (between, table)
    );

    assert_eq!(mark_range, MARK..=MARK);
    assert_eq!(removal_referenced_table, table);

    await_disappearance_of_table(routes_state, table, netlink_table_name)
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || panic!("table should be removed"))
        .await;
}

#[derive(Debug, Copy, Clone)]
enum Order {
    RuleThenRoute,
    RouteThenRule,
}

#[ip_test(I)]
#[test_matrix(
    [Order::RuleThenRoute, Order::RouteThenRule],
    [Order::RuleThenRoute, Order::RouteThenRule]
)]
#[fuchsia::test]
async fn route_table_is_cleaned_up_after_rules_and_routes_deleted<
    I: Ip + FidlRuleIpExt + FidlRouteIpExt,
>(
    add_order: Order,
    remove_order: Order,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let main_realm = sandbox
        .create_netstack_realm::<Netstack3, _>(format!("main-netstack"))
        .expect("create realm");
    let network = sandbox.create_network("network").await.expect("create network");
    let main_interface = main_realm.join_network(&network, "ep").await.expect("join network");

    let protocols = connect_to_netlink_protocols_in_realm(&main_realm);
    let (on_initialized, initialized) = oneshot::channel();
    let (netlink, worker_fut) =
        netlink::Netlink::<SenderReceiverProvider>::new_from_protocol_connections(
            NoopInterfacesHandler,
            protocols,
            on_initialized,
        );
    let _join_handle = fasync::Task::spawn(worker_fut);
    initialized.await.expect("should not be dropped");

    let mut client = add_route_client(&netlink);
    client.client.add_membership(route_group::<I>()).expect("should add membership successfully");

    fn destination<I: Ip>() -> std::net::IpAddr {
        match I::VERSION {
            IpVersion::V4 => net_declare::std_ip!("192.168.0.0"),
            IpVersion::V6 => net_declare::std_ip!("2000::"),
        }
    }

    let interface_id = u32::try_from(main_interface.id()).unwrap();

    const TABLE: u8 = 42;

    let create_route_in_table = move |table| {
        let mut route_message = RouteMessage::default();
        route_message.header = RouteHeader {
            address_family: address_family::<I>(),
            destination_prefix_length: TEST_SUBNET_LENGTH,
            source_prefix_length: 0,
            tos: 0,
            table,
            protocol: RouteProtocol::Kernel,
            scope: RouteScope::Universe,
            kind: RouteType::Unicast,
            flags: RouteFlags::empty(),
        };
        route_message.attributes.extend([
            RouteAttribute::Destination(destination::<I>().into()),
            RouteAttribute::Oif(interface_id),
            RouteAttribute::Priority(1),
        ]);
        route_message
    };
    let create_route_in_table = &create_route_in_table;

    const MARK: u32 = 1234;

    let create_rule_to_table = || {
        let mut rule_message = RuleMessage::default();
        rule_message.header = RuleHeader {
            family: address_family::<I>(),
            dst_len: 0,
            src_len: 0,
            tos: 0,
            table: TABLE,
            action: RuleAction::ToTable,
            flags: RuleFlags::empty(),
        };
        rule_message.attributes.extend([
            RuleAttribute::Priority(9),
            RuleAttribute::FwMark(MARK),
            RuleAttribute::FwMask(u32::MAX),
        ]);
        rule_message
    };
    let create_rule_to_table = &create_rule_to_table;

    let add_route_and_await_installed = |mut client: NetlinkClient| {
        let mut netlink_route_message: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::NewRoute(create_route_in_table(TABLE)).into();
        netlink_route_message.finalize();

        client.sender.0.unbounded_send(netlink_route_message).expect("should not be disconnected");
        async move {
            // We first receive notification of the route being added to the main table
            // (as a temporary hack while PBR support is being rolled out).
            // TODO(https://fxbug.dev/358649849): Remove this once PBR is completely supported.
            let SentNetlinkMessage { message, group } =
                client.receiver.next().await.expect("should not be disconnected");
            assert_eq!(group, Some(route_group::<I>()));
            let received_route_message =
                assert_matches!(message.payload, NetlinkPayload::InnerMessage(message) => message);
            assert_eq!(
                received_route_message,
                RouteNetlinkMessage::NewRoute(create_route_in_table(
                    rt_class_t_RT_TABLE_MAIN as u8
                ))
            );

            // We then receive notification of the route being added to the table we actually
            // requested.
            let SentNetlinkMessage { message: received_msg, group } =
                client.receiver.next().await.expect("should not be disconnected");
            assert_eq!(group, Some(route_group::<I>()));
            let received_route_message = assert_matches!(received_msg.payload,
                NetlinkPayload::InnerMessage(message) => message
            );

            assert_eq!(
                received_route_message,
                RouteNetlinkMessage::NewRoute(create_route_in_table(TABLE))
            );

            client
        }
    };

    let remove_route_and_await_uninstalled = |mut client: NetlinkClient| {
        let mut netlink_route_message: NetlinkMessage<RouteNetlinkMessage> =
            RouteNetlinkMessage::DelRoute(create_route_in_table(TABLE)).into();
        netlink_route_message.finalize();
        let netlink_route_message = netlink_route_message;

        client.sender.0.unbounded_send(netlink_route_message).expect("should not be disconnected");
        async move {
            // We first receive notification of the route being added to the main table
            // (as a temporary hack while PBR support is being rolled out).
            // TODO(https://fxbug.dev/358649849): Remove this once PBR is completely supported.
            let SentNetlinkMessage { message, group } =
                client.receiver.next().await.expect("should not be disconnected");
            assert_eq!(group, Some(route_group::<I>()));
            let received_route_message =
                assert_matches!(message.payload, NetlinkPayload::InnerMessage(message) => message);
            assert_eq!(
                received_route_message,
                RouteNetlinkMessage::DelRoute(create_route_in_table(
                    rt_class_t_RT_TABLE_MAIN as u8
                ))
            );

            // We then receive notification of the route being added to the table we actually
            // requested.
            let SentNetlinkMessage { message: received_msg, group } =
                client.receiver.next().await.expect("should not be disconnected");
            assert_eq!(group, Some(route_group::<I>()));
            let received_route_message = assert_matches!(received_msg.payload,
                NetlinkPayload::InnerMessage(message) => message
            );

            assert_eq!(
                received_route_message,
                RouteNetlinkMessage::DelRoute(create_route_in_table(TABLE))
            );

            client
        }
    };

    let state = &main_realm.connect_to_protocol::<I::StateMarker>().expect("connect to protocol");
    let mut rules_event_stream =
        std::pin::pin!(fnet_routes_ext::rules::rule_event_stream_from_state::<I>(state)
            .expect("get rule watcher"));
    let _preexisting_rules = fnet_routes_ext::rules::collect_rules_until_idle::<I, HashSet<_>>(
        rules_event_stream.by_ref(),
    )
    .await
    .expect("collect rules until idle");

    let mut routes_event_stream =
        std::pin::pin!(fnet_routes_ext::event_stream_from_state_with_options::<I>(
            state,
            fnet_routes_ext::WatcherOptions {
                table_interest: Some(fnet_routes::TableInterest::All(fnet_routes::All)),
            },
        )
        .expect("get routes watcher"));
    let _preexisting_routes =
        fnet_routes_ext::collect_routes_until_idle::<I, HashSet<_>>(routes_event_stream.by_ref())
            .await
            .expect("collect routes until idle");

    match add_order {
        Order::RouteThenRule => {
            client = add_route_and_await_installed(client).await;
        }
        Order::RuleThenRoute => {}
    }

    // Install the rule.
    let mut new_rule_message: NetlinkMessage<_> =
        RouteNetlinkMessage::NewRule(create_rule_to_table()).into();
    new_rule_message.finalize();
    client.sender.0.unbounded_send(new_rule_message).expect("should not be disconnected");

    // Await the rule's installation.
    let rule_event: fnet_routes_ext::rules::RuleEvent<I> = rules_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");

    let added_rule =
        assert_matches!(rule_event, fnet_routes_ext::rules::RuleEvent::Added(rule) => rule);
    // Assume that if the marks match, this is the corresponding rule.
    let (mark_range, table) = assert_matches!(added_rule, fnet_routes_ext::rules::InstalledRule {
        matcher: fnet_routes_ext::rules::RuleMatcher {
            mark_1: Some(fnet_routes_ext::rules::MarkMatcher::Marked { mask: u32::MAX, between }),
            ..
        },
        action: fnet_routes_ext::rules::RuleAction::Lookup(table),
        ..
    } => (between, table));

    assert_eq!(mark_range, MARK..=MARK);

    match add_order {
        Order::RouteThenRule => {}
        Order::RuleThenRoute => {
            client = add_route_and_await_installed(client).await;
        }
    }

    // Check that this agrees with the routes-watchers view of which table things were installed in.

    // First we expect things to be added to the main table due to the main-table-hack.
    // TODO(https://fxbug.dev/358649849): Remove this once PBR is completely supported.
    let route_event: fnet_routes_ext::Event<I> = routes_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");
    let added_route = assert_matches!(route_event, fnet_routes_ext::Event::Added(route) => route);
    let route_destination = net_types::ip::SubnetEither::from(added_route.route.destination);
    assert_eq!(
        route_destination,
        net_types::ip::SubnetEither::new(destination::<I>().into(), TEST_SUBNET_LENGTH).unwrap()
    );
    let main_table = added_route.table_id;

    // The main table is always a lower ID number than any subsequently-created tables.
    assert!(main_table.get() < table.get());

    // Now we can check for the insertion into the "real" table.
    let route_event: fnet_routes_ext::Event<I> = routes_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");
    let added_route = assert_matches!(route_event, fnet_routes_ext::Event::Added(route) => route);
    let route_destination = net_types::ip::SubnetEither::from(added_route.route.destination);
    assert_eq!(
        route_destination,
        net_types::ip::SubnetEither::new(destination::<I>().into(), TEST_SUBNET_LENGTH).unwrap()
    );
    assert_eq!(added_route.table_id, table);

    let routes_state = main_realm
        .connect_to_protocol::<fnet_routes::StateMarker>()
        .expect("connect to fuchsia.net.routes.State");
    let routes_state = &routes_state;

    let netlink_table_name = routes_state
        .get_route_table_name(added_route.table_id.get())
        .await
        .expect("should not get FIDL error")
        .expect("table should be present");

    match remove_order {
        Order::RouteThenRule => {
            client = remove_route_and_await_uninstalled(client).await;

            // After only removing the route, the table should still be kept alive because there
            // is a rule referencing it.
            let got_name = routes_state
                .get_route_table_name(added_route.table_id.get())
                .await
                .expect("should not get FIDL error")
                .expect("table should still be alive");
            assert_eq!(got_name, netlink_table_name);
        }
        Order::RuleThenRoute => {}
    }

    // Remove the rule.
    let mut del_rule_message: NetlinkMessage<_> =
        RouteNetlinkMessage::DelRule(create_rule_to_table()).into();
    del_rule_message.finalize();
    client.sender.0.unbounded_send(del_rule_message).expect("should not be disconnected");

    // Await the rule's removal.
    let rule_event: fnet_routes_ext::rules::RuleEvent<I> = rules_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");

    let removed_rule =
        assert_matches!(rule_event, fnet_routes_ext::rules::RuleEvent::Removed(rule) => rule);
    // Assume that if the marks match, this is the corresponding rule.
    let (mark_range, removal_referenced_table) = assert_matches!(removed_rule,
        fnet_routes_ext::rules::InstalledRule {
            matcher: fnet_routes_ext::rules::RuleMatcher {
                mark_1: Some(fnet_routes_ext::rules::MarkMatcher::Marked { mask: u32::MAX, between }),
                ..
            },
            action: fnet_routes_ext::rules::RuleAction::Lookup(table),
            ..
        } => (between, table)
    );

    assert_eq!(mark_range, MARK..=MARK);
    assert_eq!(removal_referenced_table, table);

    match remove_order {
        Order::RouteThenRule => {}
        Order::RuleThenRoute => {
            // After only removing the rule, the table should still be kept alive because there
            // is a route referencing it.
            let got_name = routes_state
                .get_route_table_name(added_route.table_id.get())
                .await
                .expect("should not get FIDL error")
                .expect("table should still be alive");
            assert_eq!(got_name, netlink_table_name);

            client = remove_route_and_await_uninstalled(client).await;
        }
    }

    // Check for the route removal(s).
    // First from the main table.
    // TODO(https://fxbug.dev/358649849): Remove this once PBR is completely supported.
    let route_event: fnet_routes_ext::Event<I> = routes_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");
    let removed_route =
        assert_matches!(route_event, fnet_routes_ext::Event::Removed(route) => route);
    let route_destination = net_types::ip::SubnetEither::from(removed_route.route.destination);
    assert_eq!(
        route_destination,
        net_types::ip::SubnetEither::new(destination::<I>().into(), TEST_SUBNET_LENGTH).unwrap()
    );
    assert_eq!(removed_route.table_id, main_table);

    // Now we can check for the removal from the "real" table.
    let route_event: fnet_routes_ext::Event<I> = routes_event_stream
        .next()
        .await
        .expect("should not get FIDL error")
        .expect("should not get watcher error");
    let removed_route =
        assert_matches!(route_event, fnet_routes_ext::Event::Removed(route) => route);
    let route_destination = net_types::ip::SubnetEither::from(removed_route.route.destination);
    assert_eq!(
        route_destination,
        net_types::ip::SubnetEither::new(destination::<I>().into(), TEST_SUBNET_LENGTH).unwrap()
    );
    assert_eq!(removed_route.table_id, table);

    await_disappearance_of_table(routes_state, added_route.table_id, netlink_table_name.as_str())
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || {
            panic!("timed out waiting for route table to be deleted")
        })
        .await;
    let _ = client;
}
