// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]
// Needed for invocations of the `assert_data_tree` macro.
#![recursion_limit = "256"]

use std::{collections::HashMap, convert::TryFrom as _};

use fidl_fuchsia_posix_socket as fposix_socket;

use net_declare::{fidl_mac, fidl_subnet};
use netstack_testing_common::{constants, get_inspect_data, realms::TestSandboxExt as _};
use netstack_testing_macros::netstack_test;
use packet_formats::ethernet::testutil::ETHERNET_HDR_LEN_NO_TAG;

#[netstack_test]
async fn inspect_sockets(name: &str) {
    type N = netstack_testing_common::realms::Netstack3;
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("failed to create realm");

    // Ensure ns3 has started and that there is a Socket to collect inspect data about.
    let _tcp_socket = realm
        .stream_socket(fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp)
        .await
        .expect("create TCP socket");

    let data =
        get_inspect_data(&realm, "netstack", "root", constants::inspect::DEFAULT_INSPECT_TREE_NAME)
            .await
            .expect("inspect data should be present");

    // Debug print the tree to make debugging easier in case of failures.
    println!("Got inspect data: {:#?}", data);
    fuchsia_inspect::assert_data_tree!(data, "root": contains {
        "Sockets": {
            "0": {
                LocalAddress: "[NOT BOUND]",
                RemoteAddress: "[NOT CONNECTED]",
                TransportProtocol: "TCP",
                NetworkProtocol: "IPv4"
            }
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
    fuchsia_inspect::assert_data_tree!(data, "root": contains {
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
            netemul::InterfaceConfig { name: Some(NETDEV_NAME.into()), metric: None },
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
    fuchsia_inspect::assert_data_tree!(data, "root": contains {
        "Devices": {
            "1": {
                Name: "lo",
                InterfaceId: 1u64,
                AdminEnabled: true,
                MTU: 65536u64,
                Loopback: true,
                IpAddresses: Vec::<String>::new(),
            },
            "2": {
                Name: NETDEV_NAME,
                InterfaceId: 2u64,
                AdminEnabled: true,
                MTU: u64::from(netemul::DEFAULT_MTU),
                Loopback: false,
                IpAddresses: vec!["192.168.0.1".to_string()],
                NetworkDevice: {
                    MacAddress: "2:0:0:0:0:1",
                    PhyUp: true,
                },
            }
        }
    })
}
