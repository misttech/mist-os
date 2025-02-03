// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::num::{NonZeroU16, NonZeroU64};
use std::ops::RangeInclusive;
use std::os::fd::AsFd;
use std::pin::pin;
use std::task::Poll;

use anyhow::{anyhow, Context as _};
use assert_matches::assert_matches;
use async_trait::async_trait;
use fidl_fuchsia_net_ext::{self as fnet_ext, IntoExt as _, IpExt as _};
use fidl_fuchsia_net_routes_ext::{self as fnet_routes_ext};
use fuchsia_async::net::{DatagramSocket, UdpSocket};
use fuchsia_async::{self as fasync, DurationExt, TimeoutExt as _};
use futures::future::{self, LocalBoxFuture};
use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use futures::{Future, FutureExt as _, StreamExt as _, TryFutureExt as _, TryStreamExt as _};
use net_declare::{
    fidl_ip_v4, fidl_ip_v6, fidl_mac, fidl_socket_addr, fidl_subnet, net_addr_subnet, net_ip_v4,
    net_subnet_v4, net_subnet_v6, std_ip, std_ip_v4, std_socket_addr,
};
use net_types::ethernet::Mac;
use net_types::ip::{
    AddrSubnetEither, Ip, IpAddress as _, IpInvariant, IpVersion, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
};
use net_types::Witness as _;
use netemul::{
    InterfaceConfig, RealmTcpListener as _, RealmTcpStream as _, RealmUdpSocket as _,
    TestFakeEndpoint, TestInterface, TestNetwork, TestRealm, TestSandbox,
};
use netstack_testing_common::constants::ipv6 as ipv6_consts;
use netstack_testing_common::interfaces::TestInterfaceExt as _;
use netstack_testing_common::realms::{
    KnownServiceProvider, Netstack, Netstack3, NetstackVersion, TestRealmExt, TestSandboxExt as _,
};
use netstack_testing_common::{
    devices, ndp, ping, Result, ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
    ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use packet::{InnerPacketBuilder as _, ParsablePacket as _, Serializer as _};
use packet_formats::arp::{ArpOp, ArpPacketBuilder};
use packet_formats::ethernet::{
    EtherType, EthernetFrameBuilder, EthernetFrameLengthCheck, ETHERNET_MIN_BODY_LEN_NO_TAG,
};
use packet_formats::icmp::ndp::options::{NdpOptionBuilder, PrefixInformation};
use packet_formats::icmp::ndp::NeighborAdvertisement;
use packet_formats::icmp::{
    IcmpDestUnreachable, IcmpEchoRequest, IcmpPacketBuilder, IcmpZeroCode,
    Icmpv4DestUnreachableCode, Icmpv4Packet, Icmpv6DestUnreachableCode, Icmpv6Packet, MessageBody,
};
use packet_formats::igmp::messages::IgmpPacket;
use packet_formats::ip::{IpProto, Ipv4Proto, Ipv6Proto};
use packet_formats::ipv4::{Ipv4Header as _, Ipv4Packet, Ipv4PacketBuilder};
use packet_formats::ipv6::{Ipv6Header, Ipv6Packet, Ipv6PacketBuilder};
use packet_formats::udp::UdpPacketBuilder;
use sockaddr::{IntoSockAddr as _, PureIpSockaddr, TryToSockaddrLl};
use socket2::{InterfaceIndexOrAddress, SockRef};
use test_case::{test_case, test_matrix};
use zx::{self as zx, AsHandleRef as _};
use {
    fidl_fuchsia_hardware_network as fhardware_network, fidl_fuchsia_net as fnet,
    fidl_fuchsia_net_filter as fnet_filter, fidl_fuchsia_net_filter_ext as fnet_filter_ext,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fidl_fuchsia_net_routes as fnet_routes,
    fidl_fuchsia_net_tun as fnet_tun, fidl_fuchsia_posix as fposix,
    fidl_fuchsia_posix_socket as fposix_socket, fidl_fuchsia_posix_socket_packet as fpacket,
    fidl_fuchsia_posix_socket_raw as fposix_socket_raw,
};

async fn run_udp_socket_test(
    server: &netemul::TestRealm<'_>,
    server_addr: fnet::IpAddress,
    client: &netemul::TestRealm<'_>,
    client_addr: fnet::IpAddress,
) {
    let fnet_ext::IpAddress(client_addr) = fnet_ext::IpAddress::from(client_addr);
    let client_addr = std::net::SocketAddr::new(client_addr, 1234);

    let fnet_ext::IpAddress(server_addr) = fnet_ext::IpAddress::from(server_addr);
    let server_addr = std::net::SocketAddr::new(server_addr, 8080);

    let client_sock = fasync::net::UdpSocket::bind_in_realm(client, client_addr)
        .await
        .expect("failed to create client socket");

    let server_sock = fasync::net::UdpSocket::bind_in_realm(server, server_addr)
        .await
        .expect("failed to create server socket");

    const PAYLOAD: &'static str = "Hello World";

    let client_fut = async move {
        let r = client_sock.send_to(PAYLOAD.as_bytes(), server_addr).await.expect("sendto failed");
        assert_eq!(r, PAYLOAD.as_bytes().len());
    };
    let server_fut = async move {
        let mut buf = [0u8; 1024];
        let (r, from) = server_sock.recv_from(&mut buf[..]).await.expect("recvfrom failed");
        assert_eq!(r, PAYLOAD.as_bytes().len());
        assert_eq!(&buf[..r], PAYLOAD.as_bytes());
        // Unspecified addresses will use loopback as their source
        if client_addr.ip().is_unspecified() {
            assert!(from.ip().is_loopback());
        } else {
            assert_eq!(from, client_addr);
        }
    };

    let ((), ()) = futures::future::join(client_fut, server_fut).await;
}

async fn run_ip_endpoint_packet_socket_test(
    server: &netemul::TestRealm<'_>,
    server_iface_id: u64,
    client: &netemul::TestRealm<'_>,
    client_iface_id: u64,
    ip_version: IpVersion,
    kind: fpacket::Kind,
) {
    async fn new_packet_socket_in_realm(
        realm: &netemul::TestRealm<'_>,
        addr: PureIpSockaddr,
        kind: fpacket::Kind,
    ) -> Result<fasync::net::DatagramSocket> {
        let socket = realm.packet_socket(kind).await.context("creating packet socket")?;
        let sockaddr = libc::sockaddr_ll::from(addr).into_sockaddr();
        let () = socket.bind(&sockaddr).context("binding packet_socket")?;
        let socket = fasync::net::DatagramSocket::new_from_socket(socket)
            .context("wrapping packet socket in fuchsia-async DatagramSocket")?;
        Ok(socket)
    }

    let client_iface_id = NonZeroU64::new(client_iface_id).expect("client iface id is 0");
    let server_iface_id = NonZeroU64::new(server_iface_id).expect("server iface id is 0");

    let client_sock = new_packet_socket_in_realm(
        client,
        PureIpSockaddr { interface_id: Some(client_iface_id), protocol: ip_version },
        kind,
    )
    .await
    .expect("failed to create client socket");

    let server_sock = new_packet_socket_in_realm(
        server,
        PureIpSockaddr { interface_id: Some(server_iface_id), protocol: ip_version },
        kind,
    )
    .await
    .expect("failed to create server socket");

    const PAYLOAD: &'static str = "Hello World";
    let send_to_addr = libc::sockaddr_ll::from(PureIpSockaddr {
        interface_id: Some(client_iface_id),
        protocol: ip_version,
    })
    .into_sockaddr();
    let r = client_sock.send_to(PAYLOAD.as_bytes(), send_to_addr).await.expect("sendto failed");
    assert_eq!(r, PAYLOAD.as_bytes().len());

    let mut buf = [0u8; 1024];
    // Receive from the socket, ignoring all spurious data that may be observed
    // from the network.
    let (recv_len, from) = {
        loop {
            let (recv_len, from) =
                server_sock.recv_from(&mut buf[..]).await.expect("failed to receive");
            match is_packet_spurious(ip_version, &buf[..recv_len]) {
                // NB: IPv4/IPv6 Parse errors are expected, since we're sending
                // "Hello World" and not a valid packet.
                Err(_) | Ok(false) => break (recv_len, from),
                Ok(true) => continue,
            }
        }
    };
    assert_eq!(recv_len, PAYLOAD.as_bytes().len());
    assert_eq!(&buf[..recv_len], PAYLOAD.as_bytes());
    assert_eq!(i32::from(from.family()), libc::AF_PACKET);
    let from = from.try_to_sockaddr_ll().expect("unexpected peer SockAddress type");
    assert_eq!(from.sll_protocol, sockaddr::sockaddr_ll_ip_protocol(ip_version));
    // As defined by Linux in `if_packet.h``.
    const PACKET_HOST: u8 = 0;
    assert_eq!(from.sll_pkttype, PACKET_HOST);
    // IP endpoints don't have a hardware address.
    assert_eq!(from.sll_halen, 0);
    assert_eq!(from.sll_addr, [0, 0, 0, 0, 0, 0, 0, 0]);
}

const CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("192.168.0.2/24");
const SERVER_SUBNET: fnet::Subnet = fidl_subnet!("192.168.0.1/24");
const CLIENT_MAC: fnet::MacAddress = fidl_mac!("02:00:00:00:00:02");
const SERVER_MAC: fnet::MacAddress = fidl_mac!("02:00:00:00:00:01");

enum UdpProtocol {
    Synchronous,
    Fast,
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(
    UdpProtocol::Synchronous, false; "synchronous_protocol not mapped to ipv6")]
#[test_case(
    UdpProtocol::Fast, false; "fast_protocol not mapped to ipv6")]
#[test_case(
    UdpProtocol::Synchronous, true; "synchronous_protocol mapped to ipv6")]
#[test_case(
    UdpProtocol::Fast, true; "fast_protocol mapped to ipv6")]
async fn test_udp_socket<N: Netstack>(name: &str, protocol: UdpProtocol, mapped_to_ipv6: bool) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let _packet_capture = net.start_capture(name).await.expect("starting packet capture");

    let (client, server) = match protocol {
        UdpProtocol::Synchronous => {
            let client = sandbox
                .create_netstack_realm::<N, _>(format!("{}_client", name))
                .expect("failed to create client realm");
            let server = sandbox
                .create_netstack_realm::<N, _>(format!("{}_server", name))
                .expect("failed to create server realm");
            (client, server)
        }
        UdpProtocol::Fast => {
            let version = match N::VERSION {
                NetstackVersion::Netstack2 { tracing, fast_udp: _ } => {
                    NetstackVersion::Netstack2 { tracing, fast_udp: true }
                }
                version => version,
            };
            let client = sandbox
                .create_realm(format!("{}_client", name), [KnownServiceProvider::Netstack(version)])
                .expect("failed to create client realm");
            let server = sandbox
                .create_realm(format!("{}_client", name), [KnownServiceProvider::Netstack(version)])
                .expect("failed to create client realm");
            (client, server)
        }
    };

    let client_ep = client
        .join_network_with(
            &net,
            "client",
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(CLIENT_MAC)),
            Default::default(),
        )
        .await
        .expect("client failed to join network");
    client_ep.add_address_and_subnet_route(CLIENT_SUBNET).await.expect("configure address");
    let server_ep = server
        .join_network_with(
            &net,
            "server",
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(SERVER_MAC)),
            Default::default(),
        )
        .await
        .expect("server failed to join network");
    server_ep.add_address_and_subnet_route(SERVER_SUBNET).await.expect("configure address");

    // Add static ARP entries as we've observed flakes in CQ due to ARP timeouts
    // and ARP resolution is immaterial to this test.
    futures::stream::iter([
        (&server, &server_ep, CLIENT_SUBNET.addr, CLIENT_MAC),
        (&client, &client_ep, SERVER_SUBNET.addr, SERVER_MAC),
    ])
    .for_each_concurrent(None, |(realm, ep, addr, mac)| {
        realm.add_neighbor_entry(ep.id(), addr, mac).map(|r| r.expect("add_neighbor_entry"))
    })
    .await;

    let maybe_map_to_ipv6 = move |orig_addr| match orig_addr {
        fnet::IpAddress::Ipv4(addr) => {
            if mapped_to_ipv6 {
                let addr = net_types::ip::Ipv4Addr::new(addr.addr);
                fnet::IpAddress::Ipv6(fnet::Ipv6Address {
                    addr: addr.to_ipv6_mapped().ipv6_bytes(),
                })
            } else {
                orig_addr
            }
        }
        fnet::IpAddress::Ipv6(_) => {
            unreachable!("SERVER_SUBNET and CLIENT_SUBNET expected to be Ipv4")
        }
    };

    let server_addr = maybe_map_to_ipv6(SERVER_SUBNET.addr);
    let client_addr = maybe_map_to_ipv6(CLIENT_SUBNET.addr);

    run_udp_socket_test(&server, server_addr, &client, client_addr).await
}

enum UdpCacheInvalidationReason {
    ConnectCalled,
    InterfaceDisabled,
    AddressRemoved,
    SetConfigurationCalled,
    RouteRemoved,
    RouteAdded,
}

enum ToAddrExpectation {
    Unspecified,
    Specified(Option<fnet::SocketAddress>),
}

struct UdpSendMsgPreflightSuccessExpectation {
    expected_to_addr: ToAddrExpectation,
    expect_all_eventpairs_valid: bool,
}

enum UdpSendMsgPreflightExpectation {
    Success(UdpSendMsgPreflightSuccessExpectation),
    Failure(fposix::Errno),
}

struct UdpSendMsgPreflight {
    to_addr: Option<fnet::SocketAddress>,
    expected_result: UdpSendMsgPreflightExpectation,
}

async fn setup_fastudp_network<'a>(
    name: &'a str,
    version: NetstackVersion,
    sandbox: &'a netemul::TestSandbox,
    socket_domain: fposix_socket::Domain,
) -> (
    netemul::TestNetwork<'a>,
    netemul::TestRealm<'a>,
    netemul::TestInterface<'a>,
    fposix_socket::DatagramSocketProxy,
) {
    let net = sandbox.create_network("net").await.expect("create network");
    let version = match version {
        NetstackVersion::Netstack2 { tracing, fast_udp: _ } => {
            NetstackVersion::Netstack2 { tracing, fast_udp: true }
        }
        version => version,
    };
    let netstack = sandbox
        .create_realm(name, [KnownServiceProvider::Netstack(version)])
        .expect("create netstack realm");
    let iface = netstack.join_network(&net, "ep").await.expect("failed to join network");

    let socket = {
        let socket_provider = netstack
            .connect_to_protocol::<fposix_socket::ProviderMarker>()
            .expect("connect to socket provider");
        let datagram_socket = socket_provider
            .datagram_socket(socket_domain, fposix_socket::DatagramSocketProtocol::Udp)
            .await
            .expect("call datagram_socket")
            .expect("create datagram socket");
        match datagram_socket {
            fposix_socket::ProviderDatagramSocketResponse::DatagramSocket(socket) => {
                socket.into_proxy()
            }
            socket => panic!("unexpected datagram socket variant: {:?}", socket),
        }
    };

    (net, netstack, iface, socket)
}

fn validate_send_msg_preflight_response(
    response: &fposix_socket::DatagramSocketSendMsgPreflightResponse,
    expectation: UdpSendMsgPreflightSuccessExpectation,
) -> Result {
    let fposix_socket::DatagramSocketSendMsgPreflightResponse {
        to, validity, maximum_size, ..
    } = response;
    let UdpSendMsgPreflightSuccessExpectation { expected_to_addr, expect_all_eventpairs_valid } =
        expectation;

    match expected_to_addr {
        ToAddrExpectation::Specified(to_addr) => {
            assert_eq!(*to, to_addr, "unexpected to address in boarding pass");
        }
        ToAddrExpectation::Unspecified => (),
    }

    const MAXIMUM_UDP_PACKET_SIZE: u32 = 65535;
    const UDP_HEADER_SIZE: u32 = 8;
    assert_eq!(*maximum_size, Some(MAXIMUM_UDP_PACKET_SIZE - UDP_HEADER_SIZE));

    let validity = validity.as_ref().expect("validity was missing");
    assert!(validity.len() > 0, "validity was empty");
    let all_eventpairs_valid = {
        let mut wait_items = validity
            .iter()
            .map(|eventpair| zx::WaitItem {
                handle: eventpair.as_handle_ref(),
                waitfor: zx::Signals::EVENTPAIR_PEER_CLOSED,
                pending: zx::Signals::NONE,
            })
            .collect::<Vec<_>>();
        zx::object_wait_many(&mut wait_items, zx::MonotonicInstant::INFINITE_PAST)
            == Err(zx::Status::TIMED_OUT)
    };
    if expect_all_eventpairs_valid != all_eventpairs_valid {
        return Err(anyhow!(
            "mismatched expectation on eventpair validity: expected {}, got {}",
            expect_all_eventpairs_valid,
            all_eventpairs_valid
        ));
    }
    Ok(())
}

/// Executes a preflight for each of the passed preflight configs, validating
/// the result against the passed expectation and returning all successful responses.
async fn execute_and_validate_preflights(
    preflights: impl IntoIterator<Item = UdpSendMsgPreflight>,
    proxy: &fposix_socket::DatagramSocketProxy,
) -> Vec<fposix_socket::DatagramSocketSendMsgPreflightResponse> {
    futures::stream::iter(preflights)
        .then(|preflight| {
            let UdpSendMsgPreflight { to_addr, expected_result } = preflight;
            let result =
                proxy.send_msg_preflight(&fposix_socket::DatagramSocketSendMsgPreflightRequest {
                    to: to_addr,
                    ..Default::default()
                });
            async move { (expected_result, result.await) }
        })
        .filter_map(|(expected, actual)| async move {
            let actual = actual.expect("send_msg_preflight fidl error");
            match expected {
                UdpSendMsgPreflightExpectation::Success(success_expectation) => {
                    let response = actual.expect("send_msg_preflight failed");
                    validate_send_msg_preflight_response(&response, success_expectation)
                        .expect("validate preflight response");
                    Some(response)
                }
                UdpSendMsgPreflightExpectation::Failure(expected_errno) => {
                    assert_eq!(Err(expected_errno), actual);
                    None
                }
            }
        })
        .collect::<Vec<_>>()
        .await
}

trait UdpSendMsgPreflightTestIpExt: Ip {
    const PORT: u16;
    const SOCKET_DOMAIN: fposix_socket::Domain;
    const INSTALLED_ADDR: fnet::Subnet;
    const REACHABLE_ADDR1: fnet::SocketAddress;
    const REACHABLE_ADDR2: fnet::SocketAddress;
    const UNREACHABLE_ADDR: fnet::SocketAddress;
    const OTHER_SUBNET: fnet::Subnet;

    fn forwarding_config() -> fnet_interfaces_admin::Configuration;
}

impl UdpSendMsgPreflightTestIpExt for net_types::ip::Ipv4 {
    const PORT: u16 = 80;
    const SOCKET_DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv4;
    const INSTALLED_ADDR: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const REACHABLE_ADDR1: fnet::SocketAddress =
        fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress {
            address: fidl_ip_v4!("192.0.2.101"),
            port: Self::PORT,
        });
    const REACHABLE_ADDR2: fnet::SocketAddress =
        fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress {
            address: fidl_ip_v4!("192.0.2.102"),
            port: Self::PORT,
        });
    const UNREACHABLE_ADDR: fnet::SocketAddress =
        fnet::SocketAddress::Ipv4(fnet::Ipv4SocketAddress {
            address: fidl_ip_v4!("198.51.100.1"),
            port: Self::PORT,
        });
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("203.0.113.0/24");

    fn forwarding_config() -> fnet_interfaces_admin::Configuration {
        fnet_interfaces_admin::Configuration {
            ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                unicast_forwarding: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

impl UdpSendMsgPreflightTestIpExt for net_types::ip::Ipv6 {
    const PORT: u16 = 80;
    const SOCKET_DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv6;
    const INSTALLED_ADDR: fnet::Subnet = fidl_subnet!("2001:db8::1/64");
    const REACHABLE_ADDR1: fnet::SocketAddress =
        fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
            address: fidl_ip_v6!("2001:db8::1001"),
            port: Self::PORT,
            zone_index: 0,
        });
    const REACHABLE_ADDR2: fnet::SocketAddress =
        fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
            address: fidl_ip_v6!("2001:db8::1002"),
            port: Self::PORT,
            zone_index: 0,
        });
    const UNREACHABLE_ADDR: fnet::SocketAddress =
        fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
            address: fidl_ip_v6!("2001:db8:ffff:ffff::1"),
            port: Self::PORT,
            zone_index: 0,
        });
    const OTHER_SUBNET: fnet::Subnet = fidl_subnet!("2001:db8:eeee:eeee::/64");

    fn forwarding_config() -> fnet_interfaces_admin::Configuration {
        fnet_interfaces_admin::Configuration {
            ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                unicast_forwarding: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

async fn udp_send_msg_preflight_fidl_setup<I: UdpSendMsgPreflightTestIpExt>(
    iface: &netemul::TestInterface<'_>,
    socket: &fposix_socket::DatagramSocketProxy,
) -> Vec<fposix_socket::DatagramSocketSendMsgPreflightResponse> {
    iface
        .add_address_and_subnet_route(I::INSTALLED_ADDR)
        .await
        .expect("failed to add subnet route");

    let successful_preflights = execute_and_validate_preflights(
        [
            UdpSendMsgPreflight {
                to_addr: Some(I::UNREACHABLE_ADDR),
                expected_result: UdpSendMsgPreflightExpectation::Failure(
                    fposix::Errno::Ehostunreach,
                ),
            },
            UdpSendMsgPreflight {
                to_addr: None,
                expected_result: UdpSendMsgPreflightExpectation::Failure(
                    fposix::Errno::Edestaddrreq,
                ),
            },
        ],
        &socket,
    )
    .await;
    assert_eq!(successful_preflights, []);

    let connected_addr = I::REACHABLE_ADDR1;
    socket.connect(&connected_addr).await.expect("connect fidl error").expect("connect failed");

    // We deliberately repeat an address here to ensure that the preflight can
    // be called > 1 times with the same address.
    let mut preflights: Vec<UdpSendMsgPreflight> =
        vec![I::REACHABLE_ADDR1, I::REACHABLE_ADDR2, I::REACHABLE_ADDR2]
            .iter()
            .map(|socket_address| UdpSendMsgPreflight {
                to_addr: Some(*socket_address),
                expected_result: UdpSendMsgPreflightExpectation::Success(
                    UdpSendMsgPreflightSuccessExpectation {
                        expected_to_addr: ToAddrExpectation::Specified(None),
                        expect_all_eventpairs_valid: true,
                    },
                ),
            })
            .collect();
    preflights.push(UdpSendMsgPreflight {
        to_addr: None,
        expected_result: UdpSendMsgPreflightExpectation::Success(
            UdpSendMsgPreflightSuccessExpectation {
                expected_to_addr: ToAddrExpectation::Specified(Some(connected_addr)),
                expect_all_eventpairs_valid: true,
            },
        ),
    });

    execute_and_validate_preflights(preflights, &socket).await
}

fn assert_preflights_invalidated(
    successful_preflights: impl IntoIterator<
        Item = fposix_socket::DatagramSocketSendMsgPreflightResponse,
    >,
) {
    for successful_preflight in successful_preflights {
        validate_send_msg_preflight_response(
            &successful_preflight,
            UdpSendMsgPreflightSuccessExpectation {
                expected_to_addr: ToAddrExpectation::Unspecified,
                expect_all_eventpairs_valid: false,
            },
        )
        .expect("validate preflight response");
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case("connect_called", UdpCacheInvalidationReason::ConnectCalled)]
#[test_case("Control.Disable", UdpCacheInvalidationReason::InterfaceDisabled)]
#[test_case("Control.RemoveAddress", UdpCacheInvalidationReason::AddressRemoved)]
#[test_case("Control.SetConfiguration", UdpCacheInvalidationReason::SetConfigurationCalled)]
#[test_case("route_removed", UdpCacheInvalidationReason::RouteRemoved)]
#[test_case("route_added", UdpCacheInvalidationReason::RouteAdded)]
async fn udp_send_msg_preflight_fidl<N: Netstack, I: UdpSendMsgPreflightTestIpExt>(
    root_name: &str,
    test_name: &str,
    invalidation_reason: UdpCacheInvalidationReason,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm_name = format!("{}_{}", root_name, test_name);
    let (_net, _netstack, iface, socket) =
        setup_fastudp_network(&realm_name, N::VERSION, &sandbox, I::SOCKET_DOMAIN).await;

    let successful_preflights = udp_send_msg_preflight_fidl_setup::<I>(&iface, &socket).await;

    match invalidation_reason {
        UdpCacheInvalidationReason::ConnectCalled => {
            let connected_addr = I::REACHABLE_ADDR2;
            let () = socket
                .connect(&connected_addr)
                .await
                .expect("connect fidl error")
                .expect("connect failed");
        }
        UdpCacheInvalidationReason::InterfaceDisabled => {
            let disabled = iface
                .control()
                .disable()
                .await
                .expect("disable_interface fidl error")
                .expect("failed to disable interface");
            assert_eq!(disabled, true);
        }
        UdpCacheInvalidationReason::AddressRemoved => {
            let installed_subnet = I::INSTALLED_ADDR;
            let removed = iface
                .control()
                .remove_address(&installed_subnet)
                .await
                .expect("remove_address fidl error")
                .expect("failed to remove address");
            assert!(removed, "address was not removed from interface");
        }
        UdpCacheInvalidationReason::RouteRemoved => {
            let () = iface
                .del_subnet_route(I::INSTALLED_ADDR)
                .await
                .expect("failed to delete subnet route");
        }
        UdpCacheInvalidationReason::RouteAdded => {
            let () =
                iface.add_subnet_route(I::OTHER_SUBNET).await.expect("failed to add subnet route");
        }
        UdpCacheInvalidationReason::SetConfigurationCalled => {
            let _prev_config = iface
                .control()
                .set_configuration(&I::forwarding_config())
                .await
                .expect("set_configuration fidl error")
                .expect("failed to set interface configuration");
        }
    }

    assert_preflights_invalidated(successful_preflights);
}

enum UdpCacheInvalidationReasonV4 {
    BroadcastCalled,
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case("broadcast_called", UdpCacheInvalidationReasonV4::BroadcastCalled)]
async fn udp_send_msg_preflight_fidl_v4only<N: Netstack>(
    root_name: &str,
    test_name: &str,
    invalidation_reason: UdpCacheInvalidationReasonV4,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm_name = format!("{}_{}", root_name, test_name);
    let (_net, _netstack, iface, socket) =
        setup_fastudp_network(&realm_name, N::VERSION, &sandbox, Ipv4::SOCKET_DOMAIN).await;

    let successful_preflights = udp_send_msg_preflight_fidl_setup::<Ipv4>(&iface, &socket).await;

    match invalidation_reason {
        UdpCacheInvalidationReasonV4::BroadcastCalled => {
            let () = socket
                .set_broadcast(true)
                .await
                .expect("set_so_broadcast fidl error")
                .expect("failed to set so_broadcast");
        }
    }

    assert_preflights_invalidated(successful_preflights);
}

enum UdpCacheInvalidationReasonV6 {
    Ipv6OnlyCalled,
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case("ipv6_only_called", UdpCacheInvalidationReasonV6::Ipv6OnlyCalled)]
async fn udp_send_msg_preflight_fidl_v6only<N: Netstack>(
    root_name: &str,
    test_name: &str,
    invalidation_reason: UdpCacheInvalidationReasonV6,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm_name = format!("{}_{}", root_name, test_name);
    let (_net, _netstack, iface, socket) =
        setup_fastudp_network(&realm_name, N::VERSION, &sandbox, Ipv6::SOCKET_DOMAIN).await;

    let successful_preflights = udp_send_msg_preflight_fidl_setup::<Ipv6>(&iface, &socket).await;

    match invalidation_reason {
        UdpCacheInvalidationReasonV6::Ipv6OnlyCalled => {
            let () = socket
                .set_ipv6_only(true)
                .await
                .expect("set_ipv6_only fidl error")
                .expect("failed to set ipv6 only");
        }
    }

    assert_preflights_invalidated(successful_preflights);
}

enum UdpCacheInvalidationReasonNdp {
    RouterAdvertisement,
    RouterAdvertisementWithPrefix,
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case("ra", UdpCacheInvalidationReasonNdp::RouterAdvertisement)]
#[test_case("ra_with_prefix", UdpCacheInvalidationReasonNdp::RouterAdvertisementWithPrefix)]
async fn udp_send_msg_preflight_fidl_ndp<N: Netstack>(
    root_name: &str,
    test_name: &str,
    invalidation_reason: UdpCacheInvalidationReasonNdp,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm_name = format!("{}_{}", root_name, test_name);
    let (net, realm, iface, socket) =
        setup_fastudp_network(&realm_name, N::VERSION, &sandbox, Ipv6::SOCKET_DOMAIN).await;
    let fake_ep = net.create_fake_endpoint().expect("create fake endpoint");

    let successful_preflights = udp_send_msg_preflight_fidl_setup::<Ipv6>(&iface, &socket).await;

    // Note that the following prefix must not overlap with
    // `<Ipv6 as UdpSendMsgPreflightTestIpExt>::INSTALLED_ADDR`, as there is already a subnet
    // route for the installed addr and so discovering the same prefix will not cause a route
    // to be added and induce cache invalidation.
    const PREFIX: net_types::ip::Subnet<net_types::ip::Ipv6Addr> =
        net_subnet_v6!("2001:db8:ffff:ffff::/64");
    const SOCKADDR_IN_PREFIX: fnet::SocketAddress =
        fidl_socket_addr!("[2001:db8:ffff:ffff::1]:9999");

    // These are arbitrary large lifetime values so that the information
    // contained within the RA are not deprecated/invalidated over the course
    // of the test.
    const LARGE_ROUTER_LIFETIME: u16 = 9000;
    const LARGE_PREFIX_LIFETIME: u32 = 99999;
    async fn send_ra(
        fake_ep: &netemul::TestFakeEndpoint<'_>,
        router_lifetime: u16,
        prefix_lifetime: Option<u32>,
    ) {
        let options = prefix_lifetime
            .into_iter()
            .map(|lifetime| {
                NdpOptionBuilder::PrefixInformation(PrefixInformation::new(
                    PREFIX.prefix(),  /* prefix_length */
                    true,             /* on_link_flag */
                    true,             /* autonomous_address_configuration_flag */
                    lifetime,         /* valid_lifetime */
                    lifetime,         /* preferred_lifetime */
                    PREFIX.network(), /* prefix */
                ))
            })
            .collect::<Vec<_>>();
        ndp::send_ra_with_router_lifetime(
            &fake_ep,
            router_lifetime,
            &options,
            ipv6_consts::LINK_LOCAL_ADDR,
        )
        .await
        .expect("failed to fake RA message");
    }
    fn route_found(
        fnet_routes_ext::InstalledRoute {
            route: fnet_routes_ext::Route { destination, action, properties: _ },
            effective_properties: _,
            table_id: _,
        }: fnet_routes_ext::InstalledRoute<Ipv6>,
        want: net_types::ip::Subnet<net_types::ip::Ipv6Addr>,
        interface_id: u64,
    ) -> bool {
        let route_found = destination == want;
        if route_found {
            assert_eq!(
                action,
                fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                    outbound_interface: interface_id,
                    next_hop: None,
                }),
            );
        }
        route_found
    }
    let routes_state = realm
        .connect_to_protocol::<fnet_routes::StateV6Marker>()
        .expect("connect to route state FIDL");
    let event_stream = fnet_routes_ext::event_stream_from_state::<Ipv6>(&routes_state)
        .expect("routes event stream from state");
    let mut event_stream = pin!(event_stream);
    let mut routes = std::collections::HashSet::new();

    match invalidation_reason {
        // Send a RA message with an arbitrarily chosen but large router lifetime value to
        // indicate to Netstack that a router is present. Netstack will add a default route,
        // and invalidate the cache.
        UdpCacheInvalidationReasonNdp::RouterAdvertisement => {
            send_ra(&fake_ep, LARGE_ROUTER_LIFETIME, None /* prefix_lifetime */).await;

            // Wait until a default IPv6 route is added in response to the RA.
            let mut interface_state =
                fnet_interfaces_ext::InterfaceState::<(), _>::Unknown(iface.id());
            fnet_interfaces_ext::wait_interface_with_id(
                realm.get_interface_event_stream().expect("get interface event stream"),
                &mut interface_state,
                |iface| iface.properties.has_default_ipv6_route.then_some(()),
            )
            .await
            .expect("failed to wait for default IPv6 route");
        }
        // Send a RA message with router lifetime of 0 (otherwise the router information
        // also induces a default route and this test case tests a strict superset of the
        // `RouterAdvertisement` test case), but containing a prefix information option. Since
        // the prefix is on-link, Netstack will add a subnet route, and invalidate the cache.
        UdpCacheInvalidationReasonNdp::RouterAdvertisementWithPrefix => {
            send_ra(
                &fake_ep,
                0,                           /* router_lifetime */
                Some(LARGE_PREFIX_LIFETIME), /* prefix_lifetime */
            )
            .await;

            fnet_routes_ext::wait_for_routes(event_stream.by_ref(), &mut routes, |routes| {
                routes
                    .iter()
                    .any(|installed_route| route_found(*installed_route, PREFIX, iface.id()))
            })
            .await
            .expect("failed to wait for subnet route to appear");
        }
    }

    assert_preflights_invalidated(successful_preflights);

    // Note that `SOCKADDR_IN_PREFIX` is reachable in both cases because there
    // is either a route to the prefix or a default route.
    let successful_preflights = execute_and_validate_preflights(
        [SOCKADDR_IN_PREFIX, Ipv6::REACHABLE_ADDR1].into_iter().map(|socket_address| {
            UdpSendMsgPreflight {
                to_addr: Some(socket_address),
                expected_result: UdpSendMsgPreflightExpectation::Success(
                    UdpSendMsgPreflightSuccessExpectation {
                        expected_to_addr: ToAddrExpectation::Specified(None),
                        expect_all_eventpairs_valid: true,
                    },
                ),
            }
        }),
        &socket,
    )
    .await;

    match invalidation_reason {
        // Send an RA message invalidating the existence of the router, causing
        // the default route to be removed, and the cache to be invalidated.
        UdpCacheInvalidationReasonNdp::RouterAdvertisement => {
            send_ra(&fake_ep, 0 /* router_lifetime */, None /* prefix_lifetime */).await;

            // Wait until the default IPv6 route is removed.
            let mut interface_state =
                fnet_interfaces_ext::InterfaceState::<(), _>::Unknown(iface.id());
            fnet_interfaces_ext::wait_interface_with_id(
                realm.get_interface_event_stream().expect("get interface event stream"),
                &mut interface_state,
                |iface| (!iface.properties.has_default_ipv6_route).then_some(()),
            )
            .await
            .expect("failed to wait for default IPv6 route");
        }
        // Send an RA message invalidating the prefix, causing the subnet
        // route to be removed, and the cache to be invalidated.
        UdpCacheInvalidationReasonNdp::RouterAdvertisementWithPrefix => {
            let routes_state = realm
                .connect_to_protocol::<fnet_routes::StateV6Marker>()
                .expect("connect to route state FIDL");
            let event_stream = fnet_routes_ext::event_stream_from_state::<Ipv6>(&routes_state)
                .expect("routes event stream from state");
            let mut event_stream = pin!(event_stream);
            let _: Vec<_> = fnet_routes_ext::collect_routes_until_idle(event_stream.by_ref())
                .await
                .expect("collect routes until idle");

            send_ra(&fake_ep, 0 /* router_lifetime */, Some(0) /* prefix_lifetime */).await;

            fnet_routes_ext::wait_for_routes(event_stream, &mut routes, |routes| {
                routes
                    .iter()
                    .all(|installed_route| !route_found(*installed_route, PREFIX, iface.id()))
            })
            .await
            .expect("failed to wait for subnet route to disappear");
        }
    }

    assert_preflights_invalidated(successful_preflights);
}

async fn connect_socket_and_validate_preflight(
    socket: &fposix_socket::DatagramSocketProxy,
    addr: fnet::SocketAddress,
) -> fposix_socket::DatagramSocketSendMsgPreflightResponse {
    socket.connect(&addr).await.expect("call connect").expect("connect socket");

    let response = socket
        .send_msg_preflight(&fposix_socket::DatagramSocketSendMsgPreflightRequest::default())
        .await
        .expect("call send_msg_preflight")
        .expect("preflight check should succeed");

    validate_send_msg_preflight_response(
        &response,
        UdpSendMsgPreflightSuccessExpectation {
            expected_to_addr: ToAddrExpectation::Specified(Some(addr)),
            expect_all_eventpairs_valid: true,
        },
    )
    .expect("validate preflight response");

    response
}

async fn assert_preflight_response_invalidated(
    preflight: &fposix_socket::DatagramSocketSendMsgPreflightResponse,
) {
    async fn invoke_with_retries(
        retries: usize,
        delay: zx::MonotonicDuration,
        op: impl Fn() -> Result,
    ) -> Result {
        for _ in 0..retries {
            if let Ok(()) = op() {
                return Ok(());
            }
            fasync::Timer::new(delay).await;
        }
        op()
    }

    // NB: cache invalidation that results from internal state changes (such as
    // auto-generated address invalidation or DAD failure) is not guaranteed to
    // occur synchronously with the associated events emitted by the Netstack (such
    // as notification of address removal on the interface watcher or address state
    // provider). This means that the cache might not have been invalidated
    // immediately after observing the relevant emitted event.
    //
    // We avoid flakes due to this behavior by retrying multiple times with an
    // arbitrary delay.
    const RETRY_COUNT: usize = 3;
    const RETRY_DELAY: zx::MonotonicDuration = zx::MonotonicDuration::from_millis(500);
    let result = invoke_with_retries(RETRY_COUNT, RETRY_DELAY, || {
        validate_send_msg_preflight_response(
            &preflight,
            UdpSendMsgPreflightSuccessExpectation {
                expected_to_addr: ToAddrExpectation::Unspecified,
                expect_all_eventpairs_valid: false,
            },
        )
    })
    .await;
    assert_matches!(
        result,
        Ok(()),
        "failed to observe expected cache invalidation after auto-generated address was invalidated"
    );
}

#[netstack_test]
#[variant(N, Netstack)]
async fn udp_send_msg_preflight_autogen_addr_invalidation<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let (net, netstack, iface, socket) =
        setup_fastudp_network(name, N::VERSION, &sandbox, fposix_socket::Domain::Ipv6).await;

    let interfaces_state = netstack
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to protocol");

    // Send a Router Advertisement with the autoconf flag set to trigger
    // SLAAC, but specify a very short valid lifetime so the address
    // will expire quickly.
    let fake_ep = net.create_fake_endpoint().expect("create fake endpoint");
    // NB: we want this lifetime to be short so the test does not take too long
    // to run. However, if we make it too short, the test will be flaky, because
    // it's possible for the address lifetime to expire before the subsequent
    // SendMsgPreflight call.
    const VALID_LIFETIME_SECONDS: u32 = 10;
    let options = [NdpOptionBuilder::PrefixInformation(PrefixInformation::new(
        ipv6_consts::GLOBAL_PREFIX.prefix(),  /* prefix_length */
        false,                                /* on_link_flag */
        true,                                 /* autonomous_address_configuration_flag */
        VALID_LIFETIME_SECONDS,               /* valid_lifetime */
        0,                                    /* preferred_lifetime */
        ipv6_consts::GLOBAL_PREFIX.network(), /* prefix */
    ))];
    ndp::send_ra_with_router_lifetime(&fake_ep, 0, &options, ipv6_consts::LINK_LOCAL_ADDR)
        .await
        .expect("send router advertisement");

    // Wait for an address to be auto generated.
    let autogen_address = fnet_interfaces_ext::wait_interface_with_id(
        fnet_interfaces_ext::event_stream_from_state::<fnet_interfaces_ext::DefaultInterest>(
            &interfaces_state,
            fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .expect("create event stream"),
        &mut fnet_interfaces_ext::InterfaceState::<(), _>::Unknown(iface.id()),
        |iface| {
            iface.properties.addresses.iter().find_map(
                |fnet_interfaces_ext::Address {
                     addr: fnet::Subnet { addr, prefix_len: _ },
                     assignment_state,
                     ..
                 }| {
                    assert_eq!(
                        *assignment_state,
                        fnet_interfaces::AddressAssignmentState::Assigned
                    );
                    match addr {
                        fnet::IpAddress::Ipv4(_) => None,
                        fnet::IpAddress::Ipv6(addr @ fnet::Ipv6Address { addr: bytes }) => {
                            ipv6_consts::GLOBAL_PREFIX
                                .contains(&net_types::ip::Ipv6Addr::from_bytes(*bytes))
                                .then_some(*addr)
                        }
                    }
                },
            )
        },
    )
    .await
    .expect("wait for address assignment");

    let preflight = connect_socket_and_validate_preflight(
        &socket,
        fnet::SocketAddress::Ipv6(fnet::Ipv6SocketAddress {
            address: autogen_address,
            port: 9999, // arbitrary remote port
            zone_index: 0,
        }),
    )
    .await;

    // Wait for the address to be invalidated and removed.
    fnet_interfaces_ext::wait_interface_with_id(
        fnet_interfaces_ext::event_stream_from_state::<fnet_interfaces_ext::DefaultInterest>(
            &interfaces_state,
            fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .expect("create event stream"),
        &mut fnet_interfaces_ext::InterfaceState::<(), _>::Unknown(iface.id()),
        |iface| {
            (!iface.properties.addresses.iter().any(
                |fnet_interfaces_ext::Address {
                     addr: fnet::Subnet { addr, prefix_len: _ },
                     assignment_state,
                     ..
                 }| {
                    assert_eq!(
                        *assignment_state,
                        fnet_interfaces::AddressAssignmentState::Assigned
                    );
                    match addr {
                        fnet::IpAddress::Ipv4(_) => false,
                        fnet::IpAddress::Ipv6(addr) => addr == &autogen_address,
                    }
                },
            ))
            .then_some(())
        },
    )
    .await
    .expect("wait for address removal");

    assert_preflight_response_invalidated(&preflight).await;

    // Now that the address has been invalidated and removed, subsequent calls to
    // preflight using the connected address should fail.
    let result = socket
        .send_msg_preflight(&fposix_socket::DatagramSocketSendMsgPreflightRequest {
            to: None,
            ..Default::default()
        })
        .await
        .expect("call send_msg_preflight");
    assert_eq!(result, Err(fposix::Errno::Ehostunreach));
}

#[netstack_test]
#[variant(N, Netstack)]
async fn udp_send_msg_preflight_dad_failure<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let (net, _netstack, iface, socket) =
        setup_fastudp_network(name, N::VERSION, &sandbox, fposix_socket::Domain::Ipv6).await;

    let preflight = connect_socket_and_validate_preflight(
        &socket,
        fnet_ext::SocketAddress((std::net::Ipv6Addr::LOCALHOST, 9999).into()).into(),
    )
    .await;

    // Create the fake endpoint before adding an address to the netstack to ensure
    // that we receive all NDP messages sent by the client.
    let fake_ep = net.create_fake_endpoint().expect("create fake endpoint");

    let (address_state_provider, server) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::AddressStateProviderMarker>();
    // Create the state stream before adding the address to ensure that all
    // generated events are observed.
    let state_stream = fnet_interfaces_ext::admin::assignment_state_stream(address_state_provider);
    iface
        .control()
        .add_address(
            &fnet::Subnet {
                addr: fnet::IpAddress::Ipv6(fnet::Ipv6Address {
                    addr: ipv6_consts::LINK_LOCAL_ADDR.ipv6_bytes(),
                }),
                prefix_len: ipv6_consts::LINK_LOCAL_SUBNET_PREFIX,
            },
            &fnet_interfaces_admin::AddressParameters::default(),
            server,
        )
        .expect("call add address");

    // Expect the netstack to send a DAD message, and simulate another node already
    // owning the address. Expect DAD to fail as a result.
    let _: Vec<u8> = ndp::expect_dad_neighbor_solicitation(&fake_ep).await;
    ndp::fail_dad_with_na(&fake_ep).await;
    ndp::assert_dad_failed(state_stream).await;

    assert_preflight_response_invalidated(&preflight).await;
}

#[derive(Clone, Copy, PartialEq)]
enum CmsgType {
    IpTos,
    IpTtl,
    Ipv6Tclass,
    Ipv6Hoplimit,
    Ipv6PktInfo,
    SoTimestamp,
    SoTimestampNs,
}

struct RequestedCmsgSetExpectation {
    requested_cmsg_type: Option<CmsgType>,
    valid: bool,
}

fn validate_recv_msg_postflight_response(
    response: &fposix_socket::DatagramSocketRecvMsgPostflightResponse,
    expectation: RequestedCmsgSetExpectation,
) {
    let fposix_socket::DatagramSocketRecvMsgPostflightResponse {
        validity,
        requests,
        timestamp,
        ..
    } = response;
    let RequestedCmsgSetExpectation { valid, requested_cmsg_type } = expectation;
    let cmsg_expected =
        |cmsg_type| requested_cmsg_type.is_some_and(|req_type| req_type == cmsg_type);

    use fposix_socket::{CmsgRequests, TimestampOption};

    let bits_cmsg_requested = |cmsg_type| {
        !(requests.unwrap_or_else(|| CmsgRequests::from_bits_allow_unknown(0)) & cmsg_type)
            .is_empty()
    };

    assert_eq!(bits_cmsg_requested(CmsgRequests::IP_TOS), cmsg_expected(CmsgType::IpTos));
    assert_eq!(bits_cmsg_requested(CmsgRequests::IP_TTL), cmsg_expected(CmsgType::IpTtl));
    assert_eq!(bits_cmsg_requested(CmsgRequests::IPV6_TCLASS), cmsg_expected(CmsgType::Ipv6Tclass));
    assert_eq!(
        bits_cmsg_requested(CmsgRequests::IPV6_HOPLIMIT),
        cmsg_expected(CmsgType::Ipv6Hoplimit)
    );
    assert_eq!(
        bits_cmsg_requested(CmsgRequests::IPV6_PKTINFO),
        cmsg_expected(CmsgType::Ipv6PktInfo)
    );
    assert_eq!(
        *timestamp == Some(TimestampOption::Nanosecond),
        cmsg_expected(CmsgType::SoTimestampNs)
    );
    assert_eq!(
        *timestamp == Some(TimestampOption::Microsecond),
        cmsg_expected(CmsgType::SoTimestamp)
    );

    let expected_validity =
        if valid { Err(zx::Status::TIMED_OUT) } else { Ok(zx::Signals::EVENTPAIR_PEER_CLOSED) };
    let validity = validity.as_ref().expect("expected validity present");
    assert_eq!(
        validity
            .wait_handle(zx::Signals::EVENTPAIR_PEER_CLOSED, zx::MonotonicInstant::INFINITE_PAST),
        expected_validity,
    );
}

async fn toggle_cmsg(
    requested: bool,
    proxy: &fposix_socket::DatagramSocketProxy,
    cmsg_type: CmsgType,
) {
    match cmsg_type {
        CmsgType::IpTos => {
            let () = proxy
                .set_ip_receive_type_of_service(requested)
                .await
                .expect("set_ip_receive_type_of_service fidl error")
                .expect("set_ip_receive_type_of_service failed");
        }
        CmsgType::IpTtl => {
            let () = proxy
                .set_ip_receive_ttl(requested)
                .await
                .expect("set_ip_receive_ttl fidl error")
                .expect("set_ip_receive_ttl failed");
        }
        CmsgType::Ipv6Tclass => {
            let () = proxy
                .set_ipv6_receive_traffic_class(requested)
                .await
                .expect("set_ipv6_receive_traffic_class fidl error")
                .expect("set_ipv6_receive_traffic_class failed");
        }
        CmsgType::Ipv6Hoplimit => {
            let () = proxy
                .set_ipv6_receive_hop_limit(requested)
                .await
                .expect("set_ipv6_receive_hop_limit fidl error")
                .expect("set_ipv6_receive_hop_limit failed");
        }
        CmsgType::Ipv6PktInfo => {
            let () = proxy
                .set_ipv6_receive_packet_info(requested)
                .await
                .expect("set_ipv6_receive_packet_info fidl error")
                .expect("set_ipv6_receive_packet_info failed");
        }
        CmsgType::SoTimestamp => {
            let option = if requested {
                fposix_socket::TimestampOption::Microsecond
            } else {
                fposix_socket::TimestampOption::Disabled
            };
            let () = proxy
                .set_timestamp(option)
                .await
                .expect("set_timestamp fidl error")
                .expect("set_timestamp failed");
        }
        CmsgType::SoTimestampNs => {
            let option = if requested {
                fposix_socket::TimestampOption::Nanosecond
            } else {
                fposix_socket::TimestampOption::Disabled
            };
            let () = proxy
                .set_timestamp(option)
                .await
                .expect("set_timestamp fidl error")
                .expect("set_timestamp failed");
        }
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case("ip_tos", CmsgType::IpTos)]
#[test_case("ip_ttl", CmsgType::IpTtl)]
#[test_case("ipv6_tclass", CmsgType::Ipv6Tclass)]
#[test_case("ipv6_hoplimit", CmsgType::Ipv6Hoplimit)]
#[test_case("ipv6_pktinfo", CmsgType::Ipv6PktInfo)]
#[test_case("so_timestamp_ns", CmsgType::SoTimestampNs)]
#[test_case("so_timestamp", CmsgType::SoTimestamp)]
async fn udp_recv_msg_postflight_fidl<N: Netstack>(
    root_name: &str,
    test_name: &str,
    cmsg_type: CmsgType,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let version = match N::VERSION {
        NetstackVersion::Netstack2 { tracing, fast_udp: _ } => {
            NetstackVersion::Netstack2 { tracing, fast_udp: true }
        }
        version => version,
    };
    let netstack = sandbox
        .create_realm(
            format!("{}_{}", root_name, test_name),
            [KnownServiceProvider::Netstack(version)],
        )
        .expect("failed to create netstack realm");

    let socket_provider = netstack
        .connect_to_protocol::<fposix_socket::ProviderMarker>()
        .expect("failed to connect to socket provider");

    let datagram_socket = socket_provider
        .datagram_socket(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("datagram_socket fidl error")
        .expect("failed to create datagram socket");

    let datagram_socket = match datagram_socket {
        fposix_socket::ProviderDatagramSocketResponse::DatagramSocket(socket) => socket,
        socket => panic!("unexpected datagram socket variant: {:?}", socket),
    };

    let proxy = datagram_socket.into_proxy();

    // Expect no cmsgs requested by default.
    let response = proxy
        .recv_msg_postflight()
        .await
        .expect("recv_msg_postflight fidl error")
        .expect("recv_msg_postflight failed");
    validate_recv_msg_postflight_response(
        &response,
        RequestedCmsgSetExpectation { requested_cmsg_type: None, valid: true },
    );

    toggle_cmsg(true, &proxy, cmsg_type).await;

    // Expect requesting a cmsg invalidates the returned cmsg set.
    validate_recv_msg_postflight_response(
        &response,
        RequestedCmsgSetExpectation { requested_cmsg_type: None, valid: false },
    );

    // Expect the cmsg is returned in the latest requested set.
    let response = proxy
        .recv_msg_postflight()
        .await
        .expect("recv_msg_postflight fidl error")
        .expect("recv_msg_postflight failed");
    validate_recv_msg_postflight_response(
        &response,
        RequestedCmsgSetExpectation { requested_cmsg_type: Some(cmsg_type), valid: true },
    );

    toggle_cmsg(false, &proxy, cmsg_type).await;

    // Expect unrequesting a cmsg invalidates the returned cmsg set.
    validate_recv_msg_postflight_response(
        &response,
        RequestedCmsgSetExpectation { requested_cmsg_type: Some(cmsg_type), valid: false },
    );

    // Expect the cmsg is no longer returned in the latest requested set.
    let response = proxy
        .recv_msg_postflight()
        .await
        .expect("recv_msg_postflight fidl error")
        .expect("recv_msg_postflight failed");
    validate_recv_msg_postflight_response(
        &response,
        RequestedCmsgSetExpectation { requested_cmsg_type: None, valid: true },
    );
}

async fn run_tcp_socket_test(
    server: &netemul::TestRealm<'_>,
    server_addr: fnet::IpAddress,
    client: &netemul::TestRealm<'_>,
    client_addr: fnet::IpAddress,
) {
    let fnet_ext::IpAddress(client_addr) = client_addr.into();
    let client_addr = std::net::SocketAddr::new(client_addr, 1234);

    let fnet_ext::IpAddress(server_addr) = server_addr.into();
    let server_addr = std::net::SocketAddr::new(server_addr, 8080);

    // We pick a payload that is small enough to be guaranteed to fit in a TCP segment so both the
    // client and server can read the entire payload in a single `read`.
    const PAYLOAD: &'static str = "Hello World";

    let listener = fasync::net::TcpListener::listen_in_realm(server, server_addr)
        .await
        .expect("failed to create server socket");

    let server_fut = async {
        let (_, mut stream, from) = listener.accept().await.expect("accept failed");

        let mut buf = [0u8; 1024];
        let read_count = stream.read(&mut buf).await.expect("read from tcp server stream failed");

        // Unspecified addresses will use loopback as their source
        if client_addr.ip().is_unspecified() {
            assert!(from.ip().is_loopback())
        } else {
            assert_eq!(from.ip(), client_addr.ip());
        }
        assert_eq!(read_count, PAYLOAD.as_bytes().len());
        assert_eq!(&buf[..read_count], PAYLOAD.as_bytes());

        let write_count =
            stream.write(PAYLOAD.as_bytes()).await.expect("write to tcp server stream failed");
        assert_eq!(write_count, PAYLOAD.as_bytes().len());
    };

    let client_fut = async {
        let mut stream = fasync::net::TcpStream::connect_in_realm(client, server_addr)
            .await
            .expect("failed to create client socket");

        let write_count =
            stream.write(PAYLOAD.as_bytes()).await.expect("write to tcp client stream failed");

        assert_eq!(write_count, PAYLOAD.as_bytes().len());

        let mut buf = [0u8; 1024];
        let read_count = stream.read(&mut buf).await.expect("read from tcp client stream failed");

        assert_eq!(read_count, PAYLOAD.as_bytes().len());
        assert_eq!(&buf[..read_count], PAYLOAD.as_bytes());
    };

    let ((), ()) = futures::future::join(client_fut, server_fut).await;
}

trait TestIpExt: Ip {
    const DOMAIN: fposix_socket::Domain;
    const CLIENT_SUBNET: fnet::Subnet;
    const SERVER_SUBNET: fnet::Subnet;
}

impl TestIpExt for Ipv4 {
    const DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv4;
    const CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("192.168.0.2/24");
    const SERVER_SUBNET: fnet::Subnet = fidl_subnet!("192.168.0.1/24");
}

impl TestIpExt for Ipv6 {
    const DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv6;
    const CLIENT_SUBNET: fnet::Subnet = fidl_subnet!("2001:0db8:85a3::8a2e:0370:7334/64");
    const SERVER_SUBNET: fnet::Subnet = fidl_subnet!("2001:0db8:85a3::8a2e:0370:7335/64");
}

// Note: This methods returns the two end of the established connection through
// a continuation, this is if we return them directly, the endpoints created
// inside the function will be dropped so no packets can be possibly sent and
// ultimately fail the tests. Using a closure allows us to execute the rest of
// test within the context where the endpoints are still alive.
async fn tcp_socket_accept_cross_ns<
    I: TestIpExt,
    Client: Netstack,
    Server: Netstack,
    Fut: Future,
    F: FnOnce(fasync::net::TcpStream, fasync::net::TcpStream) -> Fut,
>(
    name: &str,
    f: F,
) -> Fut::Output {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let _packet_capture = net.start_capture(name).await.expect("starting packet capture");
    let client = sandbox
        .create_netstack_realm::<Client, _>(format!("{}_client", name))
        .expect("failed to create client realm");
    let client_interface =
        client.join_network(&net, "client-ep").await.expect("failed to join network in realm");
    client_interface
        .add_address_and_subnet_route(I::CLIENT_SUBNET)
        .await
        .expect("configure address");
    client_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let server = sandbox
        .create_netstack_realm::<Server, _>(format!("{}_server", name))
        .expect("failed to create server realm");
    let server_interface =
        server.join_network(&net, "server-ep").await.expect("failed to join network in realm");
    server_interface
        .add_address_and_subnet_route(I::SERVER_SUBNET)
        .await
        .expect("configure address");
    server_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let fnet_ext::IpAddress(client_ip) = I::CLIENT_SUBNET.addr.into();

    let fnet_ext::IpAddress(server_ip) = I::SERVER_SUBNET.addr.into();
    let server_addr = std::net::SocketAddr::new(server_ip, 8080);

    let listener = fasync::net::TcpListener::listen_in_realm(&server, server_addr)
        .await
        .expect("failed to create server socket");

    let client = fasync::net::TcpStream::connect_in_realm(&client, server_addr)
        .await
        .expect("failed to create client socket");

    let (_, accepted, from) = listener.accept().await.expect("accept failed");
    assert_eq!(from.ip(), client_ip);

    f(client, accepted).await
}

#[netstack_test]
#[variant(I, Ip)]
#[variant(Client, Netstack)]
#[variant(Server, Netstack)]
async fn tcp_socket_accept<I: TestIpExt, Client: Netstack, Server: Netstack>(name: &str) {
    tcp_socket_accept_cross_ns::<I, Client, Server, _, _>(name, |_client, _server| async {}).await
}

#[netstack_test]
#[variant(I, Ip)]
#[variant(Client, Netstack)]
#[variant(Server, Netstack)]
async fn tcp_socket_send_recv<I: TestIpExt, Client: Netstack, Server: Netstack>(name: &str) {
    async fn send_recv(mut sender: fasync::net::TcpStream, mut receiver: fasync::net::TcpStream) {
        const PAYLOAD: &'static [u8] = b"Hello World";
        let write_count = sender.write(PAYLOAD).await.expect("write to tcp client stream failed");
        assert_matches!(sender.close().await, Ok(()));

        assert_eq!(write_count, PAYLOAD.len());
        let mut buf = [0u8; 16];
        let read_count = receiver.read(&mut buf).await.expect("read from tcp server stream failed");
        assert_eq!(read_count, write_count);
        assert_eq!(&buf[..read_count], PAYLOAD);

        // Echo the bytes back the already closed sender, the sender is already
        // closed and it should not cause any panic.
        assert_eq!(
            receiver.write(&buf[..read_count]).await.expect("write to tcp server stream failed"),
            read_count
        );
    }
    tcp_socket_accept_cross_ns::<I, Client, Server, _, _>(name, send_recv).await
}

#[netstack_test]
#[variant(I, Ip)]
#[variant(Client, Netstack)]
#[variant(Server, Netstack)]
async fn tcp_socket_shutdown_connection<I: TestIpExt, Client: Netstack, Server: Netstack>(
    name: &str,
) {
    tcp_socket_accept_cross_ns::<I, Client, Server, _, _>(
        name,
        |mut client: fasync::net::TcpStream, mut server: fasync::net::TcpStream| async move {
            client.shutdown(std::net::Shutdown::Both).expect("failed to shutdown the client");
            assert_eq!(
                client.write(b"Hello").await.map_err(|e| e.kind()),
                Err(std::io::ErrorKind::BrokenPipe)
            );
            assert_matches!(server.read_to_end(&mut Vec::new()).await, Ok(0));
            server.shutdown(std::net::Shutdown::Both).expect("failed to shutdown the server");
            assert_eq!(
                server.write(b"Hello").await.map_err(|e| e.kind()),
                Err(std::io::ErrorKind::BrokenPipe)
            );
            assert_matches!(client.read_to_end(&mut Vec::new()).await, Ok(0));
        },
    )
    .await
}

// Shutting down one end of the socket in both directions should cause writes to fail on the
// other end. Same applies when closing the socket, (`close()` implies `shutdown(RDWR)`).
#[netstack_test]
#[variant(I, Ip)]
#[variant(Client, Netstack)]
#[variant(Server, Netstack)]
#[test_case(false; "shutdown")]
#[test_case(true; "close")]
async fn tcp_socket_send_after_shutdown<I: TestIpExt, Client: Netstack, Server: Netstack>(
    name: &str,
    close: bool,
) {
    tcp_socket_accept_cross_ns::<I, Client, Server, _, _>(
        name,
        |mut client: fasync::net::TcpStream, server: fasync::net::TcpStream| async move {
            // Either close or shutdown the server end of the socket.
            let _server = if close {
                std::mem::drop(server);
                None
            } else {
                server.shutdown(std::net::Shutdown::Both).expect("Failed to shutdown TCP read");
                Some(server)
            };

            async {
                // Keep writing until we get an error.
                loop {
                    if let Err(e) = client.write(b"Hello").await {
                        // NS2 returns EPIPE, which is incorrect. Check the error only with NS3.
                        if !matches!(
                            Client::VERSION,
                            NetstackVersion::Netstack2 { .. } | NetstackVersion::ProdNetstack2
                        ) {
                            assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset);
                        }
                        break;
                    }
                }
            }
            .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
                panic!("timed out waiting for error from send()")
            })
            .await;
        },
    )
    .await
}

#[netstack_test]
#[variant(I, Ip)]
#[variant(N, Netstack)]
async fn tcp_socket_shutdown_listener<I: TestIpExt, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{}_client", name))
        .expect("failed to create client realm");
    let client_interface =
        client.join_network(&net, "client-ep").await.expect("failed to join network in realm");
    client_interface
        .add_address_and_subnet_route(I::CLIENT_SUBNET)
        .await
        .expect("configure address");
    client_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let server = sandbox
        .create_netstack_realm::<N, _>(format!("{}_server", name))
        .expect("failed to create server realm");
    let server_interface =
        server.join_network(&net, "server-ep").await.expect("failed to join network in realm");
    server_interface
        .add_address_and_subnet_route(I::SERVER_SUBNET)
        .await
        .expect("configure address");
    server_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let fnet_ext::IpAddress(client_ip) = I::CLIENT_SUBNET.addr.into();
    let fnet_ext::IpAddress(server_ip) = I::SERVER_SUBNET.addr.into();
    let client_addr = std::net::SocketAddr::new(client_ip, 8080);
    let server_addr = std::net::SocketAddr::new(server_ip, 8080);

    // Create listener sockets on both netstacks and shut them down.
    let client = socket2::Socket::from(
        std::net::TcpListener::listen_in_realm(&client, client_addr)
            .await
            .expect("failed to create the client socket"),
    );
    assert_matches!(client.shutdown(std::net::Shutdown::Both), Ok(()));

    let server = socket2::Socket::from(
        std::net::TcpListener::listen_in_realm(&server, server_addr)
            .await
            .expect("failed to create the server socket"),
    );

    assert_matches!(server.shutdown(std::net::Shutdown::Both), Ok(()));

    // Listen again on the server socket.
    assert_matches!(server.listen(1), Ok(()));
    let server = fasync::net::TcpListener::from_std(server.into()).unwrap();

    // Call connect on the client socket.
    let _client = fasync::net::TcpStream::connect_from_raw(client, server_addr)
        .expect("failed to connect client socket")
        .await;

    // Both should succeed and we have an established connection.
    let (_, _accepted, from) = server.accept().await.expect("accept failed");
    let fnet_ext::IpAddress(client_ip) = I::CLIENT_SUBNET.addr.into();
    assert_eq!(from.ip(), client_ip);
}

#[netstack_test]
#[variant(N, Netstack)]
async fn tcpv4_tcpv6_listeners_coexist<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let host = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create server realm");
    let interface =
        host.join_network(&net, "server-ep").await.expect("failed to join network in realm");
    interface
        .add_address_and_subnet_route(Ipv4::SERVER_SUBNET)
        .await
        .expect("failed to add v4 addr");
    interface
        .add_address_and_subnet_route(Ipv6::SERVER_SUBNET)
        .await
        .expect("failed to add v6 addr");

    let fnet_ext::IpAddress(v4_addr) = Ipv4::SERVER_SUBNET.addr.into();
    let fnet_ext::IpAddress(v6_addr) = Ipv6::SERVER_SUBNET.addr.into();
    let v4_addr = std::net::SocketAddr::new(v4_addr, 8080);
    let v6_addr = std::net::SocketAddr::new(v6_addr, 8080);
    let _listener_v4 = fasync::net::TcpListener::listen_in_realm(&host, v4_addr)
        .await
        .expect("failed to create v4 socket");
    let _listener_v6 = fasync::net::TcpListener::listen_in_realm(&host, v6_addr)
        .await
        .expect("failed to create v6 socket");
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(100; "large positive")]
#[test_case(1; "min positive")]
#[test_case(0; "zero")]
#[test_case(-1; "negative")]
async fn tcp_socket_listen<N: Netstack, I: TestIpExt>(name: &str, backlog: i16) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");

    let host = sandbox
        .create_netstack_realm::<N, _>(format!("{}host", name))
        .expect("failed to create realm");

    const PORT: u16 = 8080;

    let listener = {
        let socket = host
            .stream_socket(I::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
            .await
            .expect("create TCP socket");
        socket
            .bind(&std::net::SocketAddr::from((I::UNSPECIFIED_ADDRESS.to_ip_addr(), PORT)).into())
            .expect("no conflict");

        // Listen with the provided backlog value.
        socket
            .listen(backlog.into())
            .unwrap_or_else(|_| panic!("backlog of {} is accepted", backlog));
        fasync::net::TcpListener::from_std(socket.into()).expect("is TCP listener")
    };

    let mut conn = fasync::net::TcpStream::connect_in_realm(
        &host,
        (I::LOOPBACK_ADDRESS.to_ip_addr(), PORT).into(),
    )
    .await
    .expect("should be accepted");

    let (_, mut served, _): (fasync::net::TcpListener, _, std::net::SocketAddr) =
        listener.accept().await.expect("connection waiting");

    // Confirm that the connection is working.
    const NUM_BYTES: u8 = 10;
    let written = Vec::from_iter(0..NUM_BYTES);
    served.write_all(written.as_slice()).await.expect("write succeeds");
    let mut read = [0; NUM_BYTES as usize];
    conn.read_exact(&mut read).await.expect("read finished");
    assert_eq!(&read, written.as_slice());
}

#[netstack_test]
#[variant(N, Netstack)]
async fn tcp_socket<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{}_client", name))
        .expect("failed to create client realm");
    let client_ep = client
        .join_network_with(
            &net,
            "client",
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(CLIENT_MAC)),
            Default::default(),
        )
        .await
        .expect("client failed to join network");
    client_ep.add_address_and_subnet_route(CLIENT_SUBNET).await.expect("configure address");
    client_ep.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let server = sandbox
        .create_netstack_realm::<N, _>(format!("{}_server", name))
        .expect("failed to create server realm");
    let server_ep = server
        .join_network_with(
            &net,
            "server",
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(SERVER_MAC)),
            Default::default(),
        )
        .await
        .expect("server failed to join network");
    server_ep.add_address_and_subnet_route(SERVER_SUBNET).await.expect("configure address");
    server_ep.apply_nud_flake_workaround().await.expect("nud flake workaround");

    run_tcp_socket_test(&server, SERVER_SUBNET.addr, &client, CLIENT_SUBNET.addr).await
}

// This is a regression test for https://fxbug.dev/361402347.
#[netstack_test]
#[variant(I, Ip)]
#[variant(N, Netstack)]
async fn tcp_bind_listen_on_same_port_different_address<I: TestIpExt, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let net = sandbox.create_network("net").await.expect("create network");
    let netstack =
        sandbox.create_netstack_realm::<N, _>(format!("{}", name)).expect("create netstack realm");
    let interface = netstack.join_network(&net, "ep").await.expect("join network");
    interface.add_address_and_subnet_route(I::CLIENT_SUBNET).await.expect("configure address");
    interface.add_address_and_subnet_route(I::SERVER_SUBNET).await.expect("configure address");

    const PORT: u16 = 80;

    let first = TcpSocket::new_in_realm::<I>(&netstack).await.expect("create TCP socket");
    let fnet_ext::IpAddress(addr) = I::CLIENT_SUBNET.addr.into();
    first.bind(&std::net::SocketAddr::new(addr, PORT).into()).expect("no conflict");
    first.listen(0).expect("no conflict");

    let second = TcpSocket::new_in_realm::<I>(&netstack).await.expect("create TCP socket");
    let fnet_ext::IpAddress(addr) = I::SERVER_SUBNET.addr.into();
    second.bind(&std::net::SocketAddr::new(addr, PORT).into()).expect("no conflict");
    second.listen(0).expect("no conflict");
}

enum WhichEnd {
    Send,
    Receive,
}

#[netstack_test]
#[variant(I, Ip)]
#[variant(N, Netstack)]
#[test_case(WhichEnd::Send; "send buffer")]
#[test_case(WhichEnd::Receive; "receive buffer")]
async fn tcp_buffer_size<I: TestIpExt, N: Netstack>(name: &str, which: WhichEnd) {
    tcp_socket_accept_cross_ns::<I, N, N, _, _>(name, |mut sender, mut receiver| async move {
        // Set either the sender SO_SNDBUF or receiver SO_RECVBUF so that a
        // large amount of data can be buffered even if the receiver isn't
        // reading.
        let set_size;
        let size = match which {
            WhichEnd::Send => {
                const SEND_BUFFER_SIZE: usize = 1024 * 1024;
                set_size = SEND_BUFFER_SIZE;
                let sender_ref = SockRef::from(sender.std());
                sender_ref.set_send_buffer_size(SEND_BUFFER_SIZE).expect("set size is infallible");
                sender_ref.send_buffer_size().expect("get size is infallible")
            }
            WhichEnd::Receive => {
                const RECEIVE_BUFFER_SIZE: usize = 128 * 1024;
                let receiver_ref = SockRef::from(receiver.std());
                set_size = RECEIVE_BUFFER_SIZE;
                receiver_ref
                    .set_recv_buffer_size(RECEIVE_BUFFER_SIZE)
                    .expect("set size is infallible");
                receiver_ref.recv_buffer_size().expect("get size is infallible")
            }
        };
        assert!(size >= set_size, "{} >= {}", size, set_size);

        let data = Vec::from_iter((0..set_size).map(|i| i as u8));
        sender.write_all(data.as_slice()).await.expect("all written");
        sender.close().await.expect("close succeeds");

        let mut buf = Vec::with_capacity(set_size);
        let read = receiver.read_to_end(&mut buf).await.expect("all bytes read");
        assert_eq!(read, set_size);
    })
    .await
}

#[netstack_test]
#[variant(I, Ip)]
#[variant(N, Netstack)]
async fn decrease_tcp_sendbuf_size<I: TestIpExt, N: Netstack>(name: &str) {
    // This is a regression test for https://fxbug.dev/42072897. With Netstack3,
    // if a TCP socket had a full send buffer and a decrease of the send buffer
    // size was requested, the new size would not take effect immediately as
    // expected.  Instead, the apparent size (visible via POSIX `getsockopt`
    // with `SO_SNDBUF`) would decrease linearly as data was transferred. This
    // test verifies that this is no longer the case by filling up the send
    // buffer for a TCP socket, requesting a smaller size, then observing the
    // size as the buffer is drained (by transferring to the receiver).
    tcp_socket_accept_cross_ns::<I, N, N, _, _>(name, |mut sender, mut receiver| async move {
        // Fill up the sender and receiver buffers by writing a lot of data.
        const LARGE_BUFFER_SIZE: usize = 1024 * 1024;
        SockRef::from(sender.std()).set_send_buffer_size(LARGE_BUFFER_SIZE).expect("can set");

        let data = vec![b'x'; LARGE_BUFFER_SIZE];
        // Fill up the sending socket's send buffer. Since we can't prevent it
        // from sending data to the receiver, this will also fill up the
        // receiver's receive buffer, which is fine. We do this by writing as
        // much as possible while giving time for the sender to transfer the
        // bytes to the receiver.
        let mut written = 0;
        while sender
            .write_all(data.as_slice())
            .map(|r| {
                r.unwrap();
                true
            })
            .on_timeout(zx::MonotonicDuration::from_seconds(2), || false)
            .await
        {
            written += data.len();
        }

        // Now reduce the size of the send buffer. The apparent size of the send
        // buffer should decrease immediately.
        let sender_ref = SockRef::from(sender.std());
        let size_before = sender_ref.send_buffer_size().unwrap();
        sender_ref.set_send_buffer_size(0).expect("can set");
        let size_after = sender_ref.send_buffer_size().unwrap();
        assert!(size_before > size_after, "{} > {}", size_before, size_after);

        // Read data from the socket so that the the sender can send more.
        // This won't finish until the entire transfer has been received.
        let mut buf = vec![0; LARGE_BUFFER_SIZE];
        let mut read = 0;
        while read < written {
            read += receiver.read(&mut buf).await.expect("can read");
        }

        let sender = SockRef::from(sender.std());
        // Draining all the data from the sender into the receiver shouldn't
        // decrease the sender's apparent send buffer size.
        assert_eq!(sender.send_buffer_size().unwrap(), size_after);
        // Now that the sender's buffer is empty, try setting the size again.
        // This should have no effect!
        sender.set_send_buffer_size(0).expect("can set");
        assert_eq!(sender.send_buffer_size().unwrap(), size_after);
    })
    .await
}

// Helper function to add ip device to stack.
async fn install_ip_device(
    realm: &netemul::TestRealm<'_>,
    port: fhardware_network::PortProxy,
    addrs: impl IntoIterator<Item = fnet::Subnet>,
) -> (u64, fnet_interfaces_ext::admin::Control, fnet_interfaces_admin::DeviceControlProxy) {
    let installer = realm.connect_to_protocol::<fnet_interfaces_admin::InstallerMarker>().unwrap();

    let port_id = port.get_info().await.expect("get port info").id.expect("missing port id");
    let device = {
        let (device, server_end) =
            fidl::endpoints::create_endpoints::<fhardware_network::DeviceMarker>();
        let () = port.get_device(server_end).expect("get device");
        device
    };
    let device_control = {
        let (control, server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::DeviceControlMarker>();
        let () = installer.install_device(device, server_end).expect("install device");
        control
    };
    let control = {
        let (control, server_end) =
            fnet_interfaces_ext::admin::Control::create_endpoints().expect("create endpoints");
        let () = device_control
            .create_interface(&port_id, server_end, &fnet_interfaces_admin::Options::default())
            .expect("create interface");
        control
    };
    assert!(control.enable().await.expect("enable interface").expect("failed to enable interface"));

    let id = control.get_id().await.expect("get id");

    let () = futures::stream::iter(addrs.into_iter())
        .for_each_concurrent(None, |subnet| {
            let (address_state_provider, server_end) = fidl::endpoints::create_proxy::<
                fnet_interfaces_admin::AddressStateProviderMarker,
            >();

            // We're not interested in maintaining the address' lifecycle through
            // the proxy.
            let () = address_state_provider.detach().expect("detach");
            let () = control
                .add_address(
                    &subnet,
                    &fnet_interfaces_admin::AddressParameters {
                        add_subnet_route: Some(true),
                        ..Default::default()
                    },
                    server_end,
                )
                .expect("add address");

            // Wait for the address to be assigned.
            fnet_interfaces_ext::admin::wait_assignment_state(
                fnet_interfaces_ext::admin::assignment_state_stream(address_state_provider),
                fnet_interfaces::AddressAssignmentState::Assigned,
            )
            .map(|r| r.expect("wait assignment state"))
        })
        .await;
    (id, control, device_control)
}

/// Creates default base config for an IP tun device.
fn base_ip_device_port_config() -> fnet_tun::BasePortConfig {
    fnet_tun::BasePortConfig {
        id: Some(devices::TUN_DEFAULT_PORT_ID),
        mtu: Some(netemul::DEFAULT_MTU.into()),
        rx_types: Some(vec![
            fhardware_network::FrameType::Ipv4,
            fhardware_network::FrameType::Ipv6,
        ]),
        tx_types: Some(vec![
            fhardware_network::FrameTypeSupport {
                type_: fhardware_network::FrameType::Ipv4,
                features: fhardware_network::FRAME_FEATURES_RAW,
                supported_flags: fhardware_network::TxFlags::empty(),
            },
            fhardware_network::FrameTypeSupport {
                type_: fhardware_network::FrameType::Ipv6,
                features: fhardware_network::FRAME_FEATURES_RAW,
                supported_flags: fhardware_network::TxFlags::empty(),
            },
        ]),
        ..Default::default()
    }
}

enum IpEndpointsSocketTestCase {
    Udp,
    Tcp,
    Packet(fpacket::Kind),
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(IpEndpointsSocketTestCase::Udp; "udp_socket")]
#[test_case(IpEndpointsSocketTestCase::Tcp; "tcp_socket")]
#[test_case(IpEndpointsSocketTestCase::Packet(fpacket::Kind::Network); "packet_dgram_socket")]
#[test_case(IpEndpointsSocketTestCase::Packet(fpacket::Kind::Link); "packet_raw_socket")]
async fn ip_endpoints_socket<N: Netstack, I: Ip>(
    name: &str,
    socket_type: IpEndpointsSocketTestCase,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{}_client", name))
        .expect("failed to create client realm");
    let server = sandbox
        .create_netstack_realm::<N, _>(format!("{}_server", name))
        .expect("failed to create server realm");

    let (_tun_pair, client_port, server_port) = devices::create_tun_pair_with(
        fnet_tun::DevicePairConfig::default(),
        fnet_tun::DevicePairPortConfig {
            base: Some(base_ip_device_port_config()),
            // No MAC, this is a pure IP device.
            mac_left: None,
            mac_right: None,
            ..Default::default()
        },
    )
    .await;

    // Addresses must be in the same subnet.
    let (client_addr, server_addr) = match I::VERSION {
        IpVersion::V4 => (fidl_subnet!("192.168.0.1/24"), fidl_subnet!("192.168.0.2/24")),
        IpVersion::V6 => (fidl_subnet!("2001::1/120"), fidl_subnet!("2001::2/120")),
    };

    // We install both devices in parallel because a DevicePair will only have
    // its link signal set to up once both sides have sessions attached. This
    // way both devices will be configured "at the same time" and DAD will be
    // able to complete for IPv6 addresses.
    let (
        (client_id, _client_control, _client_device_control),
        (server_id, _server_control, _server_device_control),
    ) = futures::future::join(
        install_ip_device(&client, client_port, [client_addr]),
        install_ip_device(&server, server_port, [server_addr]),
    )
    .await;

    match socket_type {
        IpEndpointsSocketTestCase::Udp => {
            run_udp_socket_test(&server, server_addr.addr, &client, client_addr.addr).await
        }
        IpEndpointsSocketTestCase::Tcp => {
            run_tcp_socket_test(&server, server_addr.addr, &client, client_addr.addr).await
        }
        IpEndpointsSocketTestCase::Packet(kind) => {
            run_ip_endpoint_packet_socket_test(
                &server,
                server_id,
                &client,
                client_id,
                I::VERSION,
                kind,
            )
            .await
        }
    }
}

/// Returns true if the packet is one of is IGMP, MLD, or NDP.

/// This traffic implicitly exists on the network, and may be unexpectedly
/// received during tests who interact directly with the underlying device (e.g.
/// via packet sockets, or via the netdevice APIs).
///
/// Returns `Err` if the packet cannot be parsed.
fn is_packet_spurious(ip_version: IpVersion, mut body: &[u8]) -> Result<bool> {
    match ip_version {
        IpVersion::V6 => {
            let ipv6 = Ipv6Packet::parse(&mut body, ())
                .with_context(|| format!("failed to parse IPv6 packet {:?}", body))?;
            if ipv6.proto() == Ipv6Proto::Icmpv6 {
                let parse_args =
                    packet_formats::icmp::IcmpParseArgs::new(ipv6.src_ip(), ipv6.dst_ip());
                match Icmpv6Packet::parse(&mut body, parse_args)
                    .context("failed to parse ICMP packet")?
                {
                    Icmpv6Packet::Ndp(p) => {
                        println!("ignoring NDP packet {:?}", p);
                        Ok(true)
                    }
                    Icmpv6Packet::Mld(p) => {
                        println!("ignoring MLD packet {:?}", p);
                        Ok(true)
                    }
                    Icmpv6Packet::DestUnreachable(_)
                    | Icmpv6Packet::PacketTooBig(_)
                    | Icmpv6Packet::TimeExceeded(_)
                    | Icmpv6Packet::ParameterProblem(_)
                    | Icmpv6Packet::EchoRequest(_)
                    | Icmpv6Packet::EchoReply(_) => Ok(false),
                }
            } else {
                Ok(false)
            }
        }
        IpVersion::V4 => {
            let ipv4 = Ipv4Packet::parse(&mut body, ())
                .with_context(|| format!("failed to parse IPv4 packet {:?}", body))?;
            if ipv4.proto() == Ipv4Proto::Igmp {
                let p = IgmpPacket::parse(&mut body, ()).context("failed to parse IGMP packet")?;
                println!("ignoring IGMP packet {:?}", p);
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

#[netstack_test]
#[variant(N, Netstack)]
async fn ip_endpoint_packets<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create client realm");

    let tun = fuchsia_component::client::connect_to_protocol::<fnet_tun::ControlMarker>()
        .expect("failed to connect to tun protocol");

    let (tun_dev, req) = fidl::endpoints::create_proxy::<fnet_tun::DeviceMarker>();
    let () = tun
        .create_device(
            &fnet_tun::DeviceConfig { base: None, blocking: Some(true), ..Default::default() },
            req,
        )
        .expect("failed to create tun pair");

    let (_tun_port, port) = {
        let (tun_port, server_end) = fidl::endpoints::create_proxy::<fnet_tun::PortMarker>();
        let () = tun_dev
            .add_port(
                &fnet_tun::DevicePortConfig {
                    base: Some(base_ip_device_port_config()),
                    online: Some(true),
                    // No MAC, this is a pure IP device.
                    mac: None,
                    ..Default::default()
                },
                server_end,
            )
            .expect("add_port failed");

        let (port, server_end) = fidl::endpoints::create_proxy::<fhardware_network::PortMarker>();
        let () = tun_port.get_port(server_end).expect("get_port failed");
        (tun_port, port)
    };

    // Declare addresses in the same subnet. Alice is Netstack, and Bob is our
    // end of the tun device that we'll use to inject frames.
    const PREFIX_V4: u8 = 24;
    const PREFIX_V6: u8 = 120;
    const ALICE_ADDR_V4: fnet::Ipv4Address = fidl_ip_v4!("192.168.0.1");
    const ALICE_ADDR_V6: fnet::Ipv6Address = fidl_ip_v6!("2001::1");
    const BOB_ADDR_V4: fnet::Ipv4Address = fidl_ip_v4!("192.168.0.2");
    const BOB_ADDR_V6: fnet::Ipv6Address = fidl_ip_v6!("2001::2");

    let (_id, _control, _device_control) = install_ip_device(
        &realm,
        port,
        [
            fnet::Subnet { addr: fnet::IpAddress::Ipv4(ALICE_ADDR_V4), prefix_len: PREFIX_V4 },
            fnet::Subnet { addr: fnet::IpAddress::Ipv6(ALICE_ADDR_V6), prefix_len: PREFIX_V6 },
        ],
    )
    .await;

    let read_frame = futures::stream::try_unfold(tun_dev.clone(), |tun_dev| async move {
        let frame = tun_dev
            .read_frame()
            .await
            .context("read_frame_failed")?
            .map_err(zx::Status::from_raw)
            .context("read_frame returned error")?;
        Ok(Some((frame, tun_dev)))
    })
    .try_filter_map(|frame| async move {
        let frame_type = frame.frame_type.context("missing frame type in frame")?;
        let frame_data = frame.data.context("missing data in frame")?;
        let is_spurious = match frame_type {
            fhardware_network::FrameType::Ipv6 => {
                is_packet_spurious(IpVersion::V6, &frame_data[..])
            }
            fhardware_network::FrameType::Ipv4 => {
                is_packet_spurious(IpVersion::V4, &frame_data[..])
            }
            fhardware_network::FrameType::Ethernet => Ok(false),
            fhardware_network::FrameType::__SourceBreaking { unknown_ordinal } => {
                panic!("unknown frame type {unknown_ordinal}")
            }
        }?;
        Ok((!is_spurious).then_some((frame_type, frame_data)))
    });
    let mut read_frame = pin!(read_frame);

    async fn write_frame_and_read_with_timeout<S>(
        tun_dev: &fnet_tun::DeviceProxy,
        frame: fnet_tun::Frame,
        read_frame: &mut S,
    ) -> Result<Option<S::Ok>>
    where
        S: futures::stream::TryStream<Error = anyhow::Error> + std::marker::Unpin,
    {
        let () = tun_dev
            .write_frame(&frame)
            .await
            .context("write_frame failed")?
            .map_err(zx::Status::from_raw)
            .context("write_frame returned error")?;
        Ok(read_frame
            .try_next()
            .and_then(|f| {
                futures::future::ready(f.context("frame stream ended unexpectedly").map(Some))
            })
            .on_timeout(
                fasync::MonotonicInstant::after(zx::MonotonicDuration::from_millis(50)),
                || Ok(None),
            )
            .await
            .context("failed to read frame")?)
    }

    const ICMP_ID: u16 = 10;
    const SEQ_NUM: u16 = 1;
    let mut payload = [1u8, 2, 3, 4];

    // Manually build a ping frame and see it come back out of the stack.
    let src_ip = Ipv4Addr::new(BOB_ADDR_V4.addr);
    let dst_ip = Ipv4Addr::new(ALICE_ADDR_V4.addr);
    let packet = packet::Buf::new(&mut payload[..], ..)
        .encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
            src_ip,
            dst_ip,
            IcmpZeroCode,
            IcmpEchoRequest::new(ICMP_ID, SEQ_NUM),
        ))
        .encapsulate(Ipv4PacketBuilder::new(src_ip, dst_ip, 1, Ipv4Proto::Icmp))
        .serialize_vec_outer()
        .expect("serialization failed")
        .as_ref()
        .to_vec();

    // Send v4 ping request.
    let () = tun_dev
        .write_frame(&fnet_tun::Frame {
            port: Some(devices::TUN_DEFAULT_PORT_ID),
            frame_type: Some(fhardware_network::FrameType::Ipv4),
            data: Some(packet.clone()),
            meta: None,
            ..Default::default()
        })
        .await
        .expect("write_frame failed")
        .map_err(zx::Status::from_raw)
        .expect("write_frame returned error");

    // Read ping response.
    let (frame_type, data) = read_frame
        .try_next()
        .await
        .expect("failed to read ping response")
        .expect("frame stream ended unexpectedly");
    assert_eq!(frame_type, fhardware_network::FrameType::Ipv4);
    let mut bv = &data[..];
    let ipv4_packet = Ipv4Packet::parse(&mut bv, ()).expect("failed to parse IPv4 packet");
    assert_eq!(ipv4_packet.src_ip(), dst_ip);
    assert_eq!(ipv4_packet.dst_ip(), src_ip);
    assert_eq!(ipv4_packet.proto(), packet_formats::ip::Ipv4Proto::Icmp);

    let parse_args =
        packet_formats::icmp::IcmpParseArgs::new(ipv4_packet.src_ip(), ipv4_packet.dst_ip());
    let icmp_packet =
        match Icmpv4Packet::parse(&mut bv, parse_args).expect("failed to parse ICMP packet") {
            Icmpv4Packet::EchoReply(reply) => reply,
            p => panic!("got ICMP packet {:?}, want EchoReply", p),
        };
    assert_eq!(icmp_packet.message().id(), ICMP_ID);
    assert_eq!(icmp_packet.message().seq(), SEQ_NUM);

    let (inner_header, inner_body) = icmp_packet.body().bytes();
    assert!(inner_body.is_none());
    assert_eq!(inner_header, &payload[..]);

    // Send the same data again, but with an IPv6 frame type, expect that it'll
    // fail parsing and no response will be generated.
    assert_matches!(
        write_frame_and_read_with_timeout(
            &tun_dev,
            fnet_tun::Frame {
                port: Some(devices::TUN_DEFAULT_PORT_ID),
                frame_type: Some(fhardware_network::FrameType::Ipv6),
                data: Some(packet),
                meta: None,
                ..Default::default()
            },
            &mut read_frame,
        )
        .await,
        Ok(None)
    );

    // Manually build a V6 ping frame and see it come back out of the stack.
    let src_ip = Ipv6Addr::from_bytes(BOB_ADDR_V6.addr);
    let dst_ip = Ipv6Addr::from_bytes(ALICE_ADDR_V6.addr);
    let packet = packet::Buf::new(&mut payload[..], ..)
        .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
            src_ip,
            dst_ip,
            IcmpZeroCode,
            IcmpEchoRequest::new(ICMP_ID, SEQ_NUM),
        ))
        .encapsulate(Ipv6PacketBuilder::new(src_ip, dst_ip, 1, Ipv6Proto::Icmpv6))
        .serialize_vec_outer()
        .expect("serialization failed")
        .as_ref()
        .to_vec();

    // Send v6 ping request.
    let () = tun_dev
        .write_frame(&fnet_tun::Frame {
            port: Some(devices::TUN_DEFAULT_PORT_ID),
            frame_type: Some(fhardware_network::FrameType::Ipv6),
            data: Some(packet.clone()),
            meta: None,
            ..Default::default()
        })
        .await
        .expect("write_frame failed")
        .map_err(zx::Status::from_raw)
        .expect("write_frame returned error");

    // Read ping response.
    let (frame_type, data) = read_frame
        .try_next()
        .await
        .expect("failed to read ping response")
        .expect("frame stream ended unexpectedly");
    assert_eq!(frame_type, fhardware_network::FrameType::Ipv6);
    let mut bv = &data[..];
    let ipv6_packet = Ipv6Packet::parse(&mut bv, ()).expect("failed to parse IPv6 packet");
    assert_eq!(ipv6_packet.src_ip(), dst_ip);
    assert_eq!(ipv6_packet.dst_ip(), src_ip);
    assert_eq!(ipv6_packet.proto(), packet_formats::ip::Ipv6Proto::Icmpv6);

    let parse_args =
        packet_formats::icmp::IcmpParseArgs::new(ipv6_packet.src_ip(), ipv6_packet.dst_ip());
    let icmp_packet =
        match Icmpv6Packet::parse(&mut bv, parse_args).expect("failed to parse ICMPv6 packet") {
            Icmpv6Packet::EchoReply(reply) => reply,
            p => panic!("got ICMPv6 packet {:?}, want EchoReply", p),
        };
    assert_eq!(icmp_packet.message().id(), ICMP_ID);
    assert_eq!(icmp_packet.message().seq(), SEQ_NUM);

    let (inner_header, inner_body) = icmp_packet.body().bytes();
    assert!(inner_body.is_none());
    assert_eq!(inner_header, &payload[..]);

    // Send the same data again, but with an IPv4 frame type, expect that it'll
    // fail parsing and no response will be generated.
    assert_matches!(
        write_frame_and_read_with_timeout(
            &tun_dev,
            fnet_tun::Frame {
                port: Some(devices::TUN_DEFAULT_PORT_ID),
                frame_type: Some(fhardware_network::FrameType::Ipv4),
                data: Some(packet),
                meta: None,
                ..Default::default()
            },
            &mut read_frame,
        )
        .await,
        Ok(None)
    );
}

#[netstack_test]
#[variant(N, Netstack)]
async fn ping<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let create_realm = |suffix, addr| {
        let sandbox = &sandbox;
        let net = &net;
        async move {
            let realm = sandbox
                .create_netstack_realm::<N, _>(format!("{}_{}", name, suffix))
                .expect("failed to create realm");
            let interface = realm
                .join_network(&net, format!("ep_{}", suffix))
                .await
                .expect("failed to join network in realm");
            interface.add_address_and_subnet_route(addr).await.expect("configure address");
            interface.apply_nud_flake_workaround().await.expect("nud flake workaround");
            (realm, interface)
        }
    };

    let (realm_a, if_a) = create_realm("a", fidl_subnet!("192.168.1.1/16")).await;
    let (realm_b, if_b) = create_realm("b", fidl_subnet!("192.168.1.2/16")).await;

    let node_a = ping::Node::new_with_v4_and_v6_link_local(&realm_a, &if_a)
        .await
        .expect("failed to construct node A");
    let node_b = ping::Node::new_with_v4_and_v6_link_local(&realm_b, &if_b)
        .await
        .expect("failed to construct node B");

    node_a
        .ping_pairwise(std::slice::from_ref(&node_b))
        .await
        .expect("failed to ping between nodes");
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(net_addr_subnet!("192.0.2.1/32"); "v4")]
#[test_case(net_addr_subnet!("fe80::1234:5678:90ab:cdef/128"); "v6_link_local")]
#[test_case(net_addr_subnet!("2001:db8::1/128"); "v6")]
async fn ping_self<N: Netstack>(name: &str, addr: AddrSubnetEither) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let ep = sandbox.create_endpoint(name).await.expect("create endpoint");
    let interface =
        realm.install_endpoint(ep, InterfaceConfig::default()).await.expect("install endpoint");
    interface.add_address(addr.into_ext()).await.expect("add address");

    const UNSPECIFIED_PORT: u16 = 0;
    const PING_SEQ: u16 = 1;
    match addr {
        AddrSubnetEither::V4(v4) => {
            realm
                .ping_once::<Ipv4>(
                    std::net::SocketAddrV4::new(v4.addr().get().into(), UNSPECIFIED_PORT),
                    PING_SEQ,
                )
                .await
        }
        AddrSubnetEither::V6(v6) => {
            let v6 = v6.addr().get();
            realm
                .ping_once::<Ipv6>(
                    std::net::SocketAddrV6::new(
                        v6.into(),
                        UNSPECIFIED_PORT,
                        0,
                        if v6.is_unicast_link_local() {
                            u32::try_from(interface.id()).expect("interface ID should fit into u32")
                        } else {
                            0
                        },
                    ),
                    PING_SEQ,
                )
                .await
        }
    }
    .expect("ping self address");
}

enum SocketType {
    Udp,
    Tcp,
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(SocketType::Udp, true; "UDP specified")]
#[test_case(SocketType::Udp, false; "UDP unspecified")]
#[test_case(SocketType::Tcp, true; "TCP specified")]
#[test_case(SocketType::Tcp, false; "TCP unspecified")]
// Verify socket connectivity over loopback.
// The Netstack is expected to treat the unspecified address as loopback.
async fn socket_loopback_test<N: Netstack, I: Ip>(
    name: &str,
    socket_type: SocketType,
    specified: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");
    let address = specified
        .then_some(I::LOOPBACK_ADDRESS.get())
        .unwrap_or(I::UNSPECIFIED_ADDRESS)
        .to_ip_addr()
        .into_ext();

    match socket_type {
        SocketType::Udp => run_udp_socket_test(&realm, address, &realm, address).await,
        SocketType::Tcp => run_tcp_socket_test(&realm, address, &realm, address).await,
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(SocketType::Udp)]
#[test_case(SocketType::Tcp)]
async fn socket_clone_bind<N: Netstack>(name: &str, socket_type: SocketType) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = sandbox.create_network("net").await.expect("failed to create network");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let interface = realm.join_network(&network, "stack").await.expect("join network failed");
    interface
        .add_address_and_subnet_route(fidl_subnet!("192.168.1.10/16"))
        .await
        .expect("configure address");

    let socket = match socket_type {
        SocketType::Udp => realm
            .datagram_socket(
                fposix_socket::Domain::Ipv4,
                fposix_socket::DatagramSocketProtocol::Udp,
            )
            .await
            .expect("create UDP datagram socket"),
        SocketType::Tcp => realm
            .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
            .await
            .expect("create UDP datagram socket"),
    };

    // Call `Clone` on the FIDL channel to get a new socket backed by a new
    // handle. Just cloning the Socket isn't sufficient since that calls the
    // POSIX `dup()` which is handled completely within FDIO. Instead we
    // explicitly clone the underlying FD to get a new handle and transmogrify
    // that into a new Socket.
    let other_socket: socket2::Socket =
        fdio::create_fd(fdio::clone_fd(socket.as_fd()).expect("clone_fd failed"))
            .expect("create_fd failed")
            .into();

    // Since both sockets refer to the same resource, binding one will affect
    // the other's bound address.

    let bind_addr = std_socket_addr!("127.0.0.1:2048");
    socket.bind(&bind_addr.clone().into()).expect("bind should succeed");

    let local_addr = other_socket.local_addr().expect("local addr exists");
    assert_eq!(bind_addr, local_addr.as_socket().unwrap());
}

#[netstack_test]
#[variant(N, Netstack)]
async fn udp_sendto_unroutable_leaves_socket_bound<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = sandbox.create_network("net").await.expect("failed to create network");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let interface = realm.join_network(&network, "stack").await.expect("join network failed");
    interface
        .add_address_and_subnet_route(fidl_subnet!("192.168.1.10/16"))
        .await
        .expect("configure address");

    let socket = realm
        .datagram_socket(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .and_then(|d| DatagramSocket::new_from_socket(d).map_err(Into::into))
        .expect("create UDP datagram socket");

    let addr = std_socket_addr!("8.8.8.8:8080");
    let buf = [0; 8];
    let send_result = socket
        .send_to(&buf, addr.into())
        .await
        .map_err(|e| e.raw_os_error().and_then(fposix::Errno::from_primitive));
    assert_eq!(
        send_result,
        Err(Some(if N::VERSION == NetstackVersion::Netstack3 {
            // TODO(https://fxbug.dev/42051708): Figure out what code is expected
            // here and make Netstack2 and Netstack3 return codes consistent.
            fposix::Errno::Enetunreach
        } else {
            fposix::Errno::Ehostunreach
        }))
    );

    let bound_addr = socket.local_addr().expect("should be bound");
    let bound_ipv4 = bound_addr.as_socket_ipv4().expect("must be IPv4");
    assert_eq!(bound_ipv4.ip(), &std_ip_v4!("0.0.0.0"));
    assert_ne!(bound_ipv4.port(), 0);
}

#[async_trait]
trait MakeSocket: Sized {
    async fn new_in_realm<I: TestIpExt>(t: &netemul::TestRealm<'_>) -> Result<socket2::Socket>;

    fn from_socket(s: socket2::Socket) -> Result<Self>;
}

#[async_trait]
impl MakeSocket for UdpSocket {
    async fn new_in_realm<I: TestIpExt>(t: &netemul::TestRealm<'_>) -> Result<socket2::Socket> {
        t.datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp).await
    }

    fn from_socket(s: socket2::Socket) -> Result<Self> {
        UdpSocket::from_datagram(DatagramSocket::new_from_socket(s)?).map_err(Into::into)
    }
}

struct TcpSocket(socket2::Socket);

#[async_trait]
impl MakeSocket for TcpSocket {
    async fn new_in_realm<I: TestIpExt>(t: &netemul::TestRealm<'_>) -> Result<socket2::Socket> {
        t.stream_socket(I::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp).await
    }

    fn from_socket(s: socket2::Socket) -> Result<Self> {
        Ok(Self(s))
    }
}

#[derive(Debug)]
struct Interface<'a, A> {
    iface: TestInterface<'a>,
    ip: A,
}

#[derive(Debug)]
struct Network<'a, A> {
    peer_realm: netemul::TestRealm<'a>,
    peer_interface: Interface<'a, A>,
    _network: netemul::TestNetwork<'a>,
    multinic_interface: Interface<'a, A>,
}

/// Sets up [`num_peers`]+1 realms: `num_peers` peers and 1 multi-nic host. Each
/// peer is connected to the multi-nic host via a different network. Once the
/// hosts are set up and sockets initialized, the provided callback is called.
///
/// When `call_with_sockets` is invoked, all of these sockets are provided as
/// arguments. The first argument contains the sockets in the multi-NIC realm,
/// and the second argument is the socket in the peer realm.
///
/// NB: in order for callers to provide a `call_with_networks` that captures
/// its environment, we need to constrain the HRTB lifetime `'a` with
/// `'params: 'a`, i.e. "`'params`' outlives `'a`". Since "where" clauses are
/// unsupported for HRTB, the only way to do this is with an implied bound.
/// The type `&'a &'params ()` is only well-formed if `'params: 'a`, so adding
/// an argument of that type implies the bound.
/// See https://stackoverflow.com/a/72673740 for a more thorough explanation.
async fn with_multinic_and_peer_networks<
    'params,
    N: Netstack,
    I: TestIpExt,
    F: for<'a> FnOnce(
        Vec<Network<'a, I::Addr>>,
        &'a netemul::TestRealm<'a>,
        &'a &'params (),
    ) -> LocalBoxFuture<'a, ()>,
>(
    name: &str,
    num_peers: u8,
    subnet: net_types::ip::Subnet<I::Addr>,
    call_with_networks: F,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let sandbox = &sandbox;

    let multinic =
        sandbox.create_netstack_realm::<N, _>(format!("{name}_multinic")).expect("create realm");
    let multinic = &multinic;

    let networks: Vec<_> = future::join_all((0..num_peers).map(|i| async move {
        // Put all addresses in a single subnet, where the mult-nic host's
        // interface will have an address with a final octet of 1, and the peer
        // a final octet of 2.
        let ip = |host| -> I::Addr {
            I::map_ip(
                (subnet.network(), IpInvariant(host)),
                |(v4, IpInvariant(host))| {
                    let mut addr = v4.ipv4_bytes();
                    *addr.last_mut().unwrap() = host;
                    Ipv4Addr::new(addr)
                },
                |(v6, IpInvariant(host))| {
                    let mut addr = v6.ipv6_bytes();
                    *addr.last_mut().unwrap() = host;
                    net_types::ip::Ipv6Addr::from_bytes(addr)
                },
            )
        };
        let multinic_ip = ip(1);
        let peer_ip = ip(2);

        let network = sandbox.create_network(format!("net_{i}")).await.expect("create network");
        let (peer_realm, peer_interface) = {
            let peer = sandbox
                .create_netstack_realm::<N, _>(format!("{name}_peer_{i}"))
                .expect("create realm");
            let peer_iface = peer
                .join_network(&network, format!("peer-{i}-ep"))
                .await
                .expect("install interface in peer netstack");
            peer_iface
                .add_address_and_subnet_route(fnet::Subnet {
                    addr: peer_ip.to_ip_addr().into_ext(),
                    prefix_len: subnet.prefix(),
                })
                .await
                .expect("configure address");
            peer_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
            (peer, Interface { iface: peer_iface, ip: peer_ip.into() })
        };
        let multinic_interface = {
            let name = format!("multinic-ep-{i}");
            let multinic_iface =
                multinic.join_network(&network, name).await.expect("adding interface failed");
            multinic_iface
                .add_address_and_subnet_route(fnet::Subnet {
                    addr: multinic_ip.to_ip_addr().into_ext(),
                    prefix_len: subnet.prefix(),
                })
                .await
                .expect("configure address");
            multinic_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
            Interface { iface: multinic_iface, ip: multinic_ip.into() }
        };
        Network { peer_realm, peer_interface, _network: network, multinic_interface }
    }))
    .await;

    call_with_networks(networks, multinic, &&()).await
}

async fn with_multinic_and_peers<
    N: Netstack,
    S: MakeSocket,
    I: TestIpExt,
    F: FnOnce(Vec<MultiNicAndPeerConfig<S>>) -> R,
    R: Future<Output = ()>,
>(
    name: &str,
    num_peers: u8,
    subnet: net_types::ip::Subnet<I::Addr>,
    port: u16,
    call_with_sockets: F,
) {
    with_multinic_and_peer_networks::<N, I, _>(name, num_peers, subnet, |networks, multinic, ()| {
        Box::pin(async move {
            let config = future::join_all(networks.iter().map(
                |Network {
                     peer_realm,
                     peer_interface: Interface { iface: _, ip: peer_ip },
                     multinic_interface: Interface { iface: multinic_iface, ip: multinic_ip },
                     _network,
                 }| async move {
                    let multinic_socket = {
                        let socket = S::new_in_realm::<I>(multinic).await.expect("creating socket");

                        socket
                            .bind_device(Some(
                                multinic_iface
                                    .get_interface_name()
                                    .await
                                    .expect("get_name failed")
                                    .as_bytes(),
                            ))
                            .and_then(|()| {
                                socket.bind(
                                    &std::net::SocketAddr::from((
                                        std::net::Ipv4Addr::UNSPECIFIED,
                                        port,
                                    ))
                                    .into(),
                                )
                            })
                            .expect("failed to bind device");
                        S::from_socket(socket).expect("failed to create server socket")
                    };
                    let peer_socket = S::new_in_realm::<I>(&peer_realm)
                        .await
                        .and_then(|s| {
                            s.bind(
                                &std::net::SocketAddr::from((
                                    std::net::Ipv4Addr::UNSPECIFIED,
                                    port,
                                ))
                                .into(),
                            )?;
                            S::from_socket(s)
                        })
                        .expect("bind failed");
                    MultiNicAndPeerConfig {
                        multinic_socket,
                        multinic_ip: multinic_ip.clone().into(),
                        peer_socket,
                        peer_ip: peer_ip.clone().into(),
                    }
                },
            ))
            .await;

            call_with_sockets(config).await
        })
    })
    .await
}

#[netstack_test]
#[variant(N, Netstack)]
async fn udp_receive_on_bound_to_devices<N: Netstack>(name: &str) {
    const NUM_PEERS: u8 = 3;
    const PORT: u16 = 80;
    const BUFFER_SIZE: usize = 1024;
    with_multinic_and_peers::<N, UdpSocket, Ipv4, _, _>(
        name,
        NUM_PEERS,
        net_subnet_v4!("192.168.0.0/16"),
        PORT,
        |multinic_and_peers| async move {
            // Now send traffic from the peer to the addresses for each of the multinic
            // NICs. The traffic should come in on the correct sockets.

            futures::stream::iter(multinic_and_peers.iter())
                .for_each_concurrent(
                    None,
                    |MultiNicAndPeerConfig {
                         peer_socket,
                         multinic_ip,
                         peer_ip,
                         multinic_socket: _,
                     }| async move {
                        let buf = peer_ip.to_string();
                        let addr = (*multinic_ip, PORT).into();
                        assert_eq!(
                            peer_socket.send_to(buf.as_bytes(), addr).await.expect("send failed"),
                            buf.len()
                        );
                    },
                )
                .await;

            futures::stream::iter(multinic_and_peers.into_iter())
                .for_each_concurrent(
                    None,
                    |MultiNicAndPeerConfig {
                         multinic_socket,
                         peer_ip,
                         multinic_ip: _,
                         peer_socket: _,
                     }| async move {
                        let mut buffer = [0u8; BUFFER_SIZE];
                        let (len, send_addr) =
                            multinic_socket.recv_from(&mut buffer).await.expect("recv_from failed");

                        assert_eq!(send_addr, (peer_ip, PORT).into());
                        // The received packet should contain the IP address of the
                        // sending interface, which is also the source address.
                        let expected = peer_ip.to_string();
                        assert_eq!(len, expected.len());
                        assert_eq!(&buffer[..len], expected.as_bytes());
                    },
                )
                .await
        },
    )
    .await
}

#[netstack_test]
#[variant(N, Netstack)]
async fn udp_send_from_bound_to_device<N: Netstack>(name: &str) {
    const NUM_PEERS: u8 = 3;
    const PORT: u16 = 80;
    const BUFFER_SIZE: usize = 1024;

    with_multinic_and_peers::<N, UdpSocket, Ipv4, _, _>(
        name,
        NUM_PEERS,
        net_subnet_v4!("192.168.0.0/16"),
        PORT,
        |configs| async move {
            // Now send traffic from each of the multinic sockets to the
            // corresponding peer. The traffic should be sent from the address
            // corresponding to each socket's bound device.
            futures::stream::iter(configs.iter())
                .for_each_concurrent(
                    None,
                    |MultiNicAndPeerConfig {
                         multinic_ip,
                         multinic_socket,
                         peer_ip,
                         peer_socket: _,
                     }| async move {
                        let peer_addr = (*peer_ip, PORT).into();
                        let buf = multinic_ip.to_string();
                        assert_eq!(
                            multinic_socket
                                .send_to(buf.as_bytes(), peer_addr)
                                .await
                                .expect("send failed"),
                            buf.len()
                        );
                    },
                )
                .await;

            futures::stream::iter(configs)
            .for_each(
                |MultiNicAndPeerConfig {
                     peer_socket,
                     peer_ip: _,
                     multinic_ip: _,
                     multinic_socket: _,
                 }| async move {
                    let mut buffer = [0u8; BUFFER_SIZE];
                    let (len, source_addr) =
                        peer_socket.recv_from(&mut buffer).await.expect("recv_from failed");
                    let source_ip =
                        assert_matches!(source_addr, std::net::SocketAddr::V4(addr) => *addr.ip());
                    // The received packet should contain the IP address of the interface.
                    let expected = source_ip.to_string();
                    assert_eq!(len, expected.len());
                    assert_eq!(&buffer[..expected.len()], expected.as_bytes());
                },
            )
            .await;
        },
    )
    .await
}

#[netstack_test]
#[variant(N, Netstack)]
async fn test_udp_source_address_has_zone<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{}_client", name))
        .expect("failed to create client realm");
    let server = sandbox
        .create_netstack_realm::<N, _>(format!("{}_server", name))
        .expect("failed to create server realm");

    let client_ep = client
        .join_network_with(
            &net,
            "client",
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(CLIENT_MAC)),
            Default::default(),
        )
        .await
        .expect("client failed to join network");
    client_ep.add_address_and_subnet_route(Ipv6::CLIENT_SUBNET).await.expect("configure address");
    client_ep.apply_nud_flake_workaround().await.expect("apply NUD flake workaround");
    let server_ep = server
        .join_network_with(
            &net,
            "server",
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(SERVER_MAC)),
            Default::default(),
        )
        .await
        .expect("server failed to join network");
    server_ep.add_address_and_subnet_route(Ipv6::SERVER_SUBNET).await.expect("configure address");
    server_ep.apply_nud_flake_workaround().await.expect("apply NUD flake workaround");

    // Get the link local address for the client.
    let link_local_addr = std::pin::pin!(client
        .get_interface_event_stream()
        .expect("get_interface_event_stream failed")
        .filter_map(|event| async {
            match event.expect("event error").into_inner() {
                fnet_interfaces::Event::Existing(properties)
                | fnet_interfaces::Event::Added(properties) => {
                    if let Some(addresses) = properties.addresses {
                        for address in addresses {
                            if let Some(fnet::Subnet {
                                addr: fnet::IpAddress::Ipv6(addr), ..
                            }) = address.addr
                            {
                                if addr.is_unicast_link_local() {
                                    return Some(addr);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            None
        }))
    .next()
    .await
    .expect("unexpected end of events");

    let client_addr = assert_matches!(fnet::IpAddress::Ipv6(link_local_addr).into(),
                                      fnet_ext::IpAddress(std::net::IpAddr::V6(client_addr)) => client_addr);
    let client_addr = std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
        client_addr,
        1234,
        0,
        client_ep.id().try_into().unwrap(),
    ));

    let fnet_ext::IpAddress(server_addr) = fnet_ext::IpAddress::from(Ipv6::SERVER_SUBNET.addr);
    let server_addr = std::net::SocketAddr::new(server_addr, 8080);

    let client_sock = fasync::net::UdpSocket::bind_in_realm(&client, client_addr)
        .await
        .expect("failed to create client socket");

    let server_sock = fasync::net::UdpSocket::bind_in_realm(&server, server_addr)
        .await
        .expect("failed to create server socket");

    const PAYLOAD: &'static str = "Hello World";

    let client_fut = async move {
        let r = client_sock.send_to(PAYLOAD.as_bytes(), server_addr).await.expect("sendto failed");
        assert_eq!(r, PAYLOAD.as_bytes().len());
    };
    let server_fut = async move {
        let mut buf = [0u8; 1024];
        let (_, from) = server_sock.recv_from(&mut buf[..]).await.expect("recvfrom failed");
        // This will also check the zone.
        assert_eq!(from, client_addr);
    };

    let ((), ()) = futures::future::join(client_fut, server_fut).await;
}

#[netstack_test]
#[variant(N, Netstack)]
async fn tcp_connect_bound_to_device<N: Netstack>(name: &str) {
    const NUM_PEERS: u8 = 2;
    const PORT: u16 = 90;

    async fn connect_to_peer(
        config: MultiNicAndPeerConfig<TcpSocket>,
    ) -> MultiNicAndPeerConfig<fasync::net::TcpStream> {
        let MultiNicAndPeerConfig { multinic_ip, multinic_socket, peer_ip, peer_socket } = config;
        let (TcpSocket(peer_socket), TcpSocket(multinic_socket)) = (peer_socket, multinic_socket);
        peer_socket.listen(1).expect("listen on bound socket");
        let peer_socket =
            fasync::net::TcpListener::from_std(std::net::TcpListener::from(peer_socket))
                .expect("convert socket");

        let multinic_socket =
            fasync::net::TcpStream::connect_from_raw(multinic_socket, (peer_ip, PORT).into())
                .expect("start connect failed")
                .await
                .expect("connect failed");

        let (_peer_listener, peer_socket, ip): (fasync::net::TcpListener, _, _) =
            peer_socket.accept().await.expect("accept failed");

        assert_eq!(ip, (multinic_ip, PORT).into());
        MultiNicAndPeerConfig { multinic_ip, multinic_socket, peer_ip, peer_socket }
    }

    with_multinic_and_peers::<N, TcpSocket, Ipv4, _, _>(
        name,
        NUM_PEERS,
        net_subnet_v4!("192.168.0.0/16").into(),
        PORT,
        |configs| async move {
            let connected_configs = futures::stream::iter(configs)
                .map(connect_to_peer)
                .buffer_unordered(usize::MAX)
                .collect::<Vec<_>>()
                .await;

            futures::stream::iter(connected_configs)
                .enumerate()
                .for_each_concurrent(
                    None,
                    |(
                        i,
                        MultiNicAndPeerConfig {
                            multinic_ip: _,
                            mut multinic_socket,
                            peer_ip: _,
                            mut peer_socket,
                        },
                    )| async move {
                        let message = format!("send number {}", i);
                        futures::stream::iter([&mut multinic_socket, &mut peer_socket])
                            .for_each_concurrent(None, |socket| async {
                                assert_eq!(
                                    socket
                                        .write(message.as_bytes())
                                        .await
                                        .expect("host write succeeds"),
                                    message.len()
                                );

                                let mut buf = vec![0; message.len()];
                                socket.read_exact(&mut buf).await.expect("host read succeeds");
                                assert_eq!(&buf, message.as_bytes());
                            })
                            .await;
                    },
                )
                .await
        },
    )
    .await
}

#[netstack_test]
#[variant(N, Netstack)]
async fn get_bound_device_errors_after_device_deleted<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let host = sandbox.create_netstack_realm::<N, _>(format!("{name}_host")).expect("create realm");

    let bound_interface =
        host.join_network(&net, "bound-device").await.expect("host failed to join network");
    bound_interface
        .add_address_and_subnet_route(fidl_subnet!("192.168.0.1/16"))
        .await
        .expect("configure address");

    let host_sock =
        fasync::net::UdpSocket::bind_in_realm(&host, (std::net::Ipv4Addr::UNSPECIFIED, 0).into())
            .await
            .expect("failed to create host socket");

    host_sock
        .bind_device(Some(
            bound_interface.get_interface_name().await.expect("get_name failed").as_bytes(),
        ))
        .expect("set SO_BINDTODEVICE");

    let id = bound_interface.id();

    let interface_state =
        host.connect_to_protocol::<fnet_interfaces::StateMarker>().expect("connect to protocol");

    let stream =
        fnet_interfaces_ext::event_stream_from_state::<fnet_interfaces_ext::DefaultInterest>(
            &interface_state,
            fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .expect("error getting interface state event stream");
    let mut stream = pin!(stream);
    let mut state =
        std::collections::HashMap::<u64, fnet_interfaces_ext::PropertiesAndState<(), _>>::new();

    // Wait for the interface to be present.
    fnet_interfaces_ext::wait_interface(stream.by_ref(), &mut state, |interfaces| {
        interfaces.get(&id).map(|_| ())
    })
    .await
    .expect("waiting for interface addition");

    let (_endpoint, _device_control) =
        bound_interface.remove().await.expect("failed to remove interface");

    // Wait for the interface to be removed.
    fnet_interfaces_ext::wait_interface(stream, &mut state, |interfaces| {
        interfaces.get(&id).is_none().then(|| ())
    })
    .await
    .expect("waiting interface removal");

    let bound_device =
        host_sock.device().map_err(|e| e.raw_os_error().and_then(fposix::Errno::from_primitive));
    assert_eq!(bound_device, Err(Some(fposix::Errno::Enodev)));
}

struct MultiNicAndPeerConfig<S> {
    multinic_ip: net_types::ip::IpAddr,
    multinic_socket: S,
    peer_ip: net_types::ip::IpAddr,
    peer_socket: S,
}

#[netstack_test]
#[variant(N, Netstack)]
async fn send_to_remote_with_zone<N: Netstack>(name: &str) {
    const PORT: u16 = 80;
    const NUM_BYTES: usize = 10;

    async fn make_socket(realm: &netemul::TestRealm<'_>) -> fasync::net::UdpSocket {
        fasync::net::UdpSocket::bind_in_realm(realm, (std::net::Ipv6Addr::UNSPECIFIED, PORT).into())
            .await
            .expect("failed to create socket")
    }

    with_multinic_and_peer_networks::<N, net_types::ip::Ipv6, _>(
        name,
        2,
        net_types::ip::Ipv6::LINK_LOCAL_UNICAST_SUBNET,
        |networks, multinic, ()| {
            Box::pin(async move {
                let networks_and_peer_sockets =
                    future::join_all(networks.iter().map(|network| async move {
                        let Network { peer_realm, peer_interface, _network, multinic_interface } =
                            network;
                        let Interface { iface: _, ip: peer_ip } = peer_interface;
                        let peer_socket = make_socket(&peer_realm).await;
                        (multinic_interface, (peer_socket, *peer_ip))
                    }))
                    .await;

                let host_sock = make_socket(&multinic).await;
                let host_sock = &host_sock;

                let _: Vec<()> = future::join_all(networks_and_peer_sockets.iter().map(
                    |(multinic_interface, (peer_socket, peer_ip))| async move {
                        let Interface { iface: interface, ip: _ } = multinic_interface;
                        let id: u8 = interface.id().try_into().unwrap();
                        assert_eq!(
                            host_sock
                                .send_to(
                                    &[id; NUM_BYTES],
                                    std::net::SocketAddrV6::new(
                                        peer_ip.clone().into(),
                                        PORT,
                                        0,
                                        id.into()
                                    )
                                    .into(),
                                )
                                .await
                                .expect("send should succeed"),
                            NUM_BYTES
                        );

                        let mut buf = [0; NUM_BYTES + 1];
                        let (bytes, _sender) =
                            peer_socket.recv_from(&mut buf).await.expect("recv succeeds");
                        assert_eq!(bytes, NUM_BYTES);
                        assert_eq!(&buf[..NUM_BYTES], &[id; NUM_BYTES]);
                    },
                ))
                .await;
            })
        },
    )
    .await
}

async fn tcp_communicate_with_remote_with_zone<
    N: Netstack,
    M: for<'a, 's> Fn(
        &'s netemul::TestRealm<'a>,
        &'s Interface<'a, net_types::ip::Ipv6Addr>,
        net_types::ip::Ipv6Addr,
    ) -> LocalBoxFuture<'s, fasync::net::TcpStream>,
>(
    name: &str,
    make_multinic_conn: M,
) {
    const PORT: u16 = 80;
    const NUM_BYTES: usize = 10;

    let make_multinic_conn = &make_multinic_conn;
    with_multinic_and_peer_networks::<N, net_types::ip::Ipv6, _>(
        name,
        2,
        net_types::ip::Ipv6::LINK_LOCAL_UNICAST_SUBNET,
        |networks, multinic, ()| {
            Box::pin(async move {
                let interfaces_and_listeners =
                    future::join_all(networks.iter().map(|network| async move {
                        let Network { peer_realm, peer_interface, _network, multinic_interface } =
                            network;
                        let Interface { iface: _, ip: peer_ip } = peer_interface;
                        let peer_listener = fasync::net::TcpListener::listen_in_realm(
                            peer_realm,
                            (std::net::Ipv6Addr::UNSPECIFIED, PORT).into(),
                        )
                        .await
                        .expect("can listen");
                        (multinic_interface, (peer_listener, *peer_ip))
                    }))
                    .await;

                let _: Vec<()> = future::join_all(interfaces_and_listeners.into_iter().map(
                    |(multinic_interface, (peer_listener, peer_ip))| async move {
                        let mut host_conn =
                            make_multinic_conn(multinic, multinic_interface, peer_ip).await;
                        let id: u8 = multinic_interface.iface.id().try_into().unwrap();
                        let data = [id; NUM_BYTES];
                        let (_peer_listener, mut peer_conn, _) =
                            peer_listener.accept().await.expect("receive connection");
                        host_conn.write_all(&data).await.expect("can send");
                        host_conn.close().await.expect("can close");

                        let mut buf = Vec::with_capacity(data.len());
                        assert_eq!(
                            peer_conn.read_to_end(&mut buf).await.expect("can read"),
                            data.len()
                        );
                        assert_eq!(&buf, &data);
                    },
                ))
                .await;
            })
        },
    )
    .await
}

#[netstack_test]
#[variant(N, Netstack)]
async fn tcp_connect_to_remote_with_zone<N: Netstack>(name: &str) {
    match N::VERSION {
        NetstackVersion::Netstack2 { tracing: _, fast_udp: _ } | NetstackVersion::ProdNetstack2 => {
            ()
        }
        NetstackVersion::Netstack3 | NetstackVersion::ProdNetstack3 => {
            // TODO(https://fxbug.dev/42051508): Re-enable this once Netstack3
            // supports fallible device access.
            return;
        }
    }
    const PORT: u16 = 80;

    tcp_communicate_with_remote_with_zone::<N, _>(name, |realm, interface, peer_ip| {
        Box::pin(async move {
            let Interface { iface: interface, ip: _ } = interface;
            let id: u8 = interface.id().try_into().unwrap();
            fasync::net::TcpStream::connect_in_realm(
                realm,
                std::net::SocketAddrV6::new(peer_ip.clone().into(), PORT, 0, id.into()).into(),
            )
            .await
            .expect("can connect")
        })
    })
    .await
}

#[netstack_test]
#[variant(N, Netstack)]
async fn tcp_bind_with_zone_connect_unzoned<N: Netstack>(name: &str) {
    const PORT: u16 = 80;

    tcp_communicate_with_remote_with_zone::<N, _>(name, |realm, interface, peer_ip| {
        Box::pin(async move {
            let Interface { iface: interface, ip } = interface;
            let id: u8 = interface.id().try_into().unwrap();
            let socket = TcpSocket::new_in_realm::<Ipv6>(realm).await.expect("create TCP socket");
            socket
                .bind(&std::net::SocketAddrV6::new(ip.clone().into(), PORT, 0, id.into()).into())
                .expect("no conflict");
            let remote_addr = std::net::SocketAddrV6::new(peer_ip.clone().into(), PORT, 0, 0);
            fasync::net::TcpStream::connect_from_raw(socket, remote_addr.into())
                .expect("is connected")
                .await
                .expect("connected")
        })
    })
    .await
}

#[derive(PartialEq)]
enum ProtocolWithZirconSocket {
    Tcp,
    FastUdp,
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(ProtocolWithZirconSocket::Tcp)]
#[test_case(ProtocolWithZirconSocket::FastUdp)]
async fn zx_socket_rights<N: Netstack>(name: &str, protocol: ProtocolWithZirconSocket) {
    // TODO(https://fxbug.dev/42182397): Remove this test when Fast UDP is
    // supported by Netstack3.
    if matches!(N::VERSION, NetstackVersion::Netstack3 | NetstackVersion::ProdNetstack3)
        && protocol == ProtocolWithZirconSocket::FastUdp
    {
        return;
    }

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let netstack = match N::VERSION {
        NetstackVersion::Netstack2 { tracing: false, fast_udp: false } => sandbox
            .create_realm(
                format!("{}", name),
                [KnownServiceProvider::Netstack(NetstackVersion::Netstack2 {
                    fast_udp: true,
                    tracing: false,
                })],
            )
            .expect("create realm"),
        NetstackVersion::Netstack3 => {
            sandbox.create_netstack_realm::<N, _>(format!("{}", name)).expect("create realm")
        }
        v @ (NetstackVersion::Netstack2 { tracing: _, fast_udp: _ }
        | NetstackVersion::ProdNetstack2
        | NetstackVersion::ProdNetstack3) => panic!(
            "netstack_test should only be parameterized with Netstack2 or Netstack3: got {:?}",
            v
        ),
    };

    let provider = netstack
        .connect_to_protocol::<fposix_socket::ProviderMarker>()
        .expect("connect to socket provider");
    let socket = match protocol {
        ProtocolWithZirconSocket::Tcp => {
            let socket = provider
                .stream_socket(
                    fposix_socket::Domain::Ipv4,
                    fposix_socket::StreamSocketProtocol::Tcp,
                )
                .await
                .expect("call stream socket")
                .expect("request stream socket");
            let fposix_socket::StreamSocketDescribeResponse { socket, .. } =
                socket.into_proxy().describe().await.expect("call describe");
            socket
        }
        ProtocolWithZirconSocket::FastUdp => {
            let response = provider
                .datagram_socket(
                    fposix_socket::Domain::Ipv4,
                    fposix_socket::DatagramSocketProtocol::Udp,
                )
                .await
                .expect("call datagram socket")
                .expect("request datagram socket");
            let socket = match response {
                fposix_socket::ProviderDatagramSocketResponse::SynchronousDatagramSocket(_) => {
                    panic!("expected fast udp socket, got sync udp")
                }
                fposix_socket::ProviderDatagramSocketResponse::DatagramSocket(socket) => socket,
            };
            let fposix_socket::DatagramSocketDescribeResponse { socket, .. } =
                socket.into_proxy().describe().await.expect("call describe");
            socket
        }
    };

    let zx::HandleBasicInfo { rights, .. } = socket
        .expect("zircon socket returned by describe")
        .basic_info()
        .expect("get socket basic info");
    assert_eq!(
        rights.bits(),
        zx::sys::ZX_RIGHT_TRANSFER
            | zx::sys::ZX_RIGHT_WAIT
            | zx::sys::ZX_RIGHT_INSPECT
            | zx::sys::ZX_RIGHT_WRITE
            | zx::sys::ZX_RIGHT_READ
    );
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(Icmpv4DestUnreachableCode::DestNetworkUnreachable => libc::ENETUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::DestHostUnreachable => libc::EHOSTUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::DestProtocolUnreachable => libc::ENOPROTOOPT)]
#[test_case(Icmpv4DestUnreachableCode::DestPortUnreachable => libc::ECONNREFUSED)]
#[test_case(Icmpv4DestUnreachableCode::SourceRouteFailed => libc::EOPNOTSUPP)]
#[test_case(Icmpv4DestUnreachableCode::DestNetworkUnknown => libc::ENETUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::DestHostUnknown => libc::EHOSTDOWN)]
#[test_case(Icmpv4DestUnreachableCode::SourceHostIsolated => libc::ENONET)]
#[test_case(Icmpv4DestUnreachableCode::NetworkAdministrativelyProhibited => libc::ENETUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::HostAdministrativelyProhibited => libc::EHOSTUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::NetworkUnreachableForToS => libc::ENETUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::HostUnreachableForToS => libc::EHOSTUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::CommAdministrativelyProhibited => libc::EHOSTUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::HostPrecedenceViolation => libc::EHOSTUNREACH)]
#[test_case(Icmpv4DestUnreachableCode::PrecedenceCutoffInEffect => libc::EHOSTUNREACH)]
async fn tcp_icmp_error_v4<N: Netstack>(name: &str, code: Icmpv4DestUnreachableCode) -> i32 {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");
    let fake_ep = net.create_fake_endpoint().expect("failed to create fake endpoint");
    let fake_ep = &fake_ep;

    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{}_client", name))
        .expect("failed to create client realm");
    let client_interface =
        client.join_network(&net, "client-ep").await.expect("failed to join network in realm");
    client_interface
        .add_address_and_subnet_route(Ipv4::CLIENT_SUBNET)
        .await
        .expect("configure address");
    client_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let fnet_ext::IpAddress(server_ip) = Ipv4::SERVER_SUBNET.addr.into();
    let fnet_ext::IpAddress(client_ip) = Ipv4::CLIENT_SUBNET.addr.into();
    let server_mac = Mac::new(SERVER_MAC.octets);
    let fake_ep_loop = async move {
        fake_ep
            .frame_stream()
            .map(|r| r.expect("failed to read frame"))
            .for_each(|(frame, _dropped)| async move {
                let mut frame = &frame[..];
                let eth = packet_formats::ethernet::EthernetFrame::parse(
                    &mut frame,
                    packet_formats::ethernet::EthernetFrameLengthCheck::NoCheck,
                )
                .expect("error parsing ethernet frame");

                let mut frame_body = eth.body();
                let v4_packet = match Ipv4Packet::parse(&mut frame_body, ()) {
                    Ok(v4_packet) if v4_packet.proto() == IpProto::Tcp.into() => v4_packet,
                    _ => {
                        let arp = ArpPacketBuilder::<Mac, Ipv4Addr>::new(
                            ArpOp::Response,
                            server_mac,
                            assert_matches!(server_ip, std::net::IpAddr::V4(addr) => addr).into(),
                            eth.src_mac(),
                            assert_matches!(client_ip, std::net::IpAddr::V4(addr) => addr).into(),
                        )
                        .into_serializer()
                        .encapsulate(EthernetFrameBuilder::new(
                            server_mac,
                            eth.src_mac(),
                            EtherType::Arp,
                            ETHERNET_MIN_BODY_LEN_NO_TAG,
                        ))
                        .serialize_vec_outer()
                        .expect("failed to serialize ARP response")
                        .unwrap_b();
                        return fake_ep
                            .write(arp.as_ref())
                            .await
                            .expect("failed to write ARP response");
                    }
                };
                let mut body = eth.body().to_vec();
                let icmp_error = packet::Buf::new(&mut body, ..)
                    .encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                        v4_packet.dst_ip(),
                        v4_packet.src_ip(),
                        code,
                        IcmpDestUnreachable::default(),
                    ))
                    .encapsulate(Ipv4PacketBuilder::new(
                        v4_packet.dst_ip(),
                        v4_packet.src_ip(),
                        u8::MAX,
                        Ipv4Proto::Icmp.into(),
                    ))
                    .encapsulate(EthernetFrameBuilder::new(
                        eth.dst_mac(),
                        eth.src_mac(),
                        EtherType::Ipv4,
                        ETHERNET_MIN_BODY_LEN_NO_TAG,
                    ))
                    .serialize_vec_outer()
                    .expect("failed to serialize ICMP error")
                    .unwrap_b();
                fake_ep.write(icmp_error.as_ref()).await.expect("failed to write ICMP error");
            })
            .await;
    };

    let server_addr = std::net::SocketAddr::new(server_ip, 8080);

    let connect = async move {
        let error = fasync::net::TcpStream::connect_in_realm(&client, server_addr)
            .await
            .expect_err("connect should fail");
        let error = error.downcast::<std::io::Error>().expect("failed to cast to std::io::Result");
        error.raw_os_error()
    };

    futures::select! {
        () = fake_ep_loop.fuse() => unreachable!("should never finish"),
        errno = connect.fuse() => return errno.expect("must have an errno"),
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(Icmpv6DestUnreachableCode::NoRoute => libc::ENETUNREACH)]
#[test_case(Icmpv6DestUnreachableCode::PortUnreachable => libc::ECONNREFUSED)]
// TODO(https://fxbug.dev/42076684): Test against all possible codes once we can
// timeout in handshake states.
async fn tcp_icmp_error_v6<N: Netstack>(name: &str, code: Icmpv6DestUnreachableCode) -> i32 {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");
    let fake_ep = net.create_fake_endpoint().expect("failed to create fake endpoint");
    let fake_ep = &fake_ep;

    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{}_client", name))
        .expect("failed to create client realm");
    let client_interface =
        client.join_network(&net, "client-ep").await.expect("failed to join network in realm");
    client_interface
        .add_address_and_subnet_route(Ipv6::CLIENT_SUBNET)
        .await
        .expect("configure address");
    client_interface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let fnet_ext::IpAddress(server_ip) = Ipv6::SERVER_SUBNET.addr.into();
    let fnet_ext::IpAddress(client_ip) = Ipv6::CLIENT_SUBNET.addr.into();
    let server_mac = Mac::new(SERVER_MAC.octets);
    let fake_ep_loop = async move {
        fake_ep
            .frame_stream()
            .map(|r| r.expect("failed to read frame"))
            .for_each(|(frame, _dropped)| async move {
                let mut frame = &frame[..];
                let eth = packet_formats::ethernet::EthernetFrame::parse(
                    &mut frame,
                    packet_formats::ethernet::EthernetFrameLengthCheck::NoCheck,
                )
                .expect("error parsing ethernet frame");
                let server_ip: net_types::ip::Ipv6Addr =
                    assert_matches!(server_ip, std::net::IpAddr::V6(addr) => addr).into();
                let client_ip: net_types::ip::Ipv6Addr =
                    assert_matches!(client_ip, std::net::IpAddr::V6(addr) => addr).into();
                let mut frame_body = eth.body();
                let v6_packet = match Ipv6Packet::parse(&mut frame_body, ()) {
                    Ok(v6_packet) if v6_packet.proto() == IpProto::Tcp.into() => v6_packet,
                    _ => {
                        let options =
                            &[NdpOptionBuilder::TargetLinkLayerAddress(&SERVER_MAC.octets)];
                        return ndp::write_message::<&[u8], _>(
                            server_mac,
                            eth.src_mac(),
                            server_ip,
                            client_ip,
                            NeighborAdvertisement::new(false, true, false, server_ip),
                            options,
                            fake_ep,
                        )
                        .await
                        .expect("failed to send neighbor advertisement");
                    }
                };
                let mut body = eth.body().to_vec();
                let icmp_error = packet::Buf::new(&mut body, ..)
                    .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                        v6_packet.dst_ip(),
                        v6_packet.src_ip(),
                        code,
                        IcmpDestUnreachable::default(),
                    ))
                    .encapsulate(packet_formats::ipv6::Ipv6PacketBuilder::new(
                        v6_packet.dst_ip(),
                        v6_packet.src_ip(),
                        ipv6_consts::DEFAULT_HOP_LIMIT,
                        Ipv6Proto::Icmpv6.into(),
                    ))
                    .encapsulate(EthernetFrameBuilder::new(
                        eth.dst_mac(),
                        eth.src_mac(),
                        EtherType::Ipv6,
                        ETHERNET_MIN_BODY_LEN_NO_TAG,
                    ))
                    .serialize_vec_outer()
                    .expect("failed to serialize")
                    .unwrap_b();
                fake_ep.write(icmp_error.as_ref()).await.expect("failed to write to fake ep");
            })
            .await;
    };

    let server_addr = std::net::SocketAddr::new(server_ip, 8080);
    let connect = async move {
        let error = fasync::net::TcpStream::connect_in_realm(&client, server_addr)
            .await
            .expect_err("connect should fail");
        let error = error.downcast::<std::io::Error>().expect("failed to cast to std::io::Result");
        error.raw_os_error()
    };

    futures::select! {
        () = fake_ep_loop.fuse() => unreachable!("should never finish"),
        errno = connect.fuse() => return errno.expect("must have an errno"),
    }
}

/// Tests that a connection pending in an accept queue can be accepted and
/// returns the expected scope id even if the device the scope id matches has
/// been removed from the stack.
#[netstack_test]
#[variant(N, Netstack)]
async fn tcp_accept_with_removed_device_scope<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let net = sandbox.create_network("net").await.expect("failed to create network");

    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");

    let server = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_server"))
        .expect("failed to create client realm");

    let client_iface =
        client.join_network(&net, "client-ep").await.expect("failed to join network");
    let server_iface =
        server.join_network(&net, "server-ep").await.expect("failed to join network");

    async fn get_ll_addr(
        realm: &netemul::TestRealm<'_>,
        ep: &netemul::TestInterface<'_>,
    ) -> std::net::Ipv6Addr {
        let interfaces_state = realm
            .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
            .expect("connect to protocol");
        netstack_testing_common::interfaces::wait_for_v6_ll(&interfaces_state, ep.id())
            .await
            .expect("wait LL address")
            .into()
    }

    let server_addr = get_ll_addr(&server, &server_iface).await;
    let client_addr = get_ll_addr(&client, &client_iface).await;

    const PORT: u16 = 8080;
    let server_sock = fasync::net::TcpListener::listen_in_realm(
        &server,
        std::net::SocketAddrV6::new(std::net::Ipv6Addr::UNSPECIFIED, PORT, 0, 0).into(),
    )
    .await
    .expect("listen in realm");

    // We need to notify that we want readable so that fuchsia_async clears the
    // cached readable signals within _before_ we actually start the connection
    // process so we can wait for readable later with a clean slate.
    futures::future::poll_fn(|cx| {
        server_sock.need_read(cx);
        futures::task::Poll::Ready(())
    })
    .await;

    let client_sock = fasync::net::TcpStream::connect_in_realm(
        &client,
        std::net::SocketAddrV6::new(
            server_addr.into(),
            PORT,
            0,
            client_iface.id().try_into().unwrap(),
        )
        .into(),
    )
    .await
    .expect("connect");

    let client_port = client_sock.std().local_addr().expect("local addr").port();

    let server_scope: u32 = server_iface.id().try_into().unwrap();

    // Ensure that the connection is ready to be accepted, the server socket
    // must be readable.
    futures::future::poll_fn(|cx| server_sock.poll_readable(cx))
        .await
        .expect("polling server socket");

    server_iface
        .control()
        .remove()
        .await
        .expect("requesting removal")
        .expect("failed to request removal");
    assert_eq!(
        server_iface.wait_removal().await.expect("waiting removal"),
        fnet_interfaces_admin::InterfaceRemovedReason::User
    );

    let (_server_sock, _connection, from) = server_sock.accept().await.expect("accept failed");
    let v6_addr = assert_matches!(from, std::net::SocketAddr::V6(v6) => v6);
    assert_eq!(v6_addr.ip(), &client_addr);
    assert_eq!(v6_addr.port(), client_port);
    assert_eq!(v6_addr.scope_id(), server_scope);
}

trait MulticastTestIpExt:
    packet_formats::ip::IpExt
    + packet_formats::ip::IpProtoExt
    + packet_formats::icmp::IcmpIpExt
    + TestIpExt
{
    const NETWORKS: [fnet::Subnet; 2];
    const MCAST_ADDR: std::net::SocketAddr;

    fn iface_ip(index: usize) -> std::net::IpAddr {
        match Self::NETWORKS[index].addr {
            fnet::IpAddress::Ipv4(addr) => std::net::IpAddr::V4(addr.addr.into()),
            fnet::IpAddress::Ipv6(addr) => std::net::IpAddr::V6(addr.addr.into()),
        }
    }
}

impl MulticastTestIpExt for Ipv4 {
    const NETWORKS: [fnet::Subnet; 2] =
        [fidl_subnet!("50.28.45.23/24"), fidl_subnet!("10.0.0.1/24")];
    const MCAST_ADDR: std::net::SocketAddr = std_socket_addr!("224.0.0.5:3513");
}

impl MulticastTestIpExt for Ipv6 {
    const NETWORKS: [fnet::Subnet; 2] =
        [fidl_subnet!("2001:db8::1/64"), fidl_subnet!("2001:ac::1/64")];

    // Use site-local address to ensure that a global address is picked for
    // the connection and not a link-local one.
    const MCAST_ADDR: std::net::SocketAddr = std_socket_addr!("[FF05::1:2]:3513");
}

struct MulticastTestNetwork<'a> {
    _net: TestNetwork<'a>,
    iface: TestInterface<'a>,
    receiver: TestFakeEndpoint<'a>,
}

async fn init_multicast_test_networks<'a, I: MulticastTestIpExt>(
    sandbox: &'a netemul::TestSandbox,
    client: &netemul::TestRealm<'a>,
) -> Vec<MulticastTestNetwork<'a>> {
    future::join_all(I::NETWORKS.iter().enumerate().map(|(i, subnet)| async move {
        let net =
            sandbox.create_network(format!("net{i}")).await.expect("failed to create network");
        let iface =
            client.join_network(&net, format!("if{i}")).await.expect("failed to join network");
        iface.add_address_and_subnet_route(subnet.clone()).await.expect("failed to set ip");
        let receiver = net.create_fake_endpoint().expect("failed to create endpoint");
        MulticastTestNetwork { _net: net, iface, receiver }
    }))
    .await
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(0)]
#[test_case(1)]
async fn multicast_send<N: Netstack, I: MulticastTestIpExt>(name: &str, target_interface: usize) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");
    let networks = init_multicast_test_networks::<I>(&sandbox, &client).await;

    let sock = client
        .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("failed to create socket");

    match I::VERSION {
        IpVersion::V4 => {
            let addr = match I::NETWORKS[target_interface].addr {
                fnet::IpAddress::Ipv4(a) => a.addr.into(),
                fnet::IpAddress::Ipv6(_) => unreachable!("NETWORKS expected to be Ipv4"),
            };
            sock.set_multicast_if_v4(&addr).expect("failed to set IP_MULTICAST_IF")
        }
        IpVersion::V6 => sock
            .set_multicast_if_v6(networks[target_interface].iface.id().try_into().unwrap())
            .expect("failed to set IPV6_MULTICAST_IF"),
    };

    let _ = sock
        .send_to(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], &I::MCAST_ADDR.into())
        .expect("failed to send multicast packet");

    // Check that the packet is sent to the selected network.
    for (index, network) in networks.iter().enumerate() {
        let mut stream = std::pin::pin!(network
            .receiver
            .frame_stream()
            .map(|r| r.expect("failed to read frame"))
            .filter_map(|(data, dropped)| async move {
                assert_eq!(dropped, 0);
                let (_payload, _src_mac, _dst_mac, _src_ip, dst_ip, proto, _ttl) =
                    match packet_formats::testutil::parse_ip_packet_in_ethernet_frame::<I>(
                        &data[..],
                        EthernetFrameLengthCheck::NoCheck,
                    ) {
                        Ok(result) => result,
                        Err(_e) => {
                            // Packet may fail to parse if it was for a
                            // different IP version. Just skip it.
                            return None;
                        }
                    };

                if proto != IpProto::Udp.into() {
                    return None;
                }

                if dst_ip.to_ip_addr() != I::MCAST_ADDR.ip().into() {
                    panic!("UDP Packet send to an unexpected address: {:?}", dst_ip);
                }

                Some(())
            }));

        if index == target_interface {
            // Check that the packet is delivered to the target interface.
            stream
                .next()
                .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
                    panic!("timed out waiting for the multicast packet")
                })
                .await
                .expect("didn't receive the packet before end of the stream");
        } else {
            // Check that the packet is not sent to the other interface.
            stream
                .next()
                .map(|_| panic!("MulticastPacket was sent to a wrong interface"))
                .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || ())
                .await;
        }
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(None, 0, false)]
#[test_case(Some(true), 0, false)]
#[test_case(Some(true), 1, true)]
#[test_case(Some(false), 0, false)]
#[test_case(Some(false), 1, true)]
async fn multicast_loop<N: Netstack, I: MulticastTestIpExt>(
    name: &str,
    multicast_loop_value: Option<bool>,
    target_interface: usize,
    dual_stack: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");

    let networks = init_multicast_test_networks::<I>(&sandbox, &client).await;

    // Initialize send socket to send the packet on the `target_interface`.
    let send_socket = client
        .datagram_socket(
            if dual_stack { Ipv6::DOMAIN } else { I::DOMAIN },
            fposix_socket::DatagramSocketProtocol::Udp,
        )
        .await
        .expect("failed to create UDP socket");

    match I::VERSION {
        IpVersion::V4 => {
            let addr = match I::NETWORKS[target_interface].addr {
                fnet::IpAddress::Ipv4(a) => a.addr.into(),
                fnet::IpAddress::Ipv6(_) => unreachable!("NETWORKS expected to be Ipv4"),
            };
            send_socket.set_multicast_if_v4(&addr).expect("failed to set IP_MULTICAST_IF");
            if let Some(value) = multicast_loop_value {
                send_socket.set_multicast_loop_v4(value).expect("failed to set IP_MULTICAST_LOOP");
            }
        }
        IpVersion::V6 => {
            let iface_id = networks[target_interface].iface.id().try_into().unwrap();
            send_socket.set_multicast_if_v6(iface_id).expect("Failed to set IPV6_MULTICAST_LOOP");
            if let Some(value) = multicast_loop_value {
                send_socket
                    .set_multicast_loop_v6(value)
                    .expect("failed to set IPV6_MULTICAST_LOOP");

                // Set the IPv4 option to the reverse value. It's expected to
                // have no effect on IPv6 packets. NS2 doesn't implement this
                // correctly, so we only set this option in NS3.
                if N::VERSION == NetstackVersion::Netstack3 {
                    send_socket
                        .set_multicast_loop_v4(!value)
                        .expect("failed to set IP_MULTICAST_LOOP");
                }
            }
        }
    };

    // Create one socket per interface and join the same multicast group from each.
    let recv_sockets = future::join_all(networks.iter().map(|network| async {
        let recv_socket = client
            .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
            .await
            .expect("failed to create socket");
        recv_socket
            .bind_device(Some(
                network
                    .iface
                    .get_interface_name()
                    .await
                    .expect("get_interface_name failed")
                    .as_bytes(),
            ))
            .expect("failed to bind socket to an interface");
        recv_socket.bind(&I::MCAST_ADDR.into()).expect("failed to bind UDP socket");

        let iface_id = network.iface.id().try_into().unwrap();
        match I::MCAST_ADDR.ip() {
            std::net::IpAddr::V4(addr_v4) => recv_socket
                .join_multicast_v4_n(&addr_v4.into(), &InterfaceIndexOrAddress::Index(iface_id))
                .expect("failed to join multicast group"),
            std::net::IpAddr::V6(addr_v6) => recv_socket
                .join_multicast_v6(&addr_v6.into(), iface_id)
                .expect("failed to join multicast group"),
        }
        fasync::net::UdpSocket::from_socket(recv_socket.into()).unwrap()
    }))
    .await;

    // IP_MULTICAST_LOOP should be enabled if not set explicitly.
    let multicast_loop_value = multicast_loop_value.unwrap_or(true);

    let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    assert_eq!(
        send_socket.send_to(&data, &I::MCAST_ADDR.into()).expect("failed to send multicast packet"),
        data.len()
    );

    // Check that the packet is delivered where it's expected.
    for (i, recv_socket) in recv_sockets.iter().enumerate() {
        let mut buf = [0u8; 200];
        let recv_fut = recv_socket.recv_from(&mut buf);
        let packet_expected = multicast_loop_value && i == target_interface;
        if packet_expected {
            let (size, addr) = recv_fut
                .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || {
                    Err(std::io::ErrorKind::TimedOut.into())
                })
                .await
                .expect("recv_from failed");
            assert_eq!(size, data.len());
            assert_eq!(&buf[..size], &data[..]);
            assert_eq!(addr.ip(), I::iface_ip(i));
        } else {
            recv_fut
                .map(|output| panic!("unexpected received packet {output:?}"))
                .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || ())
                .await;
        }
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(true)]
#[test_case(false)]
async fn multicast_loop_on_loopback_dev<N: Netstack, I: MulticastTestIpExt>(
    name: &str,
    multicast_loop_value: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");

    let loopback_id: u32 =
        client.loopback_properties().await.unwrap().unwrap().id.get().try_into().unwrap();

    // Initialize send socket to send the packet on the `target_interface`.
    let send_socket = client
        .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("failed to create UDP socket");
    let loopback_ip: std::net::IpAddr = I::LOOPBACK_ADDRESS.to_ip_addr().into();
    send_socket
        .bind(&std::net::SocketAddr::new(loopback_ip, 0).into())
        .expect("failed to bind UDP socket");

    match I::VERSION {
        IpVersion::V4 => send_socket.set_multicast_loop_v4(multicast_loop_value),
        IpVersion::V6 => send_socket.set_multicast_loop_v6(multicast_loop_value),
    }
    .expect("failed to set IPV6_MULTICAST_LOOP");

    let recv_socket = client
        .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("failed to create socket");
    recv_socket.bind(&I::MCAST_ADDR.into()).expect("failed to bind UDP socket");

    match I::MCAST_ADDR.ip() {
        std::net::IpAddr::V4(addr_v4) => recv_socket
            .join_multicast_v4_n(&addr_v4.into(), &InterfaceIndexOrAddress::Index(loopback_id))
            .expect("failed to join multicast group"),
        std::net::IpAddr::V6(addr_v6) => recv_socket
            .join_multicast_v6(&addr_v6.into(), loopback_id)
            .expect("failed to join multicast group"),
    }

    let recv_socket = fasync::net::UdpSocket::from_socket(recv_socket.into()).unwrap();

    let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    assert_eq!(
        send_socket.send_to(&data, &I::MCAST_ADDR.into()).expect("failed to send multicast packet"),
        data.len()
    );

    // `recv_socket` is expected to receive one and only one packet.
    let mut buf = [0u8; 200];
    let (size, addr) = recv_socket
        .recv_from(&mut buf)
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || Err(std::io::ErrorKind::TimedOut.into()))
        .await
        .expect("recv_from failed");
    assert_eq!(size, data.len());
    assert_eq!(&buf[..size], &data[..]);
    assert_eq!(addr.ip(), loopback_ip);

    recv_socket
        .recv_from(&mut buf)
        .map(|output| panic!("unexpected received duplicate packet {output:?}"))
        .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || ())
        .await;
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
#[test_case(None, true; "default_should_loop")]
#[test_case(Some(true), true; "enabled_should_loop")]
#[test_case(Some(false), false; "disabled_shouldnt_loop")]
async fn multicast_loop_on_raw_ip_socket<N: Netstack, I: MulticastTestIpExt>(
    name: &str,
    multicast_loop_value: Option<bool>,
    should_receive: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");
    let networks = init_multicast_test_networks::<I>(&sandbox, &client).await;

    // NB: Ensure we send the packet over a non-loopback interface, as that
    // would defeat the purpose of the multicast_loop test.
    let iface = &networks[0].iface;

    let send_socket = client
        .raw_socket(
            I::DOMAIN,
            fposix_socket_raw::ProtocolAssociation::Associated(IpProto::Udp.into()),
        )
        .await
        .expect("failed to create socket");
    send_socket
        .bind_device(Some(
            iface.get_interface_name().await.expect("get_interface_name failed").as_bytes(),
        ))
        .expect("failed to bind socket to an interface");

    if let Some(multicast_loop) = multicast_loop_value {
        match I::VERSION {
            IpVersion::V4 => send_socket.set_multicast_loop_v4(multicast_loop),
            IpVersion::V6 => send_socket.set_multicast_loop_v6(multicast_loop),
        }
        .expect("failed to set IPV6_MULTICAST_LOOP");
    }

    let recv_socket = client
        .raw_socket(
            I::DOMAIN,
            fposix_socket_raw::ProtocolAssociation::Associated(IpProto::Udp.into()),
        )
        .await
        .expect("failed to create socket");
    let recv_socket = DatagramSocket::new_from_socket(recv_socket).unwrap();

    // NB: Multicast traffic is dropped before being delivered to raw IP sockets
    // if we don't have any interest in the packet. Register a UDP socket
    // with interest.
    let _multicast_interested_sock = {
        let socket = client
            .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
            .await
            .expect("failed to create socket");
        let iface_id = u32::try_from(iface.id()).unwrap();
        match I::MCAST_ADDR.ip() {
            std::net::IpAddr::V4(addr_v4) => socket
                .join_multicast_v4_n(&addr_v4.into(), &InterfaceIndexOrAddress::Index(iface_id))
                .expect("failed to join multicast group"),
            std::net::IpAddr::V6(addr_v6) => socket
                .join_multicast_v6(&addr_v6.into(), iface_id)
                .expect("failed to join multicast group"),
        }
        socket
    };

    let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    assert_eq!(
        send_socket.send_to(&data, &I::MCAST_ADDR.into()).expect("failed to send multicast packet"),
        data.len()
    );

    let mut buf = [0u8; 200];
    let recv_fut = recv_socket.recv_from(&mut buf);
    if should_receive {
        let (size, _addr) = recv_fut
            .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || {
                Err(std::io::ErrorKind::TimedOut.into())
            })
            .await
            .expect("recv_from failed");
        match I::VERSION {
            // NB: Raw IPv4 Sockets receive the full IP Header
            IpVersion::V4 => {
                let buffer = packet::Buf::new(buf, 0..size);
                let packet =
                    Ipv4Packet::parse(&mut buffer.as_ref(), ()).expect("parse should succeed");
                assert_eq!(packet.body(), &data[..]);
            }
            IpVersion::V6 => assert_eq!(&buf[..size], &data[..]),
        }
    } else {
        recv_fut
            .map(|output| panic!("unexpected received packet {output:?}"))
            .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || ())
            .await;
    }
}

trait RedirectTestIpExt: Ip {
    const SUBNET: fnet::Subnet;
    const ADDR: std::net::IpAddr;
}

impl RedirectTestIpExt for Ipv4 {
    const SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const ADDR: std::net::IpAddr = std_ip!("192.0.2.1");
}

impl RedirectTestIpExt for Ipv6 {
    const SUBNET: fnet::Subnet = fidl_subnet!("2001:db8::1/64");
    const ADDR: std::net::IpAddr = std_ip!("2001:db8::1");
}

struct RedirectTestSetup<'a> {
    netstack: TestRealm<'a>,
    _network: TestNetwork<'a>,
    _interface: TestInterface<'a>,
    _control: fnet_filter::ControlProxy,
    _controller: fnet_filter_ext::Controller,
}

async fn setup_redirect_test<'a>(
    name: &str,
    sandbox: &'a TestSandbox,
    subnet: fnet::Subnet,
    matcher: fnet_filter_ext::TransportProtocolMatcher,
    redirect: Option<RangeInclusive<NonZeroU16>>,
) -> RedirectTestSetup<'a> {
    use fnet_filter_ext::{
        Action, Change, Controller, ControllerId, Domain, InstalledNatRoutine, Matchers, Namespace,
        NamespaceId, NatHook, Resource, Routine, RoutineId, RoutineType, Rule, RuleId,
    };

    let netstack =
        sandbox.create_netstack_realm::<Netstack3, _>(name.to_owned()).expect("create netstack");
    let network = sandbox.create_network("net").await.expect("create network");
    let interface = netstack.join_network(&network, "interface").await.expect("join network");
    interface.add_address_and_subnet_route(subnet).await.expect("set ip");

    let control =
        netstack.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller = Controller::new(&control, &ControllerId(String::from("redirect")))
        .await
        .expect("create controller");
    let namespace_id = NamespaceId(String::from("namespace"));
    let routine_id = RoutineId { namespace: namespace_id.clone(), name: String::from("routine") };
    let resources = [
        Resource::Namespace(Namespace { id: namespace_id.clone(), domain: Domain::AllIp }),
        Resource::Routine(Routine {
            id: routine_id.clone(),
            routine_type: RoutineType::Nat(Some(InstalledNatRoutine {
                hook: NatHook::LocalEgress,
                priority: 0,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: routine_id.clone(), index: 0 },
            matchers: Matchers { transport_protocol: Some(matcher), ..Default::default() },
            action: Action::Redirect { dst_port: redirect.map(fnet_filter_ext::PortRange) },
        }),
    ];
    controller
        .push_changes(resources.iter().cloned().map(Change::Create).collect())
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");

    RedirectTestSetup {
        netstack,
        _network: network,
        _interface: interface,
        _control: control,
        _controller: controller,
    }
}

const LISTEN_PORT: NonZeroU16 = NonZeroU16::new(11111).unwrap();

struct TestCaseV4 {
    original_dst: std::net::SocketAddr,
    matcher: fnet_filter_ext::TransportProtocolMatcher,
    redirect_dst: Option<RangeInclusive<NonZeroU16>>,
    expect_redirect: bool,
}

#[netstack_test]
#[test_case(
    TestCaseV4 {
        original_dst: std::net::SocketAddr::new(Ipv4::ADDR, LISTEN_PORT.get()),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Tcp { src_port: None, dst_port: None },
        redirect_dst: None,
        expect_redirect: true,
    };
    "redirect to localhost"
)]
#[test_case(
    TestCaseV4 {
        original_dst: std::net::SocketAddr::new(Ipv4::ADDR, 22222),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Tcp { src_port: None, dst_port: None },
        redirect_dst: Some(LISTEN_PORT..=LISTEN_PORT),
        expect_redirect: true,
    };
    "redirect to localhost port 11111"
)]
#[test_case(
    TestCaseV4 {
        original_dst: std::net::SocketAddr::new(std_ip!("127.0.0.1"), LISTEN_PORT.get()),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Udp { src_port: None, dst_port: None },
        redirect_dst: None,
        expect_redirect: false,
    };
    "no redirect"
)]
async fn redirect_original_destination_v4(name: &str, test_case: TestCaseV4) {
    let TestCaseV4 { original_dst, matcher, redirect_dst, expect_redirect } = test_case;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let setup = setup_redirect_test(name, &sandbox, Ipv4::SUBNET, matcher, redirect_dst).await;

    let server = setup
        .netstack
        .stream_socket(Ipv4::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create socket");
    server
        .bind(
            &std::net::SocketAddr::from((Ipv4::LOOPBACK_ADDRESS.to_ip_addr(), LISTEN_PORT.get()))
                .into(),
        )
        .expect("no conflict");
    server.listen(1).expect("listen on server socket");

    let client = setup
        .netstack
        .stream_socket(Ipv4::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create socket");
    client.connect(&original_dst.into()).expect("connect to server");

    let (server, _addr) = server.accept().expect("accept incoming connection");

    // The original destination should be observable on both the client and server sockets.
    let verify_original_dst = |socket: &socket2::Socket| {
        let result = socket.original_dst();
        if expect_redirect {
            assert_eq!(
                result
                    .expect("get original destination of connection")
                    .as_socket()
                    .expect("should be valid socket addr"),
                original_dst
            );
        } else {
            let error = result.expect_err("socket should have no original destination").kind();
            assert_eq!(error, std::io::ErrorKind::NotFound);
        }
    };
    verify_original_dst(&client);
    verify_original_dst(&server);
}

struct TestCaseV6 {
    original_dst: std::net::SocketAddr,
    matcher: fnet_filter_ext::TransportProtocolMatcher,
    redirect_dst: Option<RangeInclusive<NonZeroU16>>,
}

#[netstack_test]
#[test_case(
    TestCaseV6 {
        original_dst: std::net::SocketAddr::new(Ipv6::ADDR, LISTEN_PORT.get()),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Tcp { src_port: None, dst_port: None },
        redirect_dst: None,
    };
    "redirect to localhost"
)]
#[test_case(
    TestCaseV6 {
        original_dst: std::net::SocketAddr::new(Ipv6::ADDR, 22222),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Tcp { src_port: None, dst_port: None },
        redirect_dst: Some(LISTEN_PORT..=LISTEN_PORT),
    };
    "redirect to localhost port 11111"
)]
#[test_case(
    TestCaseV6 {
        original_dst: std::net::SocketAddr::new(std_ip!("::1"), LISTEN_PORT.get()),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Udp { src_port: None, dst_port: None },
        redirect_dst: None,
    };
    "no redirect"
)]
async fn redirect_original_destination_v6(name: &str, test_case: TestCaseV6) {
    let TestCaseV6 { original_dst, matcher, redirect_dst } = test_case;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let setup = setup_redirect_test(name, &sandbox, Ipv6::SUBNET, matcher, redirect_dst).await;

    let server = setup
        .netstack
        .stream_socket(Ipv6::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create socket");
    server
        .bind(
            &std::net::SocketAddr::from((Ipv6::LOOPBACK_ADDRESS.to_ip_addr(), LISTEN_PORT.get()))
                .into(),
        )
        .expect("no conflict");
    server.listen(1).expect("listen on server socket");

    let client = setup
        .netstack
        .stream_socket(Ipv6::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create socket");
    client.connect(&original_dst.into()).expect("connect to server");

    let (server, _addr) = server.accept().expect("accept incoming connection");

    // Although this connection was redirected, SO_ORIGINAL_DST should return
    // ENOENT because the original destination was not an IPv4 address.
    let verify_original_dst = |socket: &socket2::Socket| {
        let result = socket.original_dst();
        let error = result.expect_err("socket should have no original destination").kind();
        assert_eq!(error, std::io::ErrorKind::NotFound);
    };
    verify_original_dst(&client);
    verify_original_dst(&server);

    // TODO(https://fxbug.dev/345465222): exercise SOL_IPV6 - IP6T_SO_ORIGINAL_DST
    // when it is available and implemented.
}

#[netstack_test]
#[test_case(
    TestCaseV4 {
        original_dst: std::net::SocketAddr::new(Ipv4::ADDR, LISTEN_PORT.get()),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Tcp { src_port: None, dst_port: None },
        redirect_dst: None,
        expect_redirect: true,
    };
    "redirect to localhost"
)]
#[test_case(
    TestCaseV4 {
        original_dst: std::net::SocketAddr::new(Ipv4::ADDR, 22222),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Tcp { src_port: None, dst_port: None },
        redirect_dst: Some(LISTEN_PORT..=LISTEN_PORT),
        expect_redirect: true,
    };
    "redirect to localhost port 11111"
)]
#[test_case(
    TestCaseV4 {
        original_dst: std::net::SocketAddr::new(std_ip!("127.0.0.1"), LISTEN_PORT.get()),
        matcher: fnet_filter_ext::TransportProtocolMatcher::Udp { src_port: None, dst_port: None },
        redirect_dst: None,
        expect_redirect: false,
    };
    "no redirect"
)]
async fn redirect_original_destination_dual_stack(name: &str, test_case: TestCaseV4) {
    let TestCaseV4 { original_dst, matcher, redirect_dst, expect_redirect } = test_case;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let setup = setup_redirect_test(name, &sandbox, Ipv4::SUBNET, matcher, redirect_dst).await;

    let server = setup
        .netstack
        .stream_socket(Ipv6::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create socket");
    server
        .bind(
            &std::net::SocketAddr::from((
                Ipv6::UNSPECIFIED_ADDRESS.to_ip_addr(),
                LISTEN_PORT.get(),
            ))
            .into(),
        )
        .expect("no conflict");
    server.listen(1).expect("listen on server socket");

    let client = setup
        .netstack
        .stream_socket(Ipv4::DOMAIN, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create socket");
    client.connect(&original_dst.into()).expect("connect to server");

    let (server, _addr) = server.accept().expect("accept incoming connection");

    // The original destination should be observable on both the client and server sockets.
    let verify_original_dst = |socket: &socket2::Socket| {
        let result = socket.original_dst();
        if expect_redirect {
            assert_eq!(
                result
                    .expect("get original destination of connection")
                    .as_socket()
                    .expect("should be valid socket addr"),
                original_dst
            );
        } else {
            let error = result.expect_err("socket should have no original destination").kind();
            assert_eq!(error, std::io::ErrorKind::NotFound);
        }
    };
    verify_original_dst(&client);
    verify_original_dst(&server);
}

#[netstack_test]
#[variant(N, Netstack)]
async fn broadcast_recv<N: Netstack>(name: &str) {
    const SUBNET: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const PORT: u16 = 3513;

    const SRC_IP: net_types::ip::Ipv4Addr = net_ip_v4!("192.0.2.2");
    const SRC_PORT: u16 = 2141;
    const DST_IP: net_types::ip::Ipv4Addr = net_ip_v4!("192.0.2.255");

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");
    let net = sandbox.create_network(format!("net0")).await.expect("failed to create network");
    let iface = client.join_network(&net, format!("if0")).await.expect("failed to join network");
    iface.add_address_and_subnet_route(SUBNET.clone()).await.expect("failed to set ip");
    let fake_ep = net.create_fake_endpoint().expect("failed to create endpoint");

    let sockets = future::join_all(std::iter::repeat(()).take(2).map(|()| async {
        let socket = client
            .datagram_socket(Ipv4::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
            .await
            .expect("Failed to create UDP socket");

        socket.set_reuse_port(true).expect("failed to set SO_REUSEPORT");

        socket
            .bind(
                &std::net::SocketAddr::from((Ipv4::UNSPECIFIED_ADDRESS.to_ip_addr(), PORT)).into(),
            )
            .expect("failed to bind socket");

        fasync::net::UdpSocket::from_socket(socket.into()).unwrap()
    }))
    .await;

    let mut test_packet = [1, 2, 3, 4, 5];
    let broadcast_packet = packet::Buf::new(&mut test_packet, ..)
        .encapsulate(UdpPacketBuilder::new(
            SRC_IP,
            DST_IP,
            core::num::NonZero::new(SRC_PORT),
            core::num::NonZero::new(PORT).unwrap(),
        ))
        .encapsulate(Ipv4PacketBuilder::new(SRC_IP, DST_IP, /*ttl=*/ 30, IpProto::Udp.into()))
        .encapsulate(EthernetFrameBuilder::new(
            /*src_mac=*/ netstack_testing_common::constants::eth::MAC_ADDR,
            /*dst_mac=*/ net_types::ethernet::Mac::BROADCAST,
            EtherType::Ipv4,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .expect("failed to serialize UDP packet")
        .unwrap_b();
    fake_ep.write(broadcast_packet.as_ref()).await.expect("failed to write UDP packet");

    // Check that the packet was delivered to all sockets.
    for socket in sockets.iter() {
        let mut buf = [0u8; 1024];
        let (size, _addr) = socket
            .recv_from(&mut buf)
            .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || {
                panic!("Broadcast packet wasn't delivered to a listening socket")
            })
            .await
            .expect("recv_from failed");
        assert_eq!(size, test_packet.len());
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
async fn broadcast_send<N: Netstack, I: TestIpExt>(name: &str) {
    const NETWORK: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    const PORT: u16 = 3513;
    const BROADCAST_ADDR: std::net::SocketAddr = std_socket_addr!("192.0.2.255:3513");

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");

    let net = sandbox.create_network(format!("net0")).await.expect("failed to create network");
    let iface = client.join_network(&net, format!("if0")).await.expect("failed to join network");
    iface.add_address_and_subnet_route(NETWORK.clone()).await.expect("failed to set ip");
    let receiver = net.create_fake_endpoint().expect("failed to create endpoint");

    let recv_socket = client
        .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("failed to create socket");
    recv_socket
        .bind(&std::net::SocketAddr::from((I::UNSPECIFIED_ADDRESS.to_ip_addr(), PORT)).into())
        .expect("failed to bind socket");
    let recv_socket = fasync::net::UdpSocket::from_socket(recv_socket.into()).unwrap();

    let socket = client
        .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("failed to create socket");

    assert_eq!(socket.broadcast().expect("getsockopt(SO_BROADCAST) failed"), false);

    let test_packet = [1, 2, 3, 4, 5];
    let err = socket
        .send_to(&test_packet, &BROADCAST_ADDR.into())
        .expect_err("sendto is expected to fail to send broadcast packets by default");
    assert_eq!(err.raw_os_error(), Some(libc::EACCES));

    socket.set_broadcast(true).expect("failed to set SO_BROADCAST");
    assert_eq!(socket.broadcast().expect("getsockopt(SO_BROADCAST) failed"), true);

    assert_eq!(
        socket
            .send_to(&test_packet, &BROADCAST_ADDR.into())
            .expect("failed to send broadcast packet"),
        test_packet.len()
    );

    // Check that the packet is sent to the network.
    std::pin::pin!(receiver.frame_stream().map(|r| r.expect("failed to read frame")).filter_map(
        |(data, dropped)| async move {
            assert_eq!(dropped, 0);
            let (_payload, _src_mac, _dst_mac, _src_ip, dst_ip, proto, _ttl) =
                match packet_formats::testutil::parse_ip_packet_in_ethernet_frame::<Ipv4>(
                    &data[..],
                    EthernetFrameLengthCheck::NoCheck,
                ) {
                    Ok(result) => result,
                    Err(_e) => {
                        // Packet may fail to parse if it was for a
                        // different IP version. Just skip it.
                        return None;
                    }
                };

            if proto != IpProto::Udp.into() {
                return None;
            }

            assert_eq!(dst_ip.to_ip_addr(), BROADCAST_ADDR.ip().into());

            Some(())
        }
    ))
    .next()
    .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
        panic!("timed out waiting for the multicast packet")
    })
    .await
    .expect("didn't receive the packet before end of the stream");

    // Check that the packet is delivered to local sockets.
    let mut buf = [0u8; 1024];
    let (size, _addr) = recv_socket
        .recv_from(&mut buf)
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || {
            panic!("Broadcast packet wasn't delivered to a listening socket")
        })
        .await
        .expect("recv_from failed");
    assert_eq!(size, test_packet.len());
}

#[netstack_test]
#[variant(N, Netstack)]
#[variant(I, Ip)]
async fn tos_tclass_send<
    N: Netstack,
    I: TestIpExt + packet_formats::ethernet::EthernetIpExt + packet_formats::ip::IpExt,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let client = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");

    let net = sandbox.create_network(format!("net0")).await.expect("failed to create network");
    let iface = client.join_network(&net, format!("if0")).await.expect("failed to join network");
    iface.add_address_and_subnet_route(I::CLIENT_SUBNET.clone()).await.expect("failed to set ip");
    let receiver = net.create_fake_endpoint().expect("failed to create endpoint");

    // Add a neighbor entry to ensure the packet is sent without having to resolve MAC.
    client
        .add_neighbor_entry(iface.id(), I::SERVER_SUBNET.addr.clone(), SERVER_MAC)
        .await
        .expect("add_neighbor_entry");

    let socket = client
        .datagram_socket(I::DOMAIN, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("failed to create socket");

    let fnet_ext::IpAddress(dst_ip) = fnet_ext::IpAddress::from(I::SERVER_SUBNET.addr);
    let dst_addr = std::net::SocketAddr::new(dst_ip, 3513);

    let socket: std::net::UdpSocket = socket.into();
    let traffic_class = 0xa7;
    let r = match I::VERSION {
        IpVersion::V4 => unsafe {
            let v = traffic_class as libc::c_int;
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&socket),
                libc::IPPROTO_IP,
                libc::IP_TOS,
                &v as *const libc::c_int as *const libc::c_void,
                std::mem::size_of_val(&v) as u32,
            )
        },
        IpVersion::V6 => unsafe {
            let v = traffic_class as libc::c_int;
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&socket),
                libc::IPPROTO_IPV6,
                libc::IPV6_TCLASS,
                &v as *const libc::c_int as *const libc::c_void,
                std::mem::size_of_val(&v) as u32,
            )
        },
    };
    assert_eq!(r, 0, "Failed to set TOS/TCLASS option");

    let test_packet = [1, 2, 3, 4, 5];
    assert_eq!(
        socket.send_to(&test_packet, &dst_addr).expect("failed to send multicast packet"),
        test_packet.len()
    );

    // Check that the packet is sent to the network.
    std::pin::pin!(receiver.frame_stream().map(|r| r.expect("failed to read frame")).filter_map(
        |(data, dropped)| async move {
            assert_eq!(dropped, 0);
            let (mut body, _src_mac, _dst_mac, ethertype) =
                packet_formats::testutil::parse_ethernet_frame(
                    &data,
                    EthernetFrameLengthCheck::NoCheck,
                )
                .expect("Failed to parse ethernet packet");
            if ethertype != Some(I::ETHER_TYPE) {
                return None;
            }

            let ip_packet = <I::Packet<_> as packet::ParsablePacket<_, _>>::parse(&mut body, ())
                .expect("Failed to parse IP packet");
            use packet_formats::ip::IpPacket;
            if ip_packet.proto() != IpProto::Udp.into() {
                return None;
            }

            let received_traffic_class = ip_packet.dscp_and_ecn().raw();
            assert_eq!(traffic_class, received_traffic_class);

            Some(())
        }
    ))
    .next()
    .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
        panic!("timed out waiting for the UDP packet packet")
    })
    .await
    .expect("didn't receive the packet before end of the stream");
}

#[netstack_test]
#[test_matrix(
    [fposix_socket::Domain::Ipv4, fposix_socket::Domain::Ipv6],
    [fposix_socket::DatagramSocketProtocol::Udp, fposix_socket::DatagramSocketProtocol::IcmpEcho],
    [fposix_socket::MarkDomain::Mark1, fposix_socket::MarkDomain::Mark2],
    [
        fposix_socket::OptionalUint32::Unset(fposix_socket::Empty),
        fposix_socket::OptionalUint32::Value(0)
    ]
)]
async fn datagram_socket_mark(
    name: &str,
    domain: fposix_socket::Domain,
    proto: fposix_socket::DatagramSocketProtocol,
    mark_domain: fposix_socket::MarkDomain,
    mark: fposix_socket::OptionalUint32,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm =
        sandbox.create_netstack_realm::<Netstack3, _>(name).expect("failed to create client realm");
    let sock =
        realm.datagram_socket(domain, proto).await.expect("failed to create datagram socket");
    let channel = fdio::clone_channel(sock).expect("failed to clone channel");
    let proxy = fposix_socket::BaseSocketProxy::new(fidl::AsyncChannel::from_channel(channel));
    proxy.set_mark(mark_domain, &mark).await.expect("fidl error").expect("set mark");
    assert_eq!(proxy.get_mark(mark_domain).await.expect("fidl error").expect("get mark"), mark);
}

#[netstack_test]
#[test_matrix(
    [fposix_socket::Domain::Ipv4, fposix_socket::Domain::Ipv6],
    [fposix_socket::MarkDomain::Mark1, fposix_socket::MarkDomain::Mark2],
    [
        fposix_socket::OptionalUint32::Unset(fposix_socket::Empty),
        fposix_socket::OptionalUint32::Value(0)
    ]
)]
async fn stream_socket_mark(
    name: &str,
    domain: fposix_socket::Domain,
    mark_domain: fposix_socket::MarkDomain,
    mark: fposix_socket::OptionalUint32,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm =
        sandbox.create_netstack_realm::<Netstack3, _>(name).expect("failed to create client realm");
    let sock = realm
        .stream_socket(domain, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("failed to create datagram socket");
    let channel = fdio::clone_channel(sock).expect("failed to clone channel");
    let proxy = fposix_socket::BaseSocketProxy::new(fidl::AsyncChannel::from_channel(channel));
    proxy.set_mark(mark_domain, &mark).await.expect("fidl error").expect("set mark");
    assert_eq!(proxy.get_mark(mark_domain).await.expect("fidl error").expect("get mark"), mark);
}

#[netstack_test]
#[test_matrix(
    [fposix_socket::Domain::Ipv4, fposix_socket::Domain::Ipv6],
    [
        fposix_socket_raw::ProtocolAssociation::Unassociated(fposix_socket_raw::Empty),
        fposix_socket_raw::ProtocolAssociation::Associated(0)
    ],
    [fposix_socket::MarkDomain::Mark1, fposix_socket::MarkDomain::Mark2],
    [
        fposix_socket::OptionalUint32::Unset(fposix_socket::Empty),
        fposix_socket::OptionalUint32::Value(0)
    ]
)]
async fn raw_socket_mark(
    name: &str,
    domain: fposix_socket::Domain,
    proto: fposix_socket_raw::ProtocolAssociation,
    mark_domain: fposix_socket::MarkDomain,
    mark: fposix_socket::OptionalUint32,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm =
        sandbox.create_netstack_realm::<Netstack3, _>(name).expect("failed to create client realm");
    let sock = realm.raw_socket(domain, proto).await.expect("failed to create datagram socket");
    let channel = fdio::clone_channel(sock).expect("failed to clone channel");
    let proxy = fposix_socket::BaseSocketProxy::new(fidl::AsyncChannel::from_channel(channel));
    proxy.set_mark(mark_domain, &mark).await.expect("fidl error").expect("set mark");
    assert_eq!(proxy.get_mark(mark_domain).await.expect("fidl error").expect("get mark"), mark);
}

#[netstack_test]
#[variant(N, Netstack)]
async fn udp_send_backpressure<N: Netstack>(name: &str) {
    const CLIENT_ADDR: fnet::Subnet = fidl_subnet!("192.0.2.1/24");
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox
        .create_netstack_realm::<N, _>(format!("{name}_client"))
        .expect("failed to create client realm");

    let (tun_device, _device) = devices::create_tun_device_with(fnet_tun::DeviceConfig {
        blocking: Some(true),
        ..Default::default()
    });
    let (port, client_port) =
        devices::create_ip_tun_port(&tun_device, devices::TUN_DEFAULT_PORT_ID).await;
    port.set_online(true).await.expect("set port online");
    let (_id, _interface_control, _device_control) =
        install_ip_device(&realm, client_port, [CLIENT_ADDR]).await;

    let socket = realm
        .datagram_socket(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("failed to create socket");
    // Set the send buffer size to the minimum possible.
    socket.set_send_buffer_size(0).expect("setting send buffer size");
    // Create an async nonblock socket.
    let socket = DatagramSocket::new_from_socket(socket).expect("creating async socket");
    const PAYLOAD: &[u8] = b"Hello";

    let server_addr = std_socket_addr!("192.0.2.2:8080");

    // Write into the socket until we observe EWOULDBLOCK, i.e., the send future
    // doesn't resolve immediately.
    let mut sent = 0;
    while let Some(r) = socket.send_to(PAYLOAD, server_addr.into()).now_or_never() {
        assert_matches!(r, Ok(_));
        sent += 1;
    }
    // At least one frame must've been sent.
    assert_ne!(sent, 0);

    // Create a new future that should unblock only when we read frames.
    let mut fut = socket.send_to(PAYLOAD, server_addr.into());
    assert_matches!(futures::poll!(&mut fut), Poll::Pending);

    // Wait for all sent frames to show up in the device queue.
    while sent != 0 {
        let fnet_tun::Frame { data, frame_type, .. } =
            tun_device.read_frame().await.expect("got frame").expect("reading frame");
        let data = data.expect("missing data");
        let frame_type = frame_type.expect("missing frame type");
        if frame_type != fhardware_network::FrameType::Ipv4 {
            continue;
        }
        let mut body = &data[..];
        let ipv4 = Ipv4Packet::parse(&mut body, ()).expect("failed to parse IPv4 packet");
        if ipv4.proto() == IpProto::Udp.into() {
            sent -= 1;
        }
    }
    // Future should unblock now that we've allowed the frames to be popped from
    // the device FIFO.
    assert_eq!(fut.await.expect("send_to error"), PAYLOAD.len());
}
