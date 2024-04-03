// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]
// Needed for invocations of the `assert_data_tree` macro.
#![recursion_limit = "256"]

use std::{collections::HashMap, convert::TryFrom as _};

use fidl_fuchsia_posix_socket as fposix_socket;

use net_declare::{fidl_mac, fidl_subnet, std_ip_v4};
use net_types::{
    ip::{IpAddress, IpVersion, Ipv4, Ipv6},
    AddrAndPortFormatter, Witness as _,
};
use netstack_testing_common::{constants, get_inspect_data, realms::TestSandboxExt as _};
use netstack_testing_macros::netstack_test;
use packet_formats::ethernet::testutil::ETHERNET_HDR_LEN_NO_TAG;
use test_case::test_case;

#[netstack_test]
async fn inspect_sockets<I: net_types::ip::Ip>(name: &str) {
    type N = netstack_testing_common::realms::Netstack3;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");
    let network = sandbox.create_network("net").await.expect("failed to create network");

    let interfaces_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("connect to protocol");
    let dev = realm.join_network(&network, "dev").await.expect("join network");
    let link_local =
        netstack_testing_common::interfaces::wait_for_v6_ll(&interfaces_state, dev.id())
            .await
            .expect("wait for v6 link local");
    let scope = dev.id().try_into().unwrap();

    // Ensure ns3 has started and that there is a Socket to collect inspect data about.
    const PORT: u16 = 8080;
    let (domain, addr) = match I::VERSION {
        IpVersion::V4 => {
            (fposix_socket::Domain::Ipv4, std::net::SocketAddr::from((std_ip_v4!("0.0.0.0"), PORT)))
        }
        IpVersion::V6 => (
            fposix_socket::Domain::Ipv6,
            std::net::SocketAddr::from(std::net::SocketAddrV6::new(
                link_local.into(),
                PORT,
                0,
                scope,
            )),
        ),
    };
    let tcp_socket = realm
        .stream_socket(domain, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create TCP socket");
    tcp_socket.bind(&addr.into()).expect("bind");

    let data =
        get_inspect_data(&realm, "netstack", "root", constants::inspect::DEFAULT_INSPECT_TREE_NAME)
            .await
            .expect("inspect data should be present");

    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);

    // NB: The sockets are keyed by an opaque debug identifier; get that here.
    let sockets = data.get_child("Sockets").unwrap();
    assert_eq!(sockets.children.len(), 1);
    let sock_name = sockets.children[0].name.clone();

    match I::VERSION {
        IpVersion::V4 => {
            diagnostics_assertions::assert_data_tree!(data, "root": contains {
                    "Sockets": {
                        sock_name => {
                            LocalAddress: format!("0.0.0.0:{PORT}"),
                            RemoteAddress: "[NOT CONNECTED]",
                            TransportProtocol: "TCP",
                            NetworkProtocol: "IPv4",
                        },
                    }
            })
        }
        IpVersion::V6 => {
            diagnostics_assertions::assert_data_tree!(data, "root": contains {
                "Sockets": {
                    sock_name => {
                        LocalAddress: format!("[{link_local}%{scope}]:{PORT}"),
                        RemoteAddress: "[NOT CONNECTED]",
                        TransportProtocol: "TCP",
                        NetworkProtocol: "IPv6",
                    }
                }
            })
        }
    }
}

enum SocketState {
    Bound,
    Connected,
}

trait TestIpExt: net_types::ip::Ip {
    const DOMAIN: fposix_socket::Domain;
}

impl TestIpExt for Ipv4 {
    const DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv4;
}

impl TestIpExt for Ipv6 {
    const DOMAIN: fposix_socket::Domain = fposix_socket::Domain::Ipv6;
}

#[netstack_test]
#[test_case(
    fposix_socket::DatagramSocketProtocol::Udp, SocketState::Bound;
    "udp_bound"
)]
#[test_case(
    fposix_socket::DatagramSocketProtocol::IcmpEcho, SocketState::Bound;
    "icmp_bound"
)]
#[test_case(
    fposix_socket::DatagramSocketProtocol::Udp, SocketState::Connected;
    "udp_connected"
)]
#[test_case(
    fposix_socket::DatagramSocketProtocol::IcmpEcho, SocketState::Connected;
    "icmp_connected"
)]
async fn inspect_datagram_sockets<I: net_types::ip::Ip + TestIpExt>(
    name: &str,
    proto: fposix_socket::DatagramSocketProtocol,
    socket_state: SocketState,
) {
    type N = netstack_testing_common::realms::Netstack3;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");

    // Ensure ns3 has started and that there is a Socket to collect inspect data about.
    let socket = realm.datagram_socket(I::DOMAIN, proto).await.expect("create datagram socket");
    const SRC_PORT: u16 = 1234;
    const DST_PORT: u16 = 5678;
    let addr = std::net::IpAddr::from(
        match socket_state {
            SocketState::Bound => I::UNSPECIFIED_ADDRESS,
            SocketState::Connected => I::LOOPBACK_ADDRESS.get(),
        }
        .to_ip_addr(),
    );
    socket.bind(&std::net::SocketAddr::from((addr, SRC_PORT)).into()).expect("bind");

    match socket_state {
        SocketState::Connected => {
            socket.connect(&std::net::SocketAddr::from((addr, DST_PORT)).into()).expect("connect");
        }
        SocketState::Bound => {}
    }

    let want_local = AddrAndPortFormatter::<_, _, I>::new(addr, SRC_PORT).to_string();
    let want_remote = match socket_state {
        SocketState::Connected => match proto {
            fposix_socket::DatagramSocketProtocol::Udp => {
                AddrAndPortFormatter::<_, _, I>::new(addr, DST_PORT).to_string()
            }
            fposix_socket::DatagramSocketProtocol::IcmpEcho => addr.to_string(),
        },
        SocketState::Bound => "[NOT CONNECTED]".to_string(),
    };
    let want_proto = match proto {
        fposix_socket::DatagramSocketProtocol::Udp => "UDP",
        fposix_socket::DatagramSocketProtocol::IcmpEcho => "ICMP_ECHO",
    };

    let data =
        get_inspect_data(&realm, "netstack", "root", constants::inspect::DEFAULT_INSPECT_TREE_NAME)
            .await
            .expect("inspect data should be present");

    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);
    // NB: The sockets are keyed by an opaque debug identifier.
    let sockets = data.get_child("Sockets").unwrap();
    assert_eq!(sockets.children.len(), 1);
    let sock_name = sockets.children[0].name.clone();
    diagnostics_assertions::assert_data_tree!(data, "root": contains {
        Sockets: {
            sock_name => {
                LocalAddress: want_local,
                RemoteAddress: want_remote,
                TransportProtocol: want_proto,
                NetworkProtocol: I::NAME,
            },
        }
    })
}

#[netstack_test]
async fn inspect_routes(name: &str) {
    type N = netstack_testing_common::realms::Netstack3;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");

    let interfaces_state = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to fuchsia.net.interfaces/State");
    let loopback_id = fidl_fuchsia_net_interfaces_ext::wait_interface(
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(
            &interfaces_state,
            fidl_fuchsia_net_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .expect("failed to create event stream"),
        &mut HashMap::<u64, fidl_fuchsia_net_interfaces_ext::PropertiesAndState<()>>::new(),
        |if_map| {
            if_map.values().find_map(
                |fidl_fuchsia_net_interfaces_ext::PropertiesAndState {
                     properties:
                         fidl_fuchsia_net_interfaces_ext::Properties { device_class, id, .. },
                     state: (),
                 }| {
                    match device_class {
                        fidl_fuchsia_net_interfaces::DeviceClass::Loopback(
                            fidl_fuchsia_net_interfaces::Empty {},
                        ) => Some(id.get()),
                        fidl_fuchsia_net_interfaces::DeviceClass::Device(_) => None,
                    }
                },
            )
        },
    )
    .await
    .expect("getting loopback id");

    let data =
        get_inspect_data(&realm, "netstack", "root", constants::inspect::DEFAULT_INSPECT_TREE_NAME)
            .await
            .expect("inspect data should be present");

    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);
    diagnostics_assertions::assert_data_tree!(data, "root": contains {
        "Routes": {
            "0": {
                Destination: "255.255.255.255/32",
                InterfaceId: loopback_id,
                Gateway: "[NONE]",
                Metric: 99999u64,
                MetricTracksInterface: false,
            },
            "1": {
                Destination: "127.0.0.0/8",
                InterfaceId: loopback_id,
                Gateway: "[NONE]",
                Metric: 100u64,
                MetricTracksInterface: true,
            },
            "2": {
                Destination: "224.0.0.0/4",
                InterfaceId: loopback_id,
                Gateway: "[NONE]",
                Metric: 100u64,
                MetricTracksInterface: true,
            },
            "3": {
                Destination: "::1/128",
                InterfaceId: loopback_id,
                Gateway: "[NONE]",
                Metric: 100u64,
                MetricTracksInterface: true,
            },
            "4": {
                Destination: "ff00::/8",
                InterfaceId: loopback_id,
                Gateway: "[NONE]",
                Metric: 100u64,
                MetricTracksInterface: true,
            },
        }
    })
}

#[netstack_test]
async fn inspect_devices(name: &str) {
    type N = netstack_testing_common::realms::Netstack3;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = sandbox.create_network("net").await.expect("failed to create network");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");

    // Install netdevice device so that non-Loopback device Inspect properties can be asserted upon.
    const NETDEV_NAME: &str = "test-eth";
    let max_frame_size = netemul::DEFAULT_MTU
        + u16::try_from(ETHERNET_HDR_LEN_NO_TAG)
            .expect("should fit ethernet header length in a u16");
    let netdev = realm
        .join_network_with(
            &network,
            "netdev-ep",
            netemul::new_endpoint_config(max_frame_size, Some(fidl_mac!("02:00:00:00:00:01"))),
            netemul::InterfaceConfig {
                name: Some(NETDEV_NAME.into()),
                metric: None,
                dad_transmits: Some(u16::MAX),
            },
        )
        .await
        .expect("failed to join network with netdevice endpoint");

    netdev
        .add_address_and_subnet_route(fidl_subnet!("192.168.0.1/24"))
        .await
        .expect("configure address");

    let data =
        get_inspect_data(&realm, "netstack", "root", constants::inspect::DEFAULT_INSPECT_TREE_NAME)
            .await
            .expect("inspect data should be present");

    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);
    diagnostics_assertions::assert_data_tree!(data, "root": contains {
        "Devices": {
            "1": {
                Name: "lo",
                InterfaceId: 1u64,
                AdminEnabled: true,
                MTU: 65536u64,
                Loopback: true,
                IPv4: {
                    Addresses: {
                        "127.0.0.1/8": {
                            ValidUntil: "infinite",
                        }
                    }
                },
                IPv6: {
                    Addresses: {
                        "::1/128": {
                            ValidUntil: "infinite",
                            IsSlaac: false,
                            Deprecated: false,
                            Assigned: true,
                        }
                    }
                },
                Counters: {
                    Rx: {
                        TotalFrames: 0u64,
                        Malformed: 0u64,
                        Ipv4Delivered: 0u64,
                        Ipv6Delivered: 0u64,
                    },
                    Tx: {
                        TotalFrames: 0u64,
                        Sent: 0u64,
                        SendIpv4Frame: 0u64,
                        SendIpv6Frame: 0u64,
                        NoQueue: 0u64,
                        QueueFull: 0u64,
                        SerializeError: 0u64,
                    },
                    Ethernet: {
                        Rx: {
                            NoEthertype: 0u64,
                            NonLocalDstAddr: 0u64,
                            UnsupportedEthertype: 0u64,
                        },
                    },
                }
            },
            "2": {
                Name: NETDEV_NAME,
                InterfaceId: 2u64,
                AdminEnabled: true,
                MTU: u64::from(netemul::DEFAULT_MTU),
                Loopback: false,
                IPv4: {
                    "Addresses": {
                        "192.168.0.1/24": {
                            ValidUntil: "infinite"
                        }
                    }
                },
                IPv6: {
                    "Addresses": {
                        "fe80::ff:fe00:1/64": {
                            ValidUntil: "infinite",
                            IsSlaac: true,
                            Deprecated: false,
                            // This will always be `false` because DAD will never complete; we set
                            // the number of DAD transmits to `u16::MAX` above.
                            Assigned: false,
                        }
                    }
                },
                NetworkDevice: {
                    MacAddress: "02:00:00:00:00:01",
                    PhyUp: true,
                },
                Counters: {
                    Rx: {
                        TotalFrames: 0u64,
                        Malformed: 0u64,
                        Ipv4Delivered: 0u64,
                        Ipv6Delivered: 0u64,
                    },
                    Tx: {
                        TotalFrames: diagnostics_assertions::AnyUintProperty,
                        Sent: diagnostics_assertions::AnyUintProperty,
                        SendIpv4Frame: diagnostics_assertions::AnyUintProperty,
                        SendIpv6Frame: diagnostics_assertions::AnyUintProperty,
                        NoQueue: 0u64,
                        QueueFull: 0u64,
                        SerializeError: 0u64,
                    },
                    Ethernet: {
                        Rx: {
                            NoEthertype: 0u64,
                            NonLocalDstAddr: 0u64,
                            UnsupportedEthertype: 0u64,
                        },
                    },
                }
            }
        }
    })
}

#[netstack_test]
async fn inspect_counters(name: &str) {
    type N = netstack_testing_common::realms::Netstack3;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let _network = sandbox.create_network("net").await.expect("failed to create network");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");

    // Send a packet over loopback to increment Tx and Rx count by 1.
    let sender = realm
        .datagram_socket(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("datagram socket creation failed");
    let addr = net_declare::std_socket_addr!("127.0.0.1:8080");
    let buf = [0; 8];
    let bytes_sent = sender.send_to(&buf, &addr.into()).expect("socket send to failed");
    assert_eq!(bytes_sent, buf.len());

    let data =
        get_inspect_data(&realm, "netstack", "root", constants::inspect::DEFAULT_INSPECT_TREE_NAME)
            .await
            .expect("inspect data should be present");

    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);
    diagnostics_assertions::assert_data_tree!(data, "root": contains {
        "Counters": {
            "Device": {
                "Rx": {
                    TotalFrames: 1u64,
                    Malformed: 0u64,
                    Ipv4Delivered: 1u64,
                    Ipv6Delivered: 0u64,
                },
                "Tx": {
                    TotalFrames: 1u64,
                    Sent: 1u64,
                    SendIpv4Frame: 1u64,
                    SendIpv6Frame: 0u64,
                    NoQueue: 0u64,
                    QueueFull: 0u64,
                    SerializeError: 0u64,
                },
                "Ethernet": {
                    "Rx": {
                        NoEthertype: 0u64,
                        NonLocalDstAddr: 0u64,
                        UnsupportedEthertype: 0u64,
                    },
                },
            },
            "Arp": {
                "Rx": {
                    TotalPackets: 0u64,
                    Requests: 0u64,
                    Responses: 0u64,
                    Malformed: 0u64,
                    NonLocalDstAddr: 0u64,
                },
                "Tx": {
                    Requests: 0u64,
                    RequestsNonLocalSrcAddr: 0u64,
                    Responses: 0u64,
                },
            },
            "NUD": {
                "V4": {
                    IcmpDestUnreachableDropped: 0u64,
                },
                "V6": {
                    IcmpDestUnreachableDropped: 0u64,
                },
            },
            "ICMP": {
                "V4": {
                    "Rx": {
                        EchoRequest: 0u64,
                        EchoReply: 0u64,
                        TimestampRequest: 0u64,
                        DestUnreachable: 0u64,
                        TimeExceeded: 0u64,
                        ParameterProblem: 0u64,
                        PacketTooBig: 0u64,
                        Error: 0u64,
                        ErrorDeliveredToTransportLayer: 0u64,
                        ErrorDeliveredToSocket: 0u64,
                    },
                    "Tx": {
                        Reply: 0u64,
                        AddressUnreachable: 0u64,
                        ProtocolUnreachable: 0u64,
                        PortUnreachable: 0u64,
                        NetUnreachable: 0u64,
                        TtlExpired: 0u64,
                        PacketTooBig: 0u64,
                        ParameterProblem: 0u64,
                        DestUnreachable: 0u64,
                        Error: 0u64,
                    },
                },
                "V6": {
                    "Rx": {
                        EchoRequest: 0u64,
                        EchoReply: 0u64,
                        TimestampRequest: 0u64,
                        DestUnreachable: 0u64,
                        TimeExceeded: 0u64,
                        ParameterProblem: 0u64,
                        PacketTooBig: 0u64,
                        Error: 0u64,
                        ErrorDeliveredToTransportLayer: 0u64,
                        ErrorDeliveredToSocket: 0u64,
                        "NDP": {
                            NeighborSolicitation: 0u64,
                            NeighborAdvertisement: 0u64,
                            RouterSolicitation: 0u64,
                            RouterAdvertisement: 0u64,
                        },
                    },
                    "Tx": {
                        Reply: 0u64,
                        AddressUnreachable: 0u64,
                        ProtocolUnreachable: 0u64,
                        PortUnreachable: 0u64,
                        NetUnreachable: 0u64,
                        TtlExpired: 0u64,
                        PacketTooBig: 0u64,
                        ParameterProblem: 0u64,
                        DestUnreachable: 0u64,
                        Error: 0u64,
                        "NDP": {
                            NeighborAdvertisement: 0u64,
                            NeighborSolicitation: 0u64,
                        },
                    },
                },
            },
            "IPv4": {
                PacketTx: 1u64,
                "PacketRx": {
                    Received: 1u64,
                    Dispatched: 1u64,
                    Delivered: 1u64,
                    OtherHost: 0u64,
                    ParameterProblem: 0u64,
                    UnspecifiedDst: 0u64,
                    UnspecifiedSrc: 0u64,
                    Dropped: 0u64,
                },
                "Forwarding": {
                    Forwarded: 0u64,
                    ForwardingDisabled: 0u64,
                    NoRouteToHost: 0u64,
                    MtuExceeded: 0u64,
                    TtlExpired: 0u64,
                },
                RxIcmpError: 0u64,
                "Fragments": {
                    ReassemblyError: 0u64,
                    NeedMoreFragments: 0u64,
                    InvalidFragment: 0u64,
                    CacheFull: 0u64,
                },
            },
            "IPv6": {
                PacketTx: 0u64,
                "PacketRx": {
                    Received: 0u64,
                    Dispatched: 0u64,
                    DeliveredMulticast: 0u64,
                    DeliveredUnicast: 0u64,
                    OtherHost: 0u64,
                    ParameterProblem: 0u64,
                    UnspecifiedDst: 0u64,
                    UnspecifiedSrc: 0u64,
                    Dropped: 0u64,
                    DroppedTentativeDst: 0u64,
                    DroppedNonUnicastSrc: 0u64,
                    DroppedExtensionHeader: 0u64,
                },
                "Forwarding": {
                    Forwarded: 0u64,
                    ForwardingDisabled: 0u64,
                    NoRouteToHost: 0u64,
                    MtuExceeded: 0u64,
                    TtlExpired: 0u64,
                },
                RxIcmpError: 0u64,
                "Fragments": {
                    ReassemblyError: 0u64,
                    NeedMoreFragments: 0u64,
                    InvalidFragment: 0u64,
                    CacheFull: 0u64,
                },
            },
            "UDP": {
                "V4": {
                    "Rx": {
                        Received: 1u64,
                        "Errors": {
                            MappedAddr: 0u64,
                            UnknownDstPort: 0u64,
                            Malformed: 0u64,
                        },
                    },
                    "Tx": {
                        Sent: 1u64,
                        Errors: 0u64,
                    },
                    IcmpErrors: 0u64,
                },
                "V6": {
                    "Rx": {
                        Received: 0u64,
                        "Errors": {
                            MappedAddr: 0u64,
                            UnknownDstPort: 0u64,
                            Malformed: 0u64,
                        },
                    },
                    "Tx": {
                        Sent: 0u64,
                        Errors: 0u64,
                    },
                    IcmpErrors: 0u64,
                },
            },
            "TCP": {
                "V4": {
                    PassiveConnectionOpenings: 0u64,
                    ActiveConnectionOpenings: 0u64,
                    "Rx": {
                        ValidSegmentsReceived: 0u64,
                        ReceivedSegmentsDispatched: 0u64,
                        "Errors": {
                            InvalidIpAddrsReceived: 0u64,
                            InvalidIpPacketsReceived: 0u64,
                            InvalidSegmentsReceived: 0u64,
                            ReceivedSegmentsNoDispatch: 0u64,
                            ListenerQueueOverflow: 0u64,
                            PassiveOpenNoRouteErrors: 0u64,
                        },
                    },
                    "Tx": {
                        SegmentsSent: 0u64,
                        "Errors": {
                            SegmentSendErrors: 0u64,
                            ActiveOpenNoRouteErrors: 0u64,
                        }
                    },
                    "Errors": {
                        FailedConnectionOpenings: 0u64,
                        FailedPortReservations: 0u64,
                    }
                },
                "V6": {
                    PassiveConnectionOpenings: 0u64,
                    ActiveConnectionOpenings: 0u64,
                    "Rx": {
                        ValidSegmentsReceived: 0u64,
                        ReceivedSegmentsDispatched: 0u64,
                        "Errors": {
                            InvalidIpAddrsReceived: 0u64,
                            InvalidIpPacketsReceived: 0u64,
                            InvalidSegmentsReceived: 0u64,
                            ReceivedSegmentsNoDispatch: 0u64,
                            ListenerQueueOverflow: 0u64,
                            PassiveOpenNoRouteErrors: 0u64,
                        },
                    },
                    "Tx": {
                        SegmentsSent: 0u64,
                        "Errors": {
                            SegmentSendErrors: 0u64,
                            ActiveOpenNoRouteErrors: 0u64,
                        }
                    },
                    "Errors": {
                        FailedConnectionOpenings: 0u64,
                        FailedPortReservations: 0u64,
                    }
                },
            },
        }
    })
}
