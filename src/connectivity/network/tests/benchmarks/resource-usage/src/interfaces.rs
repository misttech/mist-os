// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_tun as fnet_tun;
use fuchsia_zircon as zx;
use futures::{SinkExt as _, StreamExt as _};
use net_declare::fidl_mac;
use net_types::{ip::Ip as _, Witness as _};
use std::collections::HashMap;

pub struct Interfaces;

#[async_trait(?Send)]
impl crate::Workload for Interfaces {
    const NAME: &'static str = "Interfaces";

    async fn run(netstack: &netemul::TestRealm<'_>) {
        let interfaces_state = netstack
            .connect_to_protocol::<fnet_interfaces::StateMarker>()
            .expect("connect to protocol");

        // Install several interfaces backed by network-tun.
        let interfaces = {
            let stream = fnet_interfaces_ext::event_stream_from_state(
                &interfaces_state,
                fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
            )
            .expect("get interface event stream");
            futures::pin_mut!(stream);
            let mut if_state = fnet_interfaces_ext::existing(
                stream.by_ref(),
                HashMap::<u64, fnet_interfaces_ext::PropertiesAndState<()>>::new(),
            )
            .await
            .expect("collect existing interfaces");

            const NUM_INTERFACES: usize = 10;
            let (tx, rx) = futures::channel::mpsc::channel(NUM_INTERFACES);
            futures::stream::iter(0..NUM_INTERFACES)
                .for_each_concurrent(None, |_| {
                    let interfaces_state = &interfaces_state;
                    let mut tx = tx.clone();
                    async move {
                        let interface = install_interface(netstack, interfaces_state).await;
                        tx.send(interface).await.expect("receiver should not be dropped");
                    }
                })
                .await;
            drop(tx);
            fnet_interfaces_ext::wait_interface(stream.by_ref(), &mut if_state, |interfaces| {
                (interfaces.len() == NUM_INTERFACES + 1).then_some(())
            })
            .await
            .expect("observe interface creation");
            rx
        };

        interfaces
            .for_each_concurrent(None, |interface| stress_interface(interface, &interfaces_state))
            .await;

        // Wait for the interfaces we installed to be removed.
        let stream = fnet_interfaces_ext::event_stream_from_state(
            &interfaces_state,
            fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .expect("get interface event stream");
        futures::pin_mut!(stream);
        let mut interfaces = fnet_interfaces_ext::existing(
            stream.by_ref(),
            HashMap::<u64, fnet_interfaces_ext::PropertiesAndState<()>>::new(),
        )
        .await
        .expect("collect existing interfaces");
        if interfaces.len() != 1 {
            fnet_interfaces_ext::wait_interface(stream.by_ref(), &mut interfaces, |interfaces| {
                (interfaces.len() == 1).then_some(())
            })
            .await
            .expect("observe interface removal");
        }
    }
}

struct Interface {
    id: u64,
    addr: net_types::ip::Ipv6Addr,
    control: fnet_interfaces_admin::ControlProxy,
    _device_control: fnet_interfaces_admin::DeviceControlProxy,
    tun_device: fnet_tun::DeviceProxy,
    _tun_port: fnet_tun::PortProxy,
}

const TUN_DEVICE_PORT_ID: u8 = 0;
const MAC_ADDRESS: fnet::MacAddress = fidl_mac!("02:00:00:00:00:ff");

async fn install_interface(
    netstack: &netemul::TestRealm<'_>,
    interfaces_state: &fnet_interfaces::StateProxy,
) -> Interface {
    let (tun_device, netdevice) = netstack_testing_common::devices::create_tun_device();
    let (tun_port, dev_port) = netstack_testing_common::devices::create_eth_tun_port(
        &tun_device,
        TUN_DEVICE_PORT_ID,
        MAC_ADDRESS,
    )
    .await;

    let device_control = netstack_testing_common::devices::install_device(&netstack, netdevice);
    let port_id = dev_port.get_info().await.expect("get info").id.expect("missing port id");
    let (control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
            .expect("create proxy");
    device_control
        .create_interface(&port_id, server_end, &fnet_interfaces_admin::Options::default())
        .expect("create interface");
    assert!(control.enable().await.expect("call enable").expect("enable interface"));
    tun_port.set_online(true).await.expect("can set online");

    let id = control.get_id().await.expect("get id");
    let addr = netstack_testing_common::interfaces::wait_for_v6_ll(interfaces_state, id)
        .await
        .expect("waiting for link local address");

    Interface {
        id,
        addr,
        control,
        _device_control: device_control,
        tun_device,
        _tun_port: tun_port,
    }
}

async fn stress_interface(interface: Interface, interfaces_state: &fnet_interfaces::StateProxy) {
    let Interface { id, addr, control, tun_device, .. } = interface;
    let stream = fnet_interfaces_ext::event_stream_from_state(
        &interfaces_state,
        fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
    )
    .expect("get interface event stream");
    futures::pin_mut!(stream);
    let mut state = fnet_interfaces_ext::InterfaceState::<()>::Unknown(id);

    // Repeatedly toggle interface up/down and send traffic through it
    // simulating incoming neighbor solicitations.
    const ITERATIONS: u8 = 10;
    for i in 0..ITERATIONS {
        fuchsia_async::Timer::new(zx::Duration::from_millis(50)).await;

        assert!(control.disable().await.expect("call disable").expect("disable interface"));
        fnet_interfaces_ext::wait_interface_with_id(stream.by_ref(), &mut state, |iface| {
            (!iface.properties.online).then_some(())
        })
        .await
        .expect("wait for interface offline");

        fuchsia_async::Timer::new(zx::Duration::from_millis(50)).await;

        assert!(control.enable().await.expect("call enable").expect("enable interface"));
        fnet_interfaces_ext::wait_interface_with_id(stream.by_ref(), &mut state, |iface| {
            iface.properties.online.then_some(())
        })
        .await
        .expect("wait for interface online");

        // Pick a source address that is not the same as the interface's address by
        // flipping the last byte.
        let src_ip = {
            let mut bytes = addr.ipv6_bytes();
            let last = bytes.last_mut().unwrap();
            *last = !*last;
            net_types::ip::Ipv6Addr::from_bytes(bytes)
        };
        let src_mac = net_types::ethernet::Mac::new([0, 0, 0, 0, 0, i]);

        // Simulate an incoming Neighbor Solicitation.
        let frame = serialize_neighbor_solictation(src_ip, src_mac);
        loop {
            match tun_device
                .write_frame(&fnet_tun::Frame {
                    port: Some(TUN_DEVICE_PORT_ID),
                    frame_type: Some(fhardware_network::FrameType::Ethernet),
                    data: Some(frame.clone()),
                    ..Default::default()
                })
                .await
                .expect("call write_frame")
                .map_err(zx::Status::from_raw)
            {
                Ok(()) => break,
                Err(zx::Status::SHOULD_WAIT) => continue,
                Err(e) => panic!("failed to write incoming frame to tun device: {:?}", e),
            }
        }
    }
}

fn serialize_neighbor_solictation(
    src_ip: net_types::ip::Ipv6Addr,
    src_mac: net_types::ethernet::Mac,
) -> Vec<u8> {
    use packet::serialize::{InnerPacketBuilder as _, Serializer as _};
    use packet_formats::{
        ethernet::{EtherType, EthernetFrameBuilder, ETHERNET_MIN_BODY_LEN_NO_TAG},
        icmp::{ndp::NeighborSolicitation, IcmpPacketBuilder, IcmpUnusedCode},
        ip::Ipv6Proto,
        ipv6::Ipv6PacketBuilder,
    };

    let snmc = src_ip.to_solicited_node_address();
    [].into_serializer()
        .encapsulate(IcmpPacketBuilder::<_, &[u8], _>::new(
            net_types::ip::Ipv6::UNSPECIFIED_ADDRESS,
            snmc.get(),
            IcmpUnusedCode,
            NeighborSolicitation::new(src_ip),
        ))
        .encapsulate(Ipv6PacketBuilder::new(
            net_types::ip::Ipv6::UNSPECIFIED_ADDRESS,
            snmc.get(),
            netstack_testing_common::ndp::MESSAGE_TTL,
            Ipv6Proto::Icmpv6,
        ))
        .encapsulate(EthernetFrameBuilder::new(
            src_mac,
            net_types::ethernet::Mac::from(&snmc),
            EtherType::Ipv6,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .expect("serialize NDP packet in Ethernet frame")
        .unwrap_b()
        .into_inner()
}
