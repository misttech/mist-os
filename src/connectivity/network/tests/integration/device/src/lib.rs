// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::{convert::TryFrom as _, num::NonZeroU64};

use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_tun as fnet_tun;
use fidl_fuchsia_posix_socket_packet as fposix_socket_packet;

use fidl_fuchsia_net_ext::IntoExt as _;
use fuchsia_async::TimeoutExt as _;
use futures::{FutureExt as _, SinkExt as _, StreamExt as _, TryStreamExt as _};
use net_declare::{fidl_mac, fidl_subnet, std_socket_addr_v4};
use netstack_testing_common::{
    realms::{Netstack, TestSandboxExt as _},
    ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use packet::{Buf, ParsablePacket as _, Serializer};
use packet_formats::error::ParseError;
use packet_formats::ethernet::{
    EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck,
    ETHERNET_HDR_LEN_NO_TAG, ETHERNET_MIN_BODY_LEN_NO_TAG,
};
use packet_formats::icmp::MessageBody as _;
use packet_formats::ipv4::Ipv4Header as _;
use sockaddr::{EthernetSockaddr, IntoSockAddr as _};
use static_assertions::const_assert_eq;
use test_case::test_case;

const SOURCE_SUBNET: fnet::Subnet = fidl_subnet!("192.168.255.1/16");
const TARGET_SUBNET: fnet::Subnet = fidl_subnet!("192.168.254.1/16");
const SOURCE_MAC_ADDRESS: fnet::MacAddress = fidl_mac!("02:00:00:00:00:01");
const TARGET_MAC_ADDRESS: fnet::MacAddress = fidl_mac!("02:00:00:00:00:02");

/// An MTU that will result in an ICMP packet that is entirely used. Since the
/// payload length is rounded down to the nearest 8 bytes, this value minus the
/// IP header and the ethernet header must be divisible by 8.
const FULLY_USABLE_MTU: usize = 1490;

// Verifies that `FULLY_USABLE_MTU` corresponds to an MTU value that can have
// all bytes leveraged when sending a ping.
const_assert_eq!(
    max_icmp_payload_length(FULLY_USABLE_MTU),
    possible_icmp_payload_length(FULLY_USABLE_MTU)
);

/// Sends a single ICMP echo request from `source_realm` to `addr`, and waits
/// for the echo reply.
///
/// The body of the ICMP packet will be filled with `payload_length` 0 bytes.
/// Panics if the ping fails.
async fn expect_successful_ping<Ip: ping::IpExt>(
    source_realm: &netemul::TestRealm<'_>,
    addr: Ip::Addr,
    payload_length: usize,
) {
    let icmp_sock = source_realm.icmp_socket::<Ip>().await.expect("failed to create ICMP socket");

    let payload: Vec<u8> = vec![0; payload_length];
    let (mut sink, mut stream) = ping::new_unicast_sink_and_stream::<Ip, _, { u16::MAX as usize }>(
        &icmp_sock, &addr, &payload,
    );

    const SEQ: u16 = 1;
    sink.send(SEQ).await.expect("failed to send ping");
    let got = stream
        .try_next()
        .await
        .expect("echo reply failure")
        .expect("echo stream ended unexpectedly");
    assert_eq!(got, SEQ);
}

#[derive(Debug, PartialEq)]
struct IcmpPacketMetadata {
    source_address: fnet::IpAddress,
    target_address: fnet::IpAddress,
    payload_length: usize,
}

#[derive(Debug, PartialEq)]
enum IcmpEvent {
    EchoRequest(IcmpPacketMetadata),
    EchoReply(IcmpPacketMetadata),
}

// TODO(https://fxbug.dev/91971): Replace this with a shared solution.
fn to_fidl_address(addr: net_types::ip::Ipv4Addr) -> fnet::IpAddress {
    fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: addr.ipv4_bytes() })
}

/// Extracts an Ipv4 `IcmpEvent` from the provided `data`.
fn extract_icmp_event(ipv4_packet: &packet_formats::ipv4::Ipv4Packet<&[u8]>) -> Option<IcmpEvent> {
    let fidl_src_ip = to_fidl_address(ipv4_packet.src_ip());
    let fidl_dst_ip = to_fidl_address(ipv4_packet.dst_ip());

    if ipv4_packet.proto() != packet_formats::ip::Ipv4Proto::Icmp {
        // Ignore non-ICMP packets.
        return None;
    }

    let mut payload = ipv4_packet.body();
    let icmp_packet = packet_formats::icmp::Icmpv4Packet::parse(
        &mut payload,
        packet_formats::icmp::IcmpParseArgs::new(ipv4_packet.src_ip(), ipv4_packet.dst_ip()),
    )
    .expect("failed to parse ICMP packet");

    match icmp_packet {
        packet_formats::icmp::Icmpv4Packet::EchoRequest(echo) => {
            Some(IcmpEvent::EchoRequest(IcmpPacketMetadata {
                source_address: fidl_src_ip,
                target_address: fidl_dst_ip,
                payload_length: echo.body().len(),
            }))
        }
        packet_formats::icmp::Icmpv4Packet::EchoReply(echo) => {
            Some(IcmpEvent::EchoReply(IcmpPacketMetadata {
                source_address: fidl_src_ip,
                target_address: fidl_dst_ip,
                payload_length: echo.body().len(),
            }))
        }
        packet_formats::icmp::Icmpv4Packet::DestUnreachable(_)
        | packet_formats::icmp::Icmpv4Packet::ParameterProblem(_)
        | packet_formats::icmp::Icmpv4Packet::Redirect(_)
        | packet_formats::icmp::Icmpv4Packet::TimeExceeded(_)
        | packet_formats::icmp::Icmpv4Packet::TimestampReply(_)
        | packet_formats::icmp::Icmpv4Packet::TimestampRequest(_) => None,
    }
}

/// Returns the maximum payload length of a packet given the `mtu`.
const fn max_icmp_payload_length(mtu: usize) -> usize {
    // Based on the following logic:
    // https://osscs.corp.google.com/fuchsia/fuchsia/+/main:third_party/golibs/vendor/gvisor.dev/gvisor/pkg/tcpip/network/ipv4/ipv4.go;l=402;drc=42abed01bbe5e1fb34f17f64f63f6e7ba27a74c7
    // The max payload length is rounded down to the nearest number that is
    // divisible by 8 bytes.
    ((mtu
        - packet_formats::ethernet::testutil::ETHERNET_HDR_LEN_NO_TAG
        - packet_formats::ipv4::testutil::IPV4_MIN_HDR_LEN)
        & !7)
        - ping::ICMP_HEADER_LEN
}

/// Returns the maximum number of bytes that an ICMP packet body could
/// potentially fill given an `mtu`.
///
/// The returned value excludes the relevant headers.
const fn possible_icmp_payload_length(mtu: usize) -> usize {
    mtu - packet_formats::ipv4::testutil::IPV4_MIN_HDR_LEN
        - packet_formats::ethernet::testutil::ETHERNET_HDR_LEN_NO_TAG
        - ping::ICMP_HEADER_LEN
}

/// Returns a stream of `IcmpEvent`s from the provided `fake_endpoint`.
fn icmp_event_stream<'a>(
    fake_endpoint: &'a netemul::TestFakeEndpoint<'_>,
) -> impl futures::Stream<Item = IcmpEvent> + 'a {
    fake_endpoint.frame_stream().map(|r| r.expect("error getting OnData event")).filter_map(
        |(data, _dropped)| async move {
            let mut data = &data[..];

            let eth = packet_formats::ethernet::EthernetFrame::parse(
                &mut data,
                packet_formats::ethernet::EthernetFrameLengthCheck::NoCheck,
            )
            .expect("error parsing ethernet frame");

            if eth.ethertype() != Some(packet_formats::ethernet::EtherType::Ipv4) {
                // Ignore non-IPv4 packets.
                return None;
            }

            let mut frame_body = eth.body();
            let ipv4_packet = packet_formats::ipv4::Ipv4Packet::parse(&mut frame_body, ())
                .expect("failed to parse IPv4 packet");

            match ipv4_packet.fragment_type() {
                packet_formats::ipv4::Ipv4FragmentType::InitialFragment => {
                    extract_icmp_event(&ipv4_packet)
                }
                // Only consider initial fragments as we are simply verifying that the
                // payload length is of an expected size for a given MTU. Note that
                // non-initial fragments do not contain an ICMP header and therefore
                // would not parse as an ICMP packet
                packet_formats::ipv4::Ipv4FragmentType::NonInitialFragment => None,
            }
        },
    )
}

#[netstack_test]
#[test_case(
    "fragmented",
    netemul::DEFAULT_MTU.into(),
    // Set the `payload_length` to a value larger than the `mtu`. This will
    // force the ICMP packets to be fragmented.
    (netemul::DEFAULT_MTU + 100).into(),
    max_icmp_payload_length(netemul::DEFAULT_MTU.into());
    "fragmented")]
#[test_case(
    "fully_used_mtu",
    FULLY_USABLE_MTU,
    possible_icmp_payload_length(FULLY_USABLE_MTU),
    possible_icmp_payload_length(FULLY_USABLE_MTU);
    "fully used mtu"
)]
async fn ping_succeeds_with_expected_payload<N: Netstack>(
    name: &str,
    sub_name: &str,
    mtu: usize,
    payload_length: usize,
    // Corresponds to the ICMP packet body length. This length excludes the
    // relevant headers (e.g. ICMP, ethernet, and IP). When fragmented, only the
    // first packet is considered.
    expected_payload_length: usize,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = sandbox
        .create_network(format!("network_{}_{}", name, sub_name))
        .await
        .expect("failed to create network");
    let source_realm = sandbox
        .create_netstack_realm::<N, _>(format!("source_{}_{}", name, sub_name))
        .expect("failed to create source realm");
    let target_realm = sandbox
        .create_netstack_realm::<N, _>(format!("target_{}_{}", name, sub_name))
        .expect("failed to reate target realm");
    let fake_ep = network.create_fake_endpoint().expect("failed to create fake endpoint");

    let mtu = u16::try_from(mtu).expect("failed to convert mtu to u16");
    let source_if = source_realm
        .join_network_with(
            &network,
            "source_ep",
            netemul::new_endpoint_config(mtu, Some(SOURCE_MAC_ADDRESS)),
            Default::default(),
        )
        .await
        .expect("failed to join network with source");
    source_if.add_address_and_subnet_route(SOURCE_SUBNET).await.expect("configure address");
    let target_if = target_realm
        .join_network_with(
            &network,
            "target_ep",
            netemul::new_endpoint_config(mtu, Some(TARGET_MAC_ADDRESS)),
            Default::default(),
        )
        .await
        .expect("failed to join network with target");
    target_if.add_address_and_subnet_route(TARGET_SUBNET).await.expect("configure address");

    // Add static ARP entries as we've observed flakes in CQ due to ARP timeouts
    // and ARP resolution is immaterial to this test.
    futures::stream::iter([
        (&source_realm, &source_if, TARGET_SUBNET.addr, TARGET_MAC_ADDRESS),
        (&target_realm, &target_if, SOURCE_SUBNET.addr, SOURCE_MAC_ADDRESS),
    ])
    .for_each_concurrent(None, |(realm, ep, addr, mac)| {
        realm.add_neighbor_entry(ep.id(), addr, mac).map(|r| r.expect("add_neighbor_entry failed"))
    })
    .await;

    expect_successful_ping::<ping::Ipv4>(
        &source_realm,
        // TODO(https://github.com/rust-lang/rust/issues/67390): Make the
        // address const.
        std_socket_addr_v4!("192.168.254.1:0"),
        payload_length,
    )
    .await;

    let icmp_event_stream = icmp_event_stream(&fake_ep);
    futures::pin_mut!(icmp_event_stream);

    assert_eq!(
        icmp_event_stream.next().await,
        Some(IcmpEvent::EchoRequest(IcmpPacketMetadata {
            source_address: SOURCE_SUBNET.addr,
            target_address: TARGET_SUBNET.addr,
            payload_length: expected_payload_length,
        })),
    );

    assert_eq!(
        icmp_event_stream.next().await,
        Some(IcmpEvent::EchoReply(IcmpPacketMetadata {
            source_address: TARGET_SUBNET.addr,
            target_address: SOURCE_SUBNET.addr,
            payload_length: expected_payload_length,
        })),
    );
}

#[netstack_test]
async fn starts_device_in_multicast_promiscuous<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create source realm");

    let (tun, netdevice) = netstack_testing_common::devices::create_tun_device();
    let (tun_port, dev_port) = netstack_testing_common::devices::create_eth_tun_port(
        &tun,
        /* port_id */ 1,
        TARGET_MAC_ADDRESS,
    )
    .await;

    let mac_state_stream = futures::stream::unfold(
        (tun_port, Option::<fnet_tun::MacState>::None),
        |(tun_port, last_observed)| async move {
            loop {
                let fnet_tun::InternalState { mac, .. } =
                    tun_port.watch_state().await.expect("watch_state");
                let mac = mac.expect("missing mac state");
                if last_observed.as_ref().map(|l| l != &mac).unwrap_or(true) {
                    let last_observed = Some(mac.clone());
                    break Some((mac, (tun_port, last_observed)));
                }
            }
        },
    );
    futures::pin_mut!(mac_state_stream);

    assert_matches::assert_matches!(
        mac_state_stream.next().await,
        Some(fnet_tun::MacState {
            mode: Some(fhardware_network::MacFilterMode::MulticastFilter),
            multicast_filters: Some(mcast_filters),
            ..
        }) if mcast_filters == vec![]
    );

    let device_control = netstack_testing_common::devices::install_device(&realm, netdevice);
    let mut port_id = dev_port.get_info().await.expect("get info").id.expect("missing port id");
    let (control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
            .expect("create proxy");
    device_control
        .create_interface(&mut port_id, server_end, &fnet_interfaces_admin::Options::default())
        .expect("create interface");

    // Read the interface ID to make sure device install succeeded.
    let _id: u64 = control.get_id().await.expect("get id");

    assert_matches::assert_matches!(
        mac_state_stream.next().await,
        Some(fnet_tun::MacState {
            mode: Some(fhardware_network::MacFilterMode::MulticastPromiscuous),
            multicast_filters: Some(mcast_filters),
            ..
        }) if mcast_filters == vec![]
    );
}

const ETH_HDR: usize = ETHERNET_HDR_LEN_NO_TAG;
const ETH_BODY: usize = ETHERNET_MIN_BODY_LEN_NO_TAG;
const MIN_ETH_FRAME: usize = ETHERNET_HDR_LEN_NO_TAG + ETH_BODY;
const LARGE_BODY: usize = ETH_BODY + 1024;
const LARGE_FRAME: usize = ETHERNET_HDR_LEN_NO_TAG + LARGE_BODY;
#[netstack_test]
#[test_case(fposix_socket_packet::Kind::Network, 0, 0; "network no padding")]
#[test_case(fposix_socket_packet::Kind::Link, 0, 0; "link no padding")]
#[test_case(fposix_socket_packet::Kind::Network, ETH_HDR, 0; "network header only")]
#[test_case(fposix_socket_packet::Kind::Link, ETH_HDR, 0; "link header only")]
#[test_case(fposix_socket_packet::Kind::Network, MIN_ETH_FRAME, ETH_BODY ; "network min eth")]
#[test_case(fposix_socket_packet::Kind::Link, MIN_ETH_FRAME, ETH_BODY; "link min eth")]
#[test_case(fposix_socket_packet::Kind::Network, LARGE_FRAME, LARGE_BODY ; "network large body")]
#[test_case(fposix_socket_packet::Kind::Link, LARGE_FRAME, LARGE_BODY; "link large body")]
async fn device_minimum_tx_frame_size<N: Netstack>(
    name: &str,
    socket_kind: fposix_socket_packet::Kind,
    min_tx_len: usize,
    expected_body_len: usize,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create source realm");

    let (tun, netdevice) =
        netstack_testing_common::devices::create_tun_device_with(fnet_tun::DeviceConfig {
            base: Some(fnet_tun::BaseDeviceConfig {
                min_tx_buffer_length: Some(min_tx_len.try_into().unwrap()),
                ..Default::default()
            }),
            blocking: Some(true),
            ..Default::default()
        });
    let (tun_port, dev_port) =
        netstack_testing_common::devices::create_eth_tun_port(&tun, 1, SOURCE_MAC_ADDRESS).await;

    let device_control = netstack_testing_common::devices::install_device(&realm, netdevice);
    let mut port_id = dev_port.get_info().await.expect("get info").id.expect("missing port id");
    let (control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
            .expect("create proxy");
    device_control
        .create_interface(&mut port_id, server_end, &fnet_interfaces_admin::Options::default())
        .expect("create interface");
    assert_eq!(control.enable().await.expect("can enabled"), Ok(true));
    tun_port.set_online(true).await.expect("can set online");

    let id = NonZeroU64::new(control.get_id().await.expect("get ID")).unwrap();

    /// Arbitrary Ethernet frame type that doesn't correspond to any frames
    /// normally sent by the netstack.
    const ETHERTYPE: u16 = 1600;

    fn expected_frame(body_len: usize) -> Buf<Vec<u8>> {
        Buf::new(vec![0; body_len], ..)
            .encapsulate(EthernetFrameBuilder::new(
                SOURCE_MAC_ADDRESS.into_ext(),
                TARGET_MAC_ADDRESS.into_ext(),
                ETHERTYPE.into(),
                0,
            ))
            .serialize_vec_outer()
            .unwrap()
            .into_inner()
    }

    // Send an ethernet frame with an empty body from a packet socket to see how
    // much padding is applied.
    let socket = realm.packet_socket(socket_kind).await.expect("can create socket");

    let message = match socket_kind {
        fposix_socket_packet::Kind::Link => expected_frame(0),
        fposix_socket_packet::Kind::Network => Buf::new(vec![], ..),
    };

    let sent = socket
        .send_to(
            message.as_ref(),
            &libc::sockaddr_ll::from(EthernetSockaddr {
                interface_id: Some(id),
                addr: TARGET_MAC_ADDRESS.into_ext(),
                protocol: ETHERTYPE.into(),
            })
            .into_sockaddr(),
        )
        .expect("can send");
    assert_eq!(sent, message.as_ref().len());

    let full_frame: Vec<u8> = async {
        loop {
            let frame = tun.read_frame().await.expect("can read").expect("has frame").data.unwrap();
            let mut body = &frame[..];

            match EthernetFrame::parse(&mut body, EthernetFrameLengthCheck::NoCheck) {
                Ok(ethernet) => {
                    if ethernet.ethertype().map_or(false, |e| e == EtherType::from(ETHERTYPE)) {
                        break frame;
                    }
                }
                Err(e) => {
                    // We're looking for an Ethernet frame, so ignore anything
                    // that doesn't parse as one.
                    let _: ParseError = e;
                }
            }
        }
    }
    .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || panic!("failed to find sent frame"))
    .await;

    assert_eq!(full_frame, expected_frame(expected_body_len).as_ref());
}
