// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fuchsia_async::{DurationExt as _, TimeoutExt as _};
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_routes as fnet_routes, fidl_fuchsia_net_routes_ext as fnet_routes_ext,
};

use futures::{FutureExt as _, StreamExt as _};
use net_declare::{net_ip_v4, std_ip_v4};
use net_types::ip::{self as net_types_ip, Ipv4, Ipv4Addr};
use net_types::MulticastAddress as _;
use netemul::RealmUdpSocket;
use netstack_testing_common::interfaces::{self, TestInterfaceExt};
use netstack_testing_common::realms::{Netstack, NetstackVersion, TestSandboxExt};
use netstack_testing_common::{setup_network, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT};
use netstack_testing_macros::netstack_test;
use packet::ParsablePacket as _;
use packet_formats::ethernet::{EtherType, EthernetFrame, EthernetFrameLengthCheck};
use packet_formats::igmp::messages::{
    IgmpGroupRecordType, IgmpMembershipReportV1, IgmpMembershipReportV2, IgmpMembershipReportV3,
};
use packet_formats::igmp::{IgmpMessage, MessageType};
use packet_formats::ip::Ipv4Proto;
use packet_formats::testutil::parse_ip_packet;
use std::pin::pin;
use test_case::test_case;

fn check_igmpv1v2_report<'a, M: MessageType<&'a [u8], FixedHeader = net_types_ip::Ipv4Addr>>(
    dst_ip: net_types_ip::Ipv4Addr,
    igmp: IgmpMessage<&'a [u8], M>,
    expected_group: net_types_ip::Ipv4Addr,
) -> bool {
    let group_addr = igmp.group_addr();
    assert!(
        group_addr.is_multicast(),
        "IGMP reports must only be sent for multicast addresses; group_addr = {}",
        group_addr
    );

    if group_addr != expected_group {
        // We are only interested in the report for the multicast group we
        // joined.
        return false;
    }

    assert_eq!(
        dst_ip, group_addr,
        "the destination of an IGMP report should be the multicast group the report is for"
    );

    true
}

fn check_igmpv1_report(
    dst_ip: net_types_ip::Ipv4Addr,
    mut payload: &[u8],
    expected_group: net_types_ip::Ipv4Addr,
) -> bool {
    check_igmpv1v2_report(
        dst_ip,
        IgmpMessage::<_, IgmpMembershipReportV1>::parse(&mut payload, ())
            .expect("error parsing IGMP message"),
        expected_group,
    )
}

fn check_igmpv2_report(
    dst_ip: net_types_ip::Ipv4Addr,
    mut payload: &[u8],
    expected_group: net_types_ip::Ipv4Addr,
) -> bool {
    check_igmpv1v2_report(
        dst_ip,
        IgmpMessage::<_, IgmpMembershipReportV2>::parse(&mut payload, ())
            .expect("error parsing IGMP message"),
        expected_group,
    )
}

fn check_igmpv3_report(
    dst_ip: net_types_ip::Ipv4Addr,
    mut payload: &[u8],
    expected_group: net_types_ip::Ipv4Addr,
) -> bool {
    let igmp = IgmpMessage::<_, IgmpMembershipReportV3>::parse(&mut payload, ())
        .expect("error parsing IGMP message");

    let records = igmp
        .body()
        .iter()
        .map(|record| {
            let hdr = record.header();

            (*hdr.multicast_addr(), hdr.record_type(), record.sources().to_vec())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        records,
        [(expected_group, Ok(IgmpGroupRecordType::ChangeToExcludeMode), Vec::new(),)]
    );

    assert_eq!(
        dst_ip,
        net_ip_v4!("224.0.0.22"),
        "IGMPv3 reports should should be sent to the IGMPv3 routers address",
    );

    true
}

fn check_igmp_report(
    igmp_version: Option<fnet_interfaces_admin::IgmpVersion>,
    dst_ip: net_types_ip::Ipv4Addr,
    payload: &[u8],
    expected_group: net_types_ip::Ipv4Addr,
) -> bool {
    match igmp_version.unwrap_or(fnet_interfaces_admin::IgmpVersion::V3) {
        fnet_interfaces_admin::IgmpVersion::V1 => {
            check_igmpv1_report(dst_ip, payload, expected_group)
        }
        fnet_interfaces_admin::IgmpVersion::V2 => {
            check_igmpv2_report(dst_ip, payload, expected_group)
        }
        fnet_interfaces_admin::IgmpVersion::V3 => {
            check_igmpv3_report(dst_ip, payload, expected_group)
        }
        other => panic!("unknown IGMP version {:?}", other),
    }
}

#[netstack_test]
#[variant(N, Netstack)]
#[test_case(Some(fnet_interfaces_admin::IgmpVersion::V1); "igmpv1")]
#[test_case(Some(fnet_interfaces_admin::IgmpVersion::V2); "igmpv2")]
#[test_case(Some(fnet_interfaces_admin::IgmpVersion::V3); "igmpv3")]
#[test_case(None; "default")]
async fn sends_igmp_reports<N: Netstack>(
    name: &str,
    igmp_version: Option<fnet_interfaces_admin::IgmpVersion>,
) {
    const INTERFACE_ADDR: std::net::Ipv4Addr = std_ip_v4!("192.168.0.1");
    const MULTICAST_ADDR: std::net::Ipv4Addr = std_ip_v4!("224.1.2.3");

    let sandbox = netemul::TestSandbox::new().expect("error creating sandbox");
    let (_network, realm, iface, fake_ep) =
        setup_network::<N>(&sandbox, name, None).await.expect("error setting up network");

    if let Some(igmp_version) = igmp_version {
        let gen_config = |igmp_version| fnet_interfaces_admin::Configuration {
            ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                igmp: Some(fnet_interfaces_admin::IgmpConfiguration {
                    version: Some(igmp_version),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let control = iface.control();
        let new_config = gen_config(igmp_version);
        let old_config = gen_config(fnet_interfaces_admin::IgmpVersion::V3);
        assert_eq!(
            control
                .set_configuration(&new_config)
                .await
                .expect("set_configuration fidl error")
                .expect("failed to set interface configuration"),
            old_config,
        );
        assert_eq!(
            control
                .set_configuration(&new_config)
                .await
                .expect("set_configuration fidl error")
                .expect("failed to set interface configuration"),
            new_config,
        );
        assert_matches::assert_matches!(
            control
                .get_configuration()
                .await
                .expect("get_configuration fidl error")
                .expect("failed to get interface configuration"),
            fnet_interfaces_admin::Configuration {
                ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                    igmp: Some(fnet_interfaces_admin::IgmpConfiguration {
                        version: Some(got),
                        ..
                    }),
                    ..
                }),
                ..
            } => assert_eq!(got, igmp_version)
        );
    }

    let addr = fnet::Ipv4Address { addr: INTERFACE_ADDR.octets() };
    let _address_state_provider = interfaces::add_address_wait_assigned(
        iface.control(),
        fnet::Subnet { addr: fnet::IpAddress::Ipv4(addr), prefix_len: 24 },
        fidl_fuchsia_net_interfaces_admin::AddressParameters {
            add_subnet_route: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("add subnet address and route");

    let sock = fuchsia_async::net::UdpSocket::bind_in_realm(
        &realm,
        std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
    )
    .await
    .expect("error creating socket");

    let () = sock
        .as_ref()
        .join_multicast_v4(&MULTICAST_ADDR, &INTERFACE_ADDR)
        .expect("error joining multicast group");

    let net_types_ip_multicast_addr = net_types_ip::Ipv4Addr::new(MULTICAST_ADDR.octets());

    let stream = fake_ep.frame_stream().map(|r| r.expect("error getting OnData event")).filter_map(
        |(data, dropped)| {
            async move {
                assert_eq!(dropped, 0);
                let mut data = &data[..];

                // Do not check the frame length as the size of IGMP reports may be less
                // than the minimum ethernet frame length and our virtual (netemul) interface
                // does not pad runt ethernet frames before transmission.
                let eth = EthernetFrame::parse(&mut data, EthernetFrameLengthCheck::NoCheck)
                    .expect("error parsing ethernet frame");

                if eth.ethertype() != Some(EtherType::Ipv4) {
                    // Ignore non-IPv4 packets.
                    return None;
                }

                let (payload, src_ip, dst_ip, proto, ttl) =
                    parse_ip_packet::<net_types_ip::Ipv4>(&data)
                        .expect("error parsing IPv4 packet");

                if proto != Ipv4Proto::Igmp {
                    // Ignore non-IGMP packets.
                    return None;
                }

                // TODO(https://fxbug.dev/42180878): Don't send IGMP reports before a local address
                // is assigned.
                if N::VERSION != NetstackVersion::Netstack3 {
                    assert_eq!(
                        src_ip,
                        net_types_ip::Ipv4Addr::new(INTERFACE_ADDR.octets()),
                        "IGMP messages must be sent from an address assigned to the NIC",
                    );
                }

                // As per RFC 2236 section 2,
                //
                //   All IGMP messages described in this document are sent with
                //   IP TTL 1, ...
                assert_eq!(ttl, 1, "IGMP messages must have a TTL of 1");

                check_igmp_report(igmp_version, dst_ip, payload, net_types_ip_multicast_addr)
                    .then_some(())
            }
        },
    );
    let mut stream = pin!(stream);
    let () = stream
        .next()
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
            panic!("timed out waiting for the IGMP report");
        })
        .await
        .expect("error getting our expected IGMP report");
}

#[netstack_test]
#[variant(N, Netstack)]
async fn all_ones_broadcast<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("error creating sandbox");

    let name_suffixes = ["a", "b", "c"];
    let realms = name_suffixes.map(|suffix| {
        sandbox
            .create_netstack_realm::<N, _>(format!("{name}_{suffix}"))
            .unwrap_or_else(|e| panic!("create realm {suffix}: {e:?}"))
    });
    let network = sandbox.create_network(name).await.expect("create network");

    let addr_subnets = [
        net_declare::fidl_subnet!("192.168.0.1/24"),
        net_declare::fidl_subnet!("192.168.0.2/24"),
        net_declare::fidl_subnet!("192.168.0.3/24"),
    ];

    // Keep same as first `addr_subnet`.
    const SENDER_IP: std::net::IpAddr = net_declare::std_ip!("192.168.0.1");

    const DEFAULT_SUBNET: net_types::ip::Subnet<Ipv4Addr> =
        net_declare::net_subnet_v4!("0.0.0.0/0");

    let mut ifaces = Vec::new();
    for (i, realm) in realms.iter().enumerate() {
        let iface = realm
            .join_network(&network, format!("{name}_{}", name_suffixes[i]))
            .await
            .expect("join network");
        iface.add_address(addr_subnets[i]).await.expect("add address");
        iface.apply_nud_flake_workaround().await.expect("apply nud flake workaround");
        let global_route_set = iface
            .create_authenticated_global_route_set::<Ipv4>()
            .await
            .expect("create authenticated route set");

        // Add an on-link default route so that the netstacks should each be
        // able to receive broadcasts from each other.
        let default_route = fnet_routes_ext::Route::<Ipv4> {
            action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                outbound_interface: iface.id(),
                next_hop: None,
            }),
            destination: DEFAULT_SUBNET,
            properties: fnet_routes_ext::RouteProperties {
                specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                    metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(
                        fnet_routes::Empty,
                    ),
                },
            },
        };
        assert!(
            global_route_set
                .add_route(&default_route.try_into().expect("convert to FIDL route"))
                .await
                .expect("adding default route should not get FIDL error")
                .expect("adding default route should succeed"),
            "should have newly-added default route"
        );
        ifaces.push(iface);
    }

    let sending_realm = &realms[0];

    // Using 1024 as arbitrary port for sending/receiving.
    const PORT: u16 = 1024;

    let make_socket = |realm| async move {
        let socket = fuchsia_async::net::UdpSocket::bind_in_realm(
            realm,
            std::net::SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), PORT),
        )
        .await
        .expect("bind in realm");
        socket.set_broadcast(true).expect("set broadcast");
        socket
    };

    let sending_socket = make_socket(sending_realm).await;

    let mut receiving_sockets = Vec::new();
    for receiving_realm in &realms[1..] {
        receiving_sockets.push(make_socket(receiving_realm).await);
    }

    const PAYLOAD: &str = "hello";
    assert_eq!(
        sending_socket
            .send_to(
                PAYLOAD.as_bytes(),
                std::net::SocketAddr::new(std::net::Ipv4Addr::BROADCAST.into(), PORT),
            )
            .await
            .expect("send should succeed"),
        PAYLOAD.len()
    );

    let mut buf = [0u8; 16];

    for receiving_socket in receiving_sockets {
        let (n, received_from) = receiving_socket
            .recv_from(&mut buf)
            .map(Some)
            .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT, || None)
            .await
            .expect("should not have timed out")
            .expect("recv_from");
        assert_eq!(n, PAYLOAD.len());
        assert_eq!(&buf[..n], PAYLOAD.as_bytes());
        assert_eq!(received_from, std::net::SocketAddr::new(SENDER_IP, PORT));
    }
}
