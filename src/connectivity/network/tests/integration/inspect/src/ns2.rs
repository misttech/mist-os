// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]
// Needed for invocations of the `assert_data_tree` macro.
#![recursion_limit = "256"]

mod common;

use std::collections::HashMap;
use std::convert::TryFrom as _;
use std::num::NonZeroU16;

use diagnostics_assertions::{
    assert_data_tree, tree_assertion, AnyProperty, NonZeroIntProperty, PropertyAssertion,
    TreeAssertion,
};
use {
    fidl_fuchsia_io as fio, fidl_fuchsia_posix_socket as fposix_socket,
    fidl_fuchsia_posix_socket_packet as fposix_socket_packet,
    fidl_fuchsia_posix_socket_raw as fposix_socket_raw, fuchsia_async as fasync,
};

use diagnostics_hierarchy::{DiagnosticsHierarchy, Property};
use itertools::Itertools as _;
use net_declare::{fidl_ip, fidl_mac, fidl_subnet};
use net_types::ip::Ip as _;
use netemul::InStack;
use netstack_testing_common::realms::{KnownServiceProvider, Netstack, TestSandboxExt as _};
use netstack_testing_common::{constants, Result};
use netstack_testing_macros::netstack_test;
use packet::{ParsablePacket as _, Serializer as _};
use packet_formats::ethernet::testutil::ETHERNET_HDR_LEN_NO_TAG;
use packet_formats::ethernet::{EtherType, EthernetFrameBuilder, ETHERNET_MIN_BODY_LEN_NO_TAG};
use packet_formats::ipv4::{Ipv4Header as _, Ipv4PacketBuilder};
use packet_formats::udp::{UdpPacketBuilder, UdpParseArgs};
use test_case::test_case;

/// A helper type to provide address verification in inspect NIC data.
///
/// Address matcher implements `PropertyAssertion` in a stateful manner. It
/// expects all addresses in its internal set to be consumed as part of property
/// matching.
#[derive(Clone)]
struct AddressMatcher {
    set: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
}

impl AddressMatcher {
    /// Creates an `AddressMatcher` from interface properties.
    fn new(
        props: &fidl_fuchsia_net_interfaces_ext::Properties<
            fidl_fuchsia_net_interfaces_ext::DefaultInterest,
        >,
    ) -> Self {
        let set = props
            .addresses
            .iter()
            .map(
                |&fidl_fuchsia_net_interfaces_ext::Address {
                     addr: subnet,
                     assignment_state,
                     ..
                 }| {
                    assert_eq!(
                        assignment_state,
                        fidl_fuchsia_net_interfaces::AddressAssignmentState::Assigned
                    );
                    let fidl_fuchsia_net::Subnet { addr, prefix_len: _ } = subnet;
                    let prefix = match addr {
                        fidl_fuchsia_net::IpAddress::Ipv4(_) => "ipv4",
                        fidl_fuchsia_net::IpAddress::Ipv6(_) => "ipv6",
                    };
                    format!("[{}] {}", prefix, fidl_fuchsia_net_ext::Subnet::from(subnet))
                },
            )
            .collect::<std::collections::HashSet<_>>();

        Self { set: std::rc::Rc::new(std::cell::RefCell::new(set)) }
    }

    /// Checks that the internal set has been entirely consumed.
    ///
    /// Empties the internal set on return. Subsequent calls to check will
    /// always succeed.
    fn check(&self) -> Result<()> {
        let set = self.set.replace(Default::default());
        if set.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("unseen addresses left in set: {:?}", set))
        }
    }
}

impl std::ops::Drop for AddressMatcher {
    fn drop(&mut self) {
        // Always check for left over addresses on drop. Prevents the caller
        // from forgetting to do so.
        let () = self.check().expect("AddressMatcher was not emptied");
    }
}

impl PropertyAssertion for AddressMatcher {
    fn run(&self, actual: &Property<String>) -> Result<()> {
        let actual = actual.string().ok_or_else(|| {
            anyhow::anyhow!("invalid property {:#?} for AddressMatcher, want String", actual)
        })?;
        if self.set.borrow_mut().remove(actual) {
            Ok(())
        } else {
            Err(anyhow::anyhow!("{} not in expected address set", actual))
        }
    }
}

#[netstack_test]
async fn inspect_nic(name: &str) {
    type N = netstack_testing_common::realms::Netstack2;
    // The number of IPv6 addresses that the stack will assign to an interface.
    const EXPECTED_NUM_IPV6_ADDRESSES: usize = 1;

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = sandbox.create_network("net").await.expect("failed to create network");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");

    const NETDEV_MAC: fidl_fuchsia_net::MacAddress = fidl_mac!("02:0A:0B:0C:0D:0E");

    let max_frame_size = netemul::DEFAULT_MTU
        + u16::try_from(ETHERNET_HDR_LEN_NO_TAG)
            .expect("should fit ethernet header length in a u16");

    let netdev = realm
        .join_network_with(
            &network,
            "netdev-ep",
            netemul::new_endpoint_config(max_frame_size, Some(NETDEV_MAC)),
            Default::default(),
        )
        .await
        .expect("failed to join network with netdevice endpoint");
    netdev
        .add_address_and_subnet_route(fidl_subnet!("192.168.0.2/24"))
        .await
        .expect("configure address");

    let interfaces_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to fuchsia.net.interfaces/State");

    // Wait for the world to stabilize and capture the state to verify inspect
    // data.
    let (loopback_props, netdev_props) = fidl_fuchsia_net_interfaces_ext::wait_interface(
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(
            &interfaces_state,
            fidl_fuchsia_net_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .expect("failed to create event stream"),
        &mut HashMap::<u64, fidl_fuchsia_net_interfaces_ext::PropertiesAndState<(), _>>::new(),
        |if_map| {
            let loopback = if_map.values().find_map(
                |fidl_fuchsia_net_interfaces_ext::PropertiesAndState { properties, state: _ }| {
                    properties.port_class.is_loopback().then(|| properties.clone())
                },
            )?;
            // Endpoint is up, has assigned IPv4 and at least the expected number of
            // IPv6 addresses.
            let netdev_properties = {
                let fidl_fuchsia_net_interfaces_ext::PropertiesAndState { properties, state: _ } =
                    if_map.get(&netdev.id())?;
                let fidl_fuchsia_net_interfaces_ext::Properties { online, addresses, .. } =
                    properties;
                if !online {
                    return None;
                }
                let (v4_count, v6_count) = addresses.iter().fold(
                    (0, 0),
                    |(v4_count, v6_count),
                     fidl_fuchsia_net_interfaces_ext::Address {
                         addr: fidl_fuchsia_net::Subnet { addr, prefix_len: _ },
                         assignment_state,
                         ..
                     }| {
                        assert_eq!(
                            *assignment_state,
                            fidl_fuchsia_net_interfaces::AddressAssignmentState::Assigned
                        );
                        match addr {
                            fidl_fuchsia_net::IpAddress::Ipv4(_) => (v4_count + 1, v6_count),
                            fidl_fuchsia_net::IpAddress::Ipv6(_) => (v4_count, v6_count + 1),
                        }
                    },
                );
                if v4_count > 0 && v6_count >= EXPECTED_NUM_IPV6_ADDRESSES {
                    properties.clone()
                } else {
                    return None;
                }
            };
            Some((loopback, netdev_properties))
        },
    )
    .await
    .expect("failed to wait for interfaces up and addresses configured");
    let loopback_addrs = AddressMatcher::new(&loopback_props);
    let netdev_addrs = AddressMatcher::new(&netdev_props);

    // Populate the neighbor table so we can verify inspection of its entries.
    const BOB_IP: fidl_fuchsia_net::IpAddress = fidl_ip!("192.168.0.1");
    const BOB_MAC: fidl_fuchsia_net::MacAddress = fidl_mac!("02:0A:0B:0C:0D:0E");
    let () = realm
        .connect_to_protocol::<fidl_fuchsia_net_neighbor::ControllerMarker>()
        .expect("failed to connect to Controller")
        .add_entry(netdev.id(), &BOB_IP, &BOB_MAC)
        .await
        .expect("add_entry FIDL error")
        .map_err(zx::Status::from_raw)
        .expect("add_entry failed");

    let diagnostics_dir =
        realm.open_diagnostics_directory("netstack").expect("failed to open diagnostics dir");
    let data = get_inspect_data(&diagnostics_dir, "NICs", "interfaces").await;
    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);
    assert_data_tree!(data, NICs: {
        loopback_props.id.to_string() => {
            Name: loopback_props.name,
            Loopback: "true",
            LinkOnline: "true",
            AdminUp: "true",
            Promiscuous: "false",
            Up: "true",
            MTU: 65522u64,
            NICID: loopback_props.id.to_string(),
            Running: "true",
            "DHCP enabled": "false",
            LinkAddress: "00:00:00:00:00:00",
            ProtocolAddress0: loopback_addrs.clone(),
            ProtocolAddress1: loopback_addrs.clone(),
            Stats: {
                DisabledRx: {
                    Bytes: 0u64,
                    Packets: 0u64,
                },
                Tx: {
                   Bytes: 0u64,
                   Packets: 0u64,
                },
                TxPacketsDroppedNoBufferSpace: 0u64,
                Rx: {
                    Bytes: 0u64,
                    Packets: 0u64,
                },
                Neighbor: {
                    DroppedConfirmationForNoninitiatedNeighbor: 0u64,
                    DroppedInvalidLinkAddressConfirmations: 0u64,
                    UnreachableEntryLookups: 0u64,
                },
                MalformedL4RcvdPackets: 0u64,
                UnknownL3ProtocolRcvdPacketCounts: {
                    Total: {
                        Count: "0"
                    },
                },
                UnknownL4ProtocolRcvdPacketCounts: {
                    Total: {
                        Count: "0"
                    },
                },
            },
            "Network Endpoint Stats": {
                ARP: contains {},
                IPv4: contains {},
                IPv6: contains {},
            }
        },
        netdev.id().to_string() => {
            Name: netdev_props.name,
            Loopback: "false",
            LinkOnline: "true",
            AdminUp: "true",
            Promiscuous: "false",
            Up: "true",
            MTU: u64::from(netemul::DEFAULT_MTU),
            NICID: netdev.id().to_string(),
            Running: "true",
            "DHCP enabled": "false",
            LinkAddress: fidl_fuchsia_net_ext::MacAddress::from(NETDEV_MAC).to_string(),
            // IPv4.
            ProtocolAddress0: netdev_addrs.clone(),
            // Link-local IPv6.
            ProtocolAddress1: netdev_addrs.clone(),
            Stats: {
                DisabledRx: {
                    Bytes: AnyProperty,
                    Packets: AnyProperty,
                },
                Tx: {
                   Bytes: AnyProperty,
                   Packets: AnyProperty,
                },
                TxPacketsDroppedNoBufferSpace: AnyProperty,
                Rx: {
                    Bytes: AnyProperty,
                    Packets: AnyProperty,
                },
                Neighbor: {
                    DroppedConfirmationForNoninitiatedNeighbor: AnyProperty,
                    DroppedInvalidLinkAddressConfirmations: AnyProperty,
                    UnreachableEntryLookups: AnyProperty,
                },
                MalformedL4RcvdPackets: 0u64,
                UnknownL3ProtocolRcvdPacketCounts: {
                    Total: {
                        Count: "0"
                    },
                },
                UnknownL4ProtocolRcvdPacketCounts: {
                    Total: {
                        Count: "0"
                    },
                },
            },
            "Network Device Info": {
                TxDrops: AnyProperty,
                Class: "Virtual",
                RxReads: contains {},
                RxWrites: contains {},
                TxReads: contains {},
                TxWrites: contains {}
            },
            Neighbors: {
                fidl_fuchsia_net_ext::IpAddress::from(BOB_IP).to_string() => {
                    "Link address": fidl_fuchsia_net_ext::MacAddress::from(BOB_MAC).to_string(),
                    State: "Static",
                    "Last updated": NonZeroIntProperty,
                }
            },
            "Network Endpoint Stats": {
                ARP: contains {},
                IPv4: contains {},
                IPv6: contains {},
            }
        }
    });

    let () = loopback_addrs.check().expect("loopback addresses match failed");
    let () = netdev_addrs.check().expect("netdev addresses match failed");
}

#[netstack_test]
async fn inspect_routing_table(name: &str) {
    type N = netstack_testing_common::realms::Netstack2;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");
    let diagnostics_dir =
        realm.open_diagnostics_directory("netstack").expect("diagnostics dir exists");

    // NB: The Netstack component won't be started until we attempt to access
    // one of its FIDL services. Connect to fuchsia.net.interfaces/State to
    // start it.
    let _interfaces_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to fuchsia.net.interfaces/State");

    // NB: Hardcode the expected routes rather than look them up via FIDL
    // (fuchsia.net.routes). They are "keyed" in the inspect data by their
    // insertion order, but the `fuchsia.net.routes` API do not guarantee
    // listing routes in any particular order.
    let mut routing_table_assertion = TreeAssertion::new("Routes", true);
    routing_table_assertion.add_child_assertion(tree_assertion!("0": {
        "Destination": "127.0.0.0/8",
        "Gateway": "",
        "NIC": "1",
        "Metric": "100",
        "MetricTracksInterface": "true",
        "Dynamic": AnyProperty,
        "Enabled": AnyProperty,
    }));
    routing_table_assertion.add_child_assertion(tree_assertion!("1": {
        "Destination": "::1/128",
        "Gateway": "",
        "NIC": "1",
        "Metric": "100",
        "MetricTracksInterface": "true",
        "Dynamic": AnyProperty,
        "Enabled": AnyProperty,
    }));

    let data = get_inspect_data(&diagnostics_dir, "Routes", "routes").await;
    let () = routing_table_assertion
        .run(&data)
        .unwrap_or_else(|e| panic!("tree assertion fails: {}, inspect data is: {:#?}", e, data));
}

struct PacketAttributes {
    ip_proto: packet_formats::ip::Ipv4Proto,
    port: NonZeroU16,
}

const INVALID_PORT: NonZeroU16 = NonZeroU16::new(1234).unwrap();

#[netstack_test]
#[test_case(
    "invalid_trans_proto",
    vec![
        PacketAttributes {
            ip_proto: packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Tcp),
            port: dhcpv4::protocol::CLIENT_PORT,
        }
    ]; "invalid_trans_proto")]
#[test_case(
    "invalid_port",
    vec![
        PacketAttributes {
            ip_proto: packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp),
            port: INVALID_PORT,
        }
    ]; "invalid_port")]
#[test_case(
    "valid",
    vec![
        PacketAttributes {
            ip_proto: packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp),
            port: dhcpv4::protocol::CLIENT_PORT,
        }
    ]; "valid")]
#[test_case(
    "multiple_invalid_port_and_single_invalid_trans_proto",
    vec![
        PacketAttributes {
            ip_proto: packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp),
            port: INVALID_PORT,
        },
        PacketAttributes {
            ip_proto: packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp),
            port: INVALID_PORT,
        },
            PacketAttributes {
            ip_proto: packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Tcp),
            port: dhcpv4::protocol::CLIENT_PORT,
        }
    ]; "multiple_invalid_port_and_single_invalid_trans_proto")]
async fn inspect_dhcp(
    netstack_test_name: &str,
    test_case_name: &str,
    inbound_packets: Vec<PacketAttributes>,
) {
    type N = netstack_testing_common::realms::Netstack2;
    // TODO(https://fxbug.dev/42159805): Extend this test to cover the stat tracking frames discarded
    // due to an invalid PacketType.
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = sandbox.create_network("net").await.expect("failed to create network");
    let name = format!("{netstack_test_name}-{test_case_name}");
    let realm = sandbox.create_netstack_realm::<N, _>(&name).expect("failed to create realm");
    let diagnostics_dir =
        realm.open_diagnostics_directory("netstack").expect("diagnostics dir exists");
    // Create the fake endpoint before installing an endpoint in the netstack to ensure
    // that we receive all DHCP messages sent by the client.
    let fake_ep = network.create_fake_endpoint().expect("failed to create fake endpoint");
    let eth = realm.join_network(&network, "ep1").await.expect("failed to join network");
    eth.start_dhcp::<InStack>().await.expect("failed to start DHCP");

    // Wait for a DHCP message here to ensure that the client is ready to receive
    // incoming packets.
    loop {
        let (buf, _dropped_frames): (Vec<u8>, u64) =
            fake_ep.read().await.expect("failed to read from endpoint");
        let mut buf = &buf[..];
        let frame = packet_formats::ethernet::EthernetFrame::parse(
            &mut buf,
            packet_formats::ethernet::EthernetFrameLengthCheck::NoCheck,
        )
        .expect("failed to parse ethernet frame");

        match frame.ethertype().expect("failed to parse frame ethertype") {
            packet_formats::ethernet::EtherType::Ipv4 => {
                let mut frame_body = frame.body();
                let packet = packet_formats::ipv4::Ipv4Packet::parse(&mut frame_body, ())
                    .expect("failed to parse IPv4 packet");

                match packet.proto() {
                    packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp) => {
                        let mut packet_body = packet.body();
                        let datagram = packet_formats::udp::UdpPacket::parse(
                            &mut packet_body,
                            UdpParseArgs::new(packet.src_ip(), packet.dst_ip()),
                        )
                        .expect("failed to parse UDP datagram");
                        const DHCPV4_SERVER_PORT: u16 = dhcpv4::protocol::SERVER_PORT.get();
                        match datagram.dst_port().get() {
                            DHCPV4_SERVER_PORT => {
                                // Any DHCP message means the client is listening; we don't care
                                // about the contents.
                                let _: dhcpv4::protocol::Message =
                                    dhcpv4::protocol::Message::from_buffer(datagram.body())
                                        .expect("failed to parse DHCP message");
                                break;
                            }
                            port => println!(
                                "received non-DHCP UDP datagram with destination port: {:?}",
                                port
                            ),
                        }
                    }
                    proto => println!(
                        "received non-UDP IPv4 packet with transport protocol: {:?}",
                        proto
                    ),
                }
            }
            ethertype => println!("received non-IPv4 frame with ethertype: {:?}", ethertype),
        }
    }

    const SRC_IP: net_types::ip::Ipv4Addr = net_types::ip::Ipv4::UNSPECIFIED_ADDRESS;
    const DST_IP: net_types::SpecifiedAddr<net_types::ip::Ipv4Addr> =
        net_types::ip::Ipv4::LIMITED_BROADCAST_ADDRESS;

    for PacketAttributes { ip_proto, port } in &inbound_packets {
        let ser = packet::Buf::new(&mut [], ..)
            .encapsulate(UdpPacketBuilder::new(SRC_IP, *DST_IP, None, *port))
            .encapsulate(Ipv4PacketBuilder::new(SRC_IP, DST_IP, /* ttl */ 1, *ip_proto))
            .encapsulate(EthernetFrameBuilder::new(
                constants::eth::MAC_ADDR,            /* src_mac */
                net_types::ethernet::Mac::BROADCAST, /* dst_mac */
                EtherType::Ipv4,
                ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer()
            .expect("failed to serialize UDP packet")
            .unwrap_b();
        let () = fake_ep.write(ser.as_ref()).await.expect("failed to write to endpoint");
    }

    const DISCARD_STATS_NAME: &str = "PacketDiscardStats";
    const INVALID_PORT_STAT_NAME: &str = "InvalidPort";
    const INVALID_TRANS_PROTO_STAT_NAME: &str = "InvalidTransProto";
    const INVALID_PACKET_TYPE_STAT_NAME: &str = "InvalidPacketType";
    const COUNTER_PROPERTY_NAME: &str = "Count";
    const TOTAL_COUNTER_NAME: &str = "Total";
    let path = ["Stats", "DHCP Info", &eth.id().to_string(), "NICs"];

    let mut invalid_ports = HashMap::<NonZeroU16, u64>::new();
    let mut invalid_trans_protos = HashMap::<u8, u64>::new();

    for PacketAttributes { port, ip_proto } in inbound_packets {
        if ip_proto != packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp) {
            let _: &mut u64 =
                invalid_trans_protos.entry(ip_proto.into()).and_modify(|v| *v += 1).or_insert(1);
        } else if port != dhcpv4::protocol::CLIENT_PORT {
            let _: &mut u64 = invalid_ports.entry(port.into()).and_modify(|v| *v += 1).or_insert(1);
        }
    }

    let mut invalid_port_assertion = TreeAssertion::new(INVALID_PORT_STAT_NAME, true);
    let mut invalid_trans_proto_assertion = TreeAssertion::new(INVALID_TRANS_PROTO_STAT_NAME, true);
    let mut invalid_packet_type_assertion = TreeAssertion::new(INVALID_PACKET_TYPE_STAT_NAME, true);
    let mut total_packet_type_assertion = TreeAssertion::new(TOTAL_COUNTER_NAME, true);
    let () = total_packet_type_assertion
        .add_property_assertion(COUNTER_PROPERTY_NAME, Box::new(0.to_string()));
    let () = invalid_packet_type_assertion.add_child_assertion(total_packet_type_assertion);

    let mut total_port = 0;
    for (port, count) in invalid_ports {
        let mut port_assertion = TreeAssertion::new(&port.to_string(), true);
        let () = port_assertion
            .add_property_assertion(COUNTER_PROPERTY_NAME, Box::new(count.to_string()));
        total_port += count;
        let () = invalid_port_assertion.add_child_assertion(port_assertion);
    }
    let mut total_port_assertion = TreeAssertion::new(TOTAL_COUNTER_NAME, true);
    let () = total_port_assertion
        .add_property_assertion(COUNTER_PROPERTY_NAME, Box::new(total_port.to_string()));
    let () = invalid_port_assertion.add_child_assertion(total_port_assertion);

    let mut total_trans_proto = 0;
    for (proto, count) in invalid_trans_protos {
        let mut trans_proto_assertion = TreeAssertion::new(&proto.to_string(), true);
        let () = trans_proto_assertion
            .add_property_assertion(COUNTER_PROPERTY_NAME, Box::new(count.to_string()));
        total_trans_proto += count;
        let () = invalid_trans_proto_assertion.add_child_assertion(trans_proto_assertion);
    }
    let mut total_trans_proto_assertion = TreeAssertion::new(TOTAL_COUNTER_NAME, true);
    let () = total_trans_proto_assertion
        .add_property_assertion(COUNTER_PROPERTY_NAME, Box::new(total_trans_proto.to_string()));
    let () = invalid_trans_proto_assertion.add_child_assertion(total_trans_proto_assertion);

    let mut discard_stats_assertion = TreeAssertion::new(DISCARD_STATS_NAME, true);
    let () = discard_stats_assertion.add_child_assertion(invalid_port_assertion);
    let () = discard_stats_assertion.add_child_assertion(invalid_trans_proto_assertion);
    let () = discard_stats_assertion.add_child_assertion(invalid_packet_type_assertion);

    let tree_assertion = path.iter().fold(discard_stats_assertion, |acc, name| {
        let mut assertion = TreeAssertion::new(name, true);
        let () = assertion.add_child_assertion(acc);
        assertion
    });

    loop {
        let data = get_inspect_data(
            &diagnostics_dir,
            std::iter::once(DISCARD_STATS_NAME)
                .chain(path)
                .into_iter()
                .map(selectors::sanitize_string_for_selectors)
                .rev()
                .join("/"),
            "interfaces",
        )
        .await;
        match tree_assertion.run(&data) {
            Ok(()) => break,
            Err(err) => {
                println!("Got mismatched inspect data with err: {:?}", err);
            }
        }
        let () = fasync::Timer::new(std::time::Duration::from_millis(100)).await;
    }
}

// This test verifies exactly which stat counters are exported through
// inspect. If any counter is added or deleted, the inline list of the
// counters below should be updated accordingly.
//
// Note that many of the counters are implemented in gVisor. They are
// automatically exported from netstack via reflection. This test
// serves as a change detector to acknowledge any possible additions
// or deletions when importing code from upstream.
#[netstack_test]
async fn inspect_stat_counters(name: &str) {
    type N = netstack_testing_common::realms::Netstack2;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");
    // Connect to netstack service to spawn a netstack instance.
    let _stack = realm
        .connect_to_protocol::<fidl_fuchsia_net_stack::StackMarker>()
        .expect("failed to connect to fuchsia.net.stack/Stack");
    let diagnostics_dir =
        realm.open_diagnostics_directory("netstack").expect("diagnostics dir exists");

    let data =
        get_inspect_data(&diagnostics_dir, r#"Networking\ Stat\ Counters"#, "counters").await;
    // TODO(https://fxbug.dev/42140843): change AnyProperty to AnyUintProperty when available.
    assert_data_tree!(data, "Networking Stat Counters": {
        DroppedPackets: AnyProperty,
        SocketCount: AnyProperty,
        SocketsCreated: AnyProperty,
        SocketsDestroyed: AnyProperty,
        ARP: {
            DisabledPacketsReceived: AnyProperty,
            MalformedPacketsReceived: AnyProperty,
            OutgoingRepliesDropped: AnyProperty,
            OutgoingRepliesSent: AnyProperty,
            OutgoingRequestBadLocalAddressErrors: AnyProperty,
            OutgoingRequestInterfaceHasNoLocalAddressErrors: AnyProperty,
            OutgoingRequestsDropped: AnyProperty,
            OutgoingRequestsSent: AnyProperty,
            PacketsReceived: AnyProperty,
            RepliesReceived: AnyProperty,
            RequestsReceived: AnyProperty,
            RequestsReceivedUnknownTargetAddress: AnyProperty,
        },
        DHCPv6: {
            ManagedAddress: AnyProperty,
            NoConfiguration: AnyProperty,
            OtherConfiguration: AnyProperty,
        },
        ICMP: {
            V4: {
                PacketsReceived: {
                    DstUnreachable: AnyProperty,
                    EchoReply: AnyProperty,
                    EchoRequest: AnyProperty,
                    InfoReply: AnyProperty,
                    InfoRequest: AnyProperty,
                    Invalid: AnyProperty,
                    ParamProblem: AnyProperty,
                    Redirect: AnyProperty,
                    SrcQuench: AnyProperty,
                    TimeExceeded: AnyProperty,
                    Timestamp: AnyProperty,
                    TimestampReply: AnyProperty,
                },
                PacketsSent: {
                    Dropped: AnyProperty,
                    DstUnreachable: AnyProperty,
                    EchoReply: AnyProperty,
                    EchoRequest: AnyProperty,
                    InfoReply: AnyProperty,
                    InfoRequest: AnyProperty,
                    ParamProblem: AnyProperty,
                    RateLimited: AnyProperty,
                    Redirect: AnyProperty,
                    SrcQuench: AnyProperty,
                    TimeExceeded: AnyProperty,
                    Timestamp: AnyProperty,
                    TimestampReply: AnyProperty,
                },
            },
            V6: {
                PacketsReceived: {
                    DstUnreachable: AnyProperty,
                    EchoReply: AnyProperty,
                    EchoRequest: AnyProperty,
                    Invalid: AnyProperty,
                    MulticastListenerDone: AnyProperty,
                    MulticastListenerQuery: AnyProperty,
                    MulticastListenerReport: AnyProperty,
                    MulticastListenerReportV2: AnyProperty,
                    NeighborAdvert: AnyProperty,
                    NeighborSolicit: AnyProperty,
                    PacketTooBig: AnyProperty,
                    ParamProblem: AnyProperty,
                    RedirectMsg: AnyProperty,
                    RouterAdvert: AnyProperty,
                    RouterOnlyPacketsDroppedByHost: AnyProperty,
                    RouterSolicit: AnyProperty,
                    TimeExceeded: AnyProperty,
                    Unrecognized: AnyProperty,
                },
                PacketsSent: {
                    Dropped: AnyProperty,
                    DstUnreachable: AnyProperty,
                    EchoReply: AnyProperty,
                    EchoRequest: AnyProperty,
                    MulticastListenerDone: AnyProperty,
                    MulticastListenerQuery: AnyProperty,
                    MulticastListenerReport: AnyProperty,
                    MulticastListenerReportV2: AnyProperty,
                    NeighborAdvert: AnyProperty,
                    NeighborSolicit: AnyProperty,
                    PacketTooBig: AnyProperty,
                    ParamProblem: AnyProperty,
                    RateLimited: AnyProperty,
                    RedirectMsg: AnyProperty,
                    RouterAdvert: AnyProperty,
                    RouterSolicit: AnyProperty,
                    TimeExceeded: AnyProperty,
                },
            },
        },
        IGMP: {
            PacketsReceived: {
                ChecksumErrors: AnyProperty,
                Invalid: AnyProperty,
                LeaveGroup: AnyProperty,
                MembershipQuery: AnyProperty,
                Unrecognized: AnyProperty,
                V1MembershipReport: AnyProperty,
                V2MembershipReport: AnyProperty,
                V3MembershipReport: AnyProperty,
            },
            PacketsSent: {
                Dropped: AnyProperty,
                LeaveGroup: AnyProperty,
                MembershipQuery: AnyProperty,
                V1MembershipReport: AnyProperty,
                V2MembershipReport: AnyProperty,
                V3MembershipReport: AnyProperty,
            },
        },
        IP: {
            DisabledPacketsReceived: AnyProperty,
            IPTablesForwardDropped: AnyProperty,
            IPTablesInputDropped: AnyProperty,
            IPTablesOutputDropped: AnyProperty,
            IPTablesPostroutingDropped: AnyProperty,
            IPTablesPreroutingDropped: AnyProperty,
            InvalidDestinationAddressesReceived: AnyProperty,
            InvalidSourceAddressesReceived: AnyProperty,
            MalformedFragmentsReceived: AnyProperty,
            MalformedPacketsReceived: AnyProperty,
            OptionRecordRouteReceived: AnyProperty,
            OptionRouterAlertReceived: AnyProperty,
            OptionTimestampReceived: AnyProperty,
            OptionUnknownReceived: AnyProperty,
            OutgoingPacketErrors: AnyProperty,
            PacketsDelivered: AnyProperty,
            PacketsReceived: AnyProperty,
            PacketsSent: AnyProperty,
            ValidPacketsReceived: AnyProperty,
            Forwarding: {
                Errors: AnyProperty,
                ExhaustedTTL: AnyProperty,
                ExtensionHeaderProblem: AnyProperty,
                HostUnreachable: AnyProperty,
                InitializingSource: AnyProperty,
                LinkLocalDestination: AnyProperty,
                LinkLocalSource: AnyProperty,
                NoMulticastPendingQueueBufferSpace: AnyProperty,
                OutgoingDeviceClosedForSend: AnyProperty,
                OutgoingDeviceNoBufferSpace: AnyProperty,
                PacketTooBig: AnyProperty,
                UnexpectedMulticastInputInterface: AnyProperty,
                UnknownOutputEndpoint: AnyProperty,
                Unrouteable: AnyProperty,
            },
        },
        IPv6AddressConfig: {
            DHCPv6ManagedAddressOnly: AnyProperty,
            GlobalSLAACAndDHCPv6ManagedAddress: AnyProperty,
            GlobalSLAACOnly: AnyProperty,
            NoGlobalSLAACOrDHCPv6ManagedAddress: AnyProperty,
        },
        NICs: {
            MalformedL4RcvdPackets: AnyProperty,
            DisabledRx: {
                Bytes: AnyProperty,
                Packets: AnyProperty,
            },
            Neighbor: {
                DroppedConfirmationForNoninitiatedNeighbor: AnyProperty,
                DroppedInvalidLinkAddressConfirmations: AnyProperty,
                UnreachableEntryLookups: AnyProperty,
            },
            Rx: {
                Bytes: AnyProperty,
                Packets: AnyProperty,
            },
            Tx: {
                Bytes: AnyProperty,
                Packets: AnyProperty,
            },
            TxPacketsDroppedNoBufferSpace: AnyProperty,
            UnknownL3ProtocolRcvdPacketCounts: {
                Total: {
                    Count: "0"
                },
            },
            UnknownL4ProtocolRcvdPacketCounts: {
                Total: {
                    Count: "0"
                },
            },
        },
        TCP: {
            ActiveConnectionOpenings: AnyProperty,
            ChecksumErrors: AnyProperty,
            CurrentConnected: AnyProperty,
            CurrentEstablished: AnyProperty,
            EstablishedClosed: AnyProperty,
            EstablishedResets: AnyProperty,
            EstablishedTimedout: AnyProperty,
            FailedConnectionAttempts: AnyProperty,
            FailedPortReservations: AnyProperty,
            FastRecovery: AnyProperty,
            FastRetransmit: AnyProperty,
            ForwardMaxInFlightDrop: AnyProperty,
            InvalidSegmentsReceived: AnyProperty,
            ListenOverflowAckDrop: AnyProperty,
            ListenOverflowInvalidSynCookieRcvd: AnyProperty,
            ListenOverflowSynCookieRcvd: AnyProperty,
            ListenOverflowSynCookieSent: AnyProperty,
            ListenOverflowSynDrop: AnyProperty,
            PassiveConnectionOpenings: AnyProperty,
            ResetsReceived: AnyProperty,
            ResetsSent: AnyProperty,
            Retransmits: AnyProperty,
            SACKRecovery: AnyProperty,
            SegmentSendErrors: AnyProperty,
            SegmentsAckedWithDSACK: AnyProperty,
            SegmentsSent: AnyProperty,
            SlowStartRetransmits: AnyProperty,
            SpuriousRTORecovery: AnyProperty,
            SpuriousRecovery: AnyProperty,
            TLPRecovery: AnyProperty,
            Timeouts: AnyProperty,
            ValidSegmentsReceived: AnyProperty,
        },
        UDP: {
            ChecksumErrors: AnyProperty,
            MalformedPacketsReceived: AnyProperty,
            PacketSendErrors: AnyProperty,
            PacketsReceived: AnyProperty,
            PacketsSent: AnyProperty,
            ReceiveBufferErrors: AnyProperty,
            UnknownPortErrors: AnyProperty,
        },
        MaxSocketOptionStats: {
            // Socket options common to all sockets.
            SetReuseAddress: 0u64,
            GetReuseAddress: 0u64,
            GetError: 0u64,
            SetBroadcast: 0u64,
            GetBroadcast: 0u64,
            SetSendBuffer: 0u64,
            GetSendBuffer: 0u64,
            SetReceiveBuffer: 0u64,
            GetReceiveBuffer: 0u64,
            SetKeepAlive: 0u64,
            GetKeepAlive: 0u64,
            SetOutOfBandInline: 0u64,
            GetOutOfBandInline: 0u64,
            SetNoCheck: 0u64,
            GetNoCheck: 0u64,
            SetLinger: 0u64,
            GetLinger: 0u64,
            SetReusePort: 0u64,
            GetReusePort: 0u64,
            GetAcceptConn: 0u64,
            SetBindToDevice: 0u64,
            GetBindToDevice: 0u64,
            SetTimestamp: 0u64,
            GetTimestamp: 0u64,

            // Socket options defined on network sockets.
            SetIpTypeOfService: 0u64,
            GetIpTypeOfService: 0u64,
            SetIpTtl: 0u64,
            GetIpTtl: 0u64,
            SetIpPacketInfo: 0u64,
            GetIpPacketInfo: 0u64,
            SetIpReceiveTypeOfService: 0u64,
            GetIpReceiveTypeOfService: 0u64,
            SetIpReceiveTtl: 0u64,
            GetIpReceiveTtl: 0u64,
            SetIpMulticastInterface: 0u64,
            GetIpMulticastInterface: 0u64,
            SetIpMulticastTtl: 0u64,
            GetIpMulticastTtl: 0u64,
            SetIpMulticastLoopback: 0u64,
            GetIpMulticastLoopback: 0u64,
            SetIpv6MulticastInterface: 0u64,
            GetIpv6MulticastInterface: 0u64,
            SetIpv6UnicastHops: 0u64,
            GetIpv6UnicastHops: 0u64,
            SetIpv6ReceiveHopLimit: 0u64,
            GetIpv6ReceiveHopLimit: 0u64,
            SetIpv6MulticastHops: 0u64,
            GetIpv6MulticastHops: 0u64,
            SetIpv6MulticastLoopback: 0u64,
            GetIpv6MulticastLoopback: 0u64,
            AddIpMembership: 0u64,
            DropIpMembership: 0u64,
            AddIpv6Membership: 0u64,
            DropIpv6Membership: 0u64,
            SetIpv6Only: 0u64,
            GetIpv6Only: 0u64,
            SetIpv6ReceiveTrafficClass: 0u64,
            GetIpv6ReceiveTrafficClass: 0u64,
            SetIpv6TrafficClass: 0u64,
            GetIpv6TrafficClass: 0u64,
            SetIpv6ReceivePacketInfo: 0u64,
            GetIpv6ReceivePacketInfo: 0u64,

            // Socket options defined on stream sockets.
            SetTcpNoDelay: 0u64,
            GetTcpNoDelay: 0u64,
            SetTcpMaxSegment: 0u64,
            GetTcpMaxSegment: 0u64,
            SetTcpCork: 0u64,
            GetTcpCork: 0u64,
            SetTcpKeepAliveIdle: 0u64,
            GetTcpKeepAliveIdle: 0u64,
            SetTcpKeepAliveInterval: 0u64,
            GetTcpKeepAliveInterval: 0u64,
            SetTcpKeepAliveCount: 0u64,
            GetTcpKeepAliveCount: 0u64,
            SetTcpSynCount: 0u64,
            GetTcpSynCount: 0u64,
            SetTcpLinger: 0u64,
            GetTcpLinger: 0u64,
            SetTcpDeferAccept: 0u64,
            GetTcpDeferAccept: 0u64,
            SetTcpWindowClamp: 0u64,
            GetTcpWindowClamp: 0u64,
            GetTcpInfo: 0u64,
            SetTcpQuickAck: 0u64,
            GetTcpQuickAck: 0u64,
            SetTcpCongestion: 0u64,
            GetTcpCongestion: 0u64,
            SetTcpUserTimeout: 0u64,
            GetTcpUserTimeout: 0u64,

            // Socket options defined on raw sockets.
            SetIpHeaderIncluded: 0u64,
            GetIpHeaderIncluded: 0u64,
            SetIcmpv6Filter: 0u64,
            GetIcmpv6Filter: 0u64,
            SetIpv6Checksum: 0u64,
            GetIpv6Checksum: 0u64,
        }
    });
}

#[netstack_test]
async fn inspect_socket_stats(name: &str) {
    type N = netstack_testing_common::realms::Netstack2;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");

    let _tcp_socket = realm
        .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create TCP socket");
    let _udp_socket = realm
        .datagram_socket(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("create UDP socket");
    let _raw_socket = realm
        .raw_socket(
            fposix_socket::Domain::Ipv4,
            fposix_socket_raw::ProtocolAssociation::Unassociated(fposix_socket_raw::Empty),
        )
        .await
        .expect("create raw socket");
    let _packet_socket = realm
        .packet_socket(fposix_socket_packet::Kind::Network)
        .await
        .expect("create packet socket");

    let diagnostics_dir =
        realm.open_diagnostics_directory("netstack").expect("diagnostics dir exists");
    let mut data = get_inspect_data(&diagnostics_dir, r#"Socket\ Info"#, "sockets").await;
    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);
    // TODO(https://fxbug.dev/324494938): We are getting the property ChecksumErrors duplicated in
    // one case. Remove it before asserting as assert_data_tree expects it to be present only once.
    let receive_errors = data.get_child_by_path_mut(&["1", "Stats", "ReceiveErrors"]).unwrap();
    receive_errors.properties.sort_by(|a, b| a.key().cmp(b.key()));
    receive_errors.properties.dedup_by_key(|p| p.key().clone());
    assert_data_tree!(data, "Socket Info": contains {
        "1": {
            BindAddress: "",
            BindNICID: "0",
            LocalAddress: "0.0.0.0:0",
            NetworkProtocol: "IPv4",
            RegisterNICID: "0",
            RemoteAddress: "0.0.0.0:0",
            State: "INITIAL",
            TransportProtocol: "TCP",
            Stats: {
              FailedConnectionAttempts: 0u64,
              SegmentsReceived: 0u64,
              SegmentsSent: 0u64,
              ReadErrors: {
                InvalidEndpointState: 0u64,
                NotConnected: 0u64,
                ReadClosed: 0u64,
              },
              ReceiveErrors: {
                ChecksumErrors: 0u64,
                ClosedReceiver: 0u64,
                ListenOverflowAckDrop: 0u64,
                ListenOverflowSynDrop: 0u64,
                MalformedPacketsReceived: 0u64,
                ReceiveBufferOverflow: 0u64,
                SegmentQueueDropped: 0u64,
                WantZeroRcvWindow: 0u64,
                ZeroRcvWindowState: 0u64,
              },
              SendErrors: {
                FastRetransmit: 0u64,
                NoRoute: 0u64,
                Retransmits: 0u64,
                SegmentSendToNetworkFailed: 0u64,
                SendToNetworkFailed: 0u64,
                SynSendToNetworkFailed: 0u64,
                Timeouts: 0u64,
              },
              WriteErrors: {
                InvalidArgs: 0u64,
                InvalidEndpointState: 0u64,
                WriteClosed: 0u64,
              },
            },
            "Socket Option Stats": {
              AddIpMembership: 0u64,
              AddIpv6Membership: 0u64,
              DropIpMembership: 0u64,
              DropIpv6Membership: 0u64,
              GetAcceptConn: 0u64,
              GetBindToDevice: 0u64,
              GetBroadcast: 0u64,
              GetError: 0u64,
              GetIpMulticastInterface: 0u64,
              GetIpMulticastLoopback: 0u64,
              GetIpMulticastTtl: 0u64,
              GetIpPacketInfo: 0u64,
              GetIpReceiveTtl: 0u64,
              GetIpReceiveTypeOfService: 0u64,
              GetIpTtl: 0u64,
              GetIpTypeOfService: 0u64,
              GetIpv6MulticastHops: 0u64,
              GetIpv6MulticastInterface: 0u64,
              GetIpv6MulticastLoopback: 0u64,
              GetIpv6Only: 0u64,
              GetIpv6ReceiveHopLimit: 0u64,
              GetIpv6ReceivePacketInfo: 0u64,
              GetIpv6ReceiveTrafficClass: 0u64,
              GetIpv6TrafficClass: 0u64,
              GetIpv6UnicastHops: 0u64,
              GetKeepAlive: 0u64,
              GetLinger: 0u64,
              GetNoCheck: 0u64,
              GetOutOfBandInline: 0u64,
              GetReceiveBuffer: 0u64,
              GetReuseAddress: 0u64,
              GetReusePort: 0u64,
              GetSendBuffer: 0u64,
              GetTcpCongestion: 0u64,
              GetTcpCork: 0u64,
              GetTcpDeferAccept: 0u64,
              GetTcpInfo: 0u64,
              GetTcpKeepAliveCount: 0u64,
              GetTcpKeepAliveIdle: 0u64,
              GetTcpKeepAliveInterval: 0u64,
              GetTcpLinger: 0u64,
              GetTcpMaxSegment: 0u64,
              GetTcpNoDelay: 0u64,
              GetTcpQuickAck: 0u64,
              GetTcpSynCount: 0u64,
              GetTcpUserTimeout: 0u64,
              GetTcpWindowClamp: 0u64,
              GetTimestamp: 0u64,
              SetBindToDevice: 0u64,
              SetBroadcast: 0u64,
              SetIpMulticastInterface: 0u64,
              SetIpMulticastLoopback: 0u64,
              SetIpMulticastTtl: 0u64,
              SetIpPacketInfo: 0u64,
              SetIpReceiveTtl: 0u64,
              SetIpReceiveTypeOfService: 0u64,
              SetIpTtl: 0u64,
              SetIpTypeOfService: 0u64,
              SetIpv6MulticastHops: 0u64,
              SetIpv6MulticastInterface: 0u64,
              SetIpv6MulticastLoopback: 0u64,
              SetIpv6Only: 0u64,
              SetIpv6ReceiveHopLimit: 0u64,
              SetIpv6ReceivePacketInfo: 0u64,
              SetIpv6ReceiveTrafficClass: 0u64,
              SetIpv6TrafficClass: 0u64,
              SetIpv6UnicastHops: 0u64,
              SetKeepAlive: 0u64,
              SetLinger: 0u64,
              SetNoCheck: 0u64,
              SetOutOfBandInline: 0u64,
              SetReceiveBuffer: 0u64,
              SetReuseAddress: 0u64,
              SetReusePort: 0u64,
              SetSendBuffer: 0u64,
              SetTcpCongestion: 0u64,
              SetTcpCork: 0u64,
              SetTcpDeferAccept: 0u64,
              SetTcpKeepAliveCount: 0u64,
              SetTcpKeepAliveIdle: 0u64,
              SetTcpKeepAliveInterval: 0u64,
              SetTcpLinger: 0u64,
              SetTcpMaxSegment: 0u64,
              SetTcpNoDelay: 0u64,
              SetTcpQuickAck: 0u64,
              SetTcpSynCount: 0u64,
              SetTcpUserTimeout: 0u64,
              SetTcpWindowClamp: 0u64,
              SetTimestamp: 0u64,
            },
        },
        "2": {
            BindAddress: "",
            BindNICID: "0",
            LocalAddress: "0.0.0.0:0",
            NetworkProtocol: "IPv4",
            RegisterNICID: "0",
            RemoteAddress: "0.0.0.0:0",
            State: "INITIAL",
            TransportProtocol: "UDP",
            Stats: {
              PacketsReceived: 0u64,
              PacketsSent: 0u64,
              ReadErrors: {
                InvalidEndpointState: 0u64,
                NotConnected: 0u64,
                ReadClosed: 0u64,
              },
              ReceiveErrors: {
                ChecksumErrors: 0u64,
                ClosedReceiver: 0u64,
                MalformedPacketsReceived: 0u64,
                ReceiveBufferOverflow: 0u64,
              },
              SendErrors: {
                NoRoute: 0u64,
                SendToNetworkFailed: 0u64,
              },
              WriteErrors: {
                InvalidArgs: 0u64,
                InvalidEndpointState: 0u64,
                WriteClosed: 0u64,
              },
            },
            "Socket Option Stats": {
              AddIpMembership: 0u64,
              AddIpv6Membership: 0u64,
              DropIpMembership: 0u64,
              DropIpv6Membership: 0u64,
              GetAcceptConn: 0u64,
              GetBindToDevice: 0u64,
              GetBroadcast: 0u64,
              GetError: 0u64,
              GetIpMulticastInterface: 0u64,
              GetIpMulticastLoopback: 0u64,
              GetIpMulticastTtl: 0u64,
              GetIpPacketInfo: 0u64,
              GetIpReceiveTtl: 0u64,
              GetIpReceiveTypeOfService: 0u64,
              GetIpTtl: 0u64,
              GetIpTypeOfService: 0u64,
              GetIpv6MulticastHops: 0u64,
              GetIpv6MulticastInterface: 0u64,
              GetIpv6MulticastLoopback: 0u64,
              GetIpv6Only: 0u64,
              GetIpv6ReceiveHopLimit: 0u64,
              GetIpv6ReceivePacketInfo: 0u64,
              GetIpv6ReceiveTrafficClass: 0u64,
              GetIpv6TrafficClass: 0u64,
              GetIpv6UnicastHops: 0u64,
              GetKeepAlive: 0u64,
              GetLinger: 0u64,
              GetNoCheck: 0u64,
              GetOutOfBandInline: 0u64,
              GetReceiveBuffer: 0u64,
              GetReuseAddress: 0u64,
              GetReusePort: 0u64,
              GetSendBuffer: 0u64,
              GetTimestamp: 0u64,
              SetBindToDevice: 0u64,
              SetBroadcast: 0u64,
              SetIpMulticastInterface: 0u64,
              SetIpMulticastLoopback: 0u64,
              SetIpMulticastTtl: 0u64,
              SetIpPacketInfo: 0u64,
              SetIpReceiveTtl: 0u64,
              SetIpReceiveTypeOfService: 0u64,
              SetIpTtl: 0u64,
              SetIpTypeOfService: 0u64,
              SetIpv6MulticastHops: 0u64,
              SetIpv6MulticastInterface: 0u64,
              SetIpv6MulticastLoopback: 0u64,
              SetIpv6Only: 0u64,
              SetIpv6ReceiveHopLimit: 0u64,
              SetIpv6ReceivePacketInfo: 0u64,
              SetIpv6ReceiveTrafficClass: 0u64,
              SetIpv6TrafficClass: 0u64,
              SetIpv6UnicastHops: 0u64,
              SetKeepAlive: 0u64,
              SetLinger: 0u64,
              SetNoCheck: 0u64,
              SetOutOfBandInline: 0u64,
              SetReceiveBuffer: 0u64,
              SetReuseAddress: 0u64,
              SetReusePort: 0u64,
              SetSendBuffer: 0u64,
              SetTimestamp: 0u64,
            },
        },
        "3": {
            BindAddress: "",
            BindNICID: "0",
            LocalAddress: "0.0.0.0:0",
            NetworkProtocol: "IPv4",
            RegisterNICID: "0",
            RemoteAddress: "0.0.0.0:0",
            State: "",
            TransportProtocol: "UNKNOWN",
            Stats: {
              PacketsReceived: 0u64,
              PacketsSent: 0u64,
              ReadErrors: {
                InvalidEndpointState: 0u64,
                NotConnected: 0u64,
                ReadClosed: 0u64,
              },
              ReceiveErrors: {
                ChecksumErrors: 0u64,
                ClosedReceiver: 0u64,
                MalformedPacketsReceived: 0u64,
                ReceiveBufferOverflow: 0u64,
              },
              SendErrors: {
                NoRoute: 0u64,
                SendToNetworkFailed: 0u64,
              },
              WriteErrors: {
                InvalidArgs: 0u64,
                InvalidEndpointState: 0u64,
                WriteClosed: 0u64,
              },
            },
            "Socket Option Stats": {
              AddIpMembership: 0u64,
              AddIpv6Membership: 0u64,
              DropIpMembership: 0u64,
              DropIpv6Membership: 0u64,
              GetAcceptConn: 0u64,
              GetBindToDevice: 0u64,
              GetBroadcast: 0u64,
              GetError: 0u64,
              GetIcmpv6Filter: 0u64,
              GetIpHeaderIncluded: 0u64,
              GetIpMulticastInterface: 0u64,
              GetIpMulticastLoopback: 0u64,
              GetIpMulticastTtl: 0u64,
              GetIpPacketInfo: 0u64,
              GetIpReceiveTtl: 0u64,
              GetIpReceiveTypeOfService: 0u64,
              GetIpTtl: 0u64,
              GetIpTypeOfService: 0u64,
              GetIpv6Checksum: 0u64,
              GetIpv6MulticastHops: 0u64,
              GetIpv6MulticastInterface: 0u64,
              GetIpv6MulticastLoopback: 0u64,
              GetIpv6Only: 0u64,
              GetIpv6ReceiveHopLimit: 0u64,
              GetIpv6ReceivePacketInfo: 0u64,
              GetIpv6ReceiveTrafficClass: 0u64,
              GetIpv6TrafficClass: 0u64,
              GetIpv6UnicastHops: 0u64,
              GetKeepAlive: 0u64,
              GetLinger: 0u64,
              GetNoCheck: 0u64,
              GetOutOfBandInline: 0u64,
              GetReceiveBuffer: 0u64,
              GetReuseAddress: 0u64,
              GetReusePort: 0u64,
              GetSendBuffer: 0u64,
              GetTimestamp: 0u64,
              SetBindToDevice: 0u64,
              SetBroadcast: 0u64,
              SetIcmpv6Filter: 0u64,
              SetIpHeaderIncluded: 0u64,
              SetIpMulticastInterface: 0u64,
              SetIpMulticastLoopback: 0u64,
              SetIpMulticastTtl: 0u64,
              SetIpPacketInfo: 0u64,
              SetIpReceiveTtl: 0u64,
              SetIpReceiveTypeOfService: 0u64,
              SetIpTtl: 0u64,
              SetIpTypeOfService: 0u64,
              SetIpv6Checksum: 0u64,
              SetIpv6MulticastHops: 0u64,
              SetIpv6MulticastInterface: 0u64,
              SetIpv6MulticastLoopback: 0u64,
              SetIpv6Only: 0u64,
              SetIpv6ReceiveHopLimit: 0u64,
              SetIpv6ReceivePacketInfo: 0u64,
              SetIpv6ReceiveTrafficClass: 0u64,
              SetIpv6TrafficClass: 0u64,
              SetIpv6UnicastHops: 0u64,
              SetKeepAlive: 0u64,
              SetLinger: 0u64,
              SetNoCheck: 0u64,
              SetOutOfBandInline: 0u64,
              SetReceiveBuffer: 0u64,
              SetReuseAddress: 0u64,
              SetReusePort: 0u64,
              SetSendBuffer: 0u64,
              SetTimestamp: 0u64,
            },
        },
        "4": {
            BindAddress: "",
            BindNICID: "0",
            LocalAddress: "<nil>:0",
            NetworkProtocol: "UNKNOWN",
            RegisterNICID: "0",
            RemoteAddress: "<nil>:0",
            State: "",
            TransportProtocol: "UNKNOWN",
            Stats: {
              PacketsReceived: 0u64,
              PacketsSent: 0u64,
              ReadErrors: {
                InvalidEndpointState: 0u64,
                NotConnected: 0u64,
                ReadClosed: 0u64,
              },
              ReceiveErrors: {
                ChecksumErrors: 0u64,
                ClosedReceiver: 0u64,
                MalformedPacketsReceived: 0u64,
                ReceiveBufferOverflow: 0u64,
              },
              SendErrors: {
                NoRoute: 0u64,
                SendToNetworkFailed: 0u64,
              },
              WriteErrors: {
                InvalidArgs: 0u64,
                InvalidEndpointState: 0u64,
                WriteClosed: 0u64,
              },
            },
            "Socket Option Stats": {
              AddIpMembership: 0u64,
              AddIpv6Membership: 0u64,
              DropIpMembership: 0u64,
              DropIpv6Membership: 0u64,
              GetAcceptConn: 0u64,
              GetBindToDevice: 0u64,
              GetBroadcast: 0u64,
              GetError: 0u64,
              GetIpMulticastInterface: 0u64,
              GetIpMulticastLoopback: 0u64,
              GetIpMulticastTtl: 0u64,
              GetIpPacketInfo: 0u64,
              GetIpReceiveTtl: 0u64,
              GetIpReceiveTypeOfService: 0u64,
              GetIpTtl: 0u64,
              GetIpTypeOfService: 0u64,
              GetIpv6MulticastHops: 0u64,
              GetIpv6MulticastInterface: 0u64,
              GetIpv6MulticastLoopback: 0u64,
              GetIpv6Only: 0u64,
              GetIpv6ReceiveHopLimit: 0u64,
              GetIpv6ReceivePacketInfo: 0u64,
              GetIpv6ReceiveTrafficClass: 0u64,
              GetIpv6TrafficClass: 0u64,
              GetIpv6UnicastHops: 0u64,
              GetKeepAlive: 0u64,
              GetLinger: 0u64,
              GetNoCheck: 0u64,
              GetOutOfBandInline: 0u64,
              GetReceiveBuffer: 0u64,
              GetReuseAddress: 0u64,
              GetReusePort: 0u64,
              GetSendBuffer: 0u64,
              GetTimestamp: 0u64,
              SetBindToDevice: 0u64,
              SetBroadcast: 0u64,
              SetIpMulticastInterface: 0u64,
              SetIpMulticastLoopback: 0u64,
              SetIpMulticastTtl: 0u64,
              SetIpPacketInfo: 0u64,
              SetIpReceiveTtl: 0u64,
              SetIpReceiveTypeOfService: 0u64,
              SetIpTtl: 0u64,
              SetIpTypeOfService: 0u64,
              SetIpv6MulticastHops: 0u64,
              SetIpv6MulticastInterface: 0u64,
              SetIpv6MulticastLoopback: 0u64,
              SetIpv6Only: 0u64,
              SetIpv6ReceiveHopLimit: 0u64,
              SetIpv6ReceivePacketInfo: 0u64,
              SetIpv6ReceiveTrafficClass: 0u64,
              SetIpv6TrafficClass: 0u64,
              SetIpv6UnicastHops: 0u64,
              SetKeepAlive: 0u64,
              SetLinger: 0u64,
              SetNoCheck: 0u64,
              SetOutOfBandInline: 0u64,
              SetReceiveBuffer: 0u64,
              SetReuseAddress: 0u64,
              SetReusePort: 0u64,
              SetSendBuffer: 0u64,
              SetTimestamp: 0u64,
            },
        },
    });
}

struct InspectDataGetter {
    diagnostics_dir: fio::DirectoryProxy,
}

impl common::InspectDataGetter for InspectDataGetter {
    async fn get_inspect_data(&self, metric: &str) -> diagnostics_hierarchy::DiagnosticsHierarchy {
        get_inspect_data(&self.diagnostics_dir, metric, "counters").await
    }
}

#[netstack_test]
async fn inspect_for_sampler(name: &str) {
    type N = netstack_testing_common::realms::Netstack2;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");
    // Connect to netstack service to spawn a netstack instance.
    let _stack = realm
        .connect_to_protocol::<fidl_fuchsia_net_stack::StackMarker>()
        .expect("failed to connect to fuchsia.net.stack/Stack");
    let diagnostics_dir =
        realm.open_diagnostics_directory("netstack").expect("diagnostics dir exists");

    let sampler = InspectDataGetter { diagnostics_dir };

    common::inspect_for_sampler_test_inner(&sampler).await;
}

const CONFIG_DATA_EMPTY: &str = "/pkg/netstack/empty.json";
const CONFIG_DATA_SPECIFIED_PROCS: &str = "/pkg/netstack/specified_procs.json";
const CONFIG_DATA_NONEXISTENT: &str = "/pkg/netstack/idontexist.json";

#[netstack_test]
#[test_case(
    true, "DEBUG", "1m0s", false, false, CONFIG_DATA_EMPTY, None;
    "default debug config"
)]
#[test_case(
    false, "INFO", "2m0s", true, true, CONFIG_DATA_SPECIFIED_PROCS, Some(2);
    "non-default config"
)]
#[test_case(
    false, "INFO", "2m0s", true, true, CONFIG_DATA_NONEXISTENT, None;
    "config-data file is nonexistent"
)]
async fn inspect_config(
    name: &str,
    log_packets: bool,
    verbosity: &str,
    socket_stats_sampling_interval: &str,
    no_opaque_iids: bool,
    fast_udp: bool,
    config_data: &str,
    expect_max_procs_set: Option<usize>,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = {
        let mut netstack = fidl_fuchsia_netemul::ChildDef::from(&KnownServiceProvider::Netstack(
            netstack_testing_common::realms::Netstack2::VERSION,
        ));
        let fidl_fuchsia_netemul::ChildDef { program_args, .. } = &mut netstack;
        *program_args = Some(vec![
            format!("--log-packets={log_packets}"),
            format!("--verbosity={verbosity}"),
            format!("--socket-stats-sampling-interval={socket_stats_sampling_interval}"),
            format!("--no-opaque-iids={no_opaque_iids}"),
            format!("--fast-udp={fast_udp}"),
            format!("--config-data={config_data}"),
        ]);
        sandbox.create_realm(name, [netstack]).expect("create realm")
    };

    // Connect to a protocol exposed by netstack so that it starts up.
    let _ = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("connect to protocol");

    let mut inspect_assertion = tree_assertion!("Runtime Configuration Flags": {
        "log-packets": format!("{log_packets}"),
        "verbosity": verbosity.to_string(),
        "socket-stats-sampling-interval": socket_stats_sampling_interval.to_string(),
        "no-opaque-iids": format!("{no_opaque_iids}"),
        "fast-udp": format!("{fast_udp}"),
    });
    let mut config_data_assertion = tree_assertion!("config-data": {
        file: config_data.to_string(),
        "num-cpu": AnyProperty,
    });
    if let Some(max_procs) = expect_max_procs_set {
        config_data_assertion
            .add_property_assertion("max-procs", Box::new(format!("{}", max_procs)));
    }
    inspect_assertion.add_child_assertion(config_data_assertion);

    let diagnostics_dir =
        realm.open_diagnostics_directory("netstack").expect("diagnostics dir exists");
    let data =
        get_inspect_data(&diagnostics_dir, r#"Runtime\ Configuration\ Flags"#, "configuration")
            .await;
    inspect_assertion
        .run(&data)
        .unwrap_or_else(|e| panic!("unexpected inspect data: {:?}; got {:#?}", e, data));
}

async fn get_inspect_data(
    diagnostics_dir: &fio::DirectoryProxy,
    tree_selector: impl AsRef<str>,
    subdir: &str,
) -> DiagnosticsHierarchy {
    let tree_selector = tree_selector.as_ref();
    let selector =
        selectors::parse_selector::<selectors::VerboseError>(&format!("netstack:{tree_selector}"))
            .expect("parse selector");
    netstack_testing_common::get_deprecated_netstack2_inspect_data(
        diagnostics_dir,
        subdir,
        [selector],
    )
    .await
}
