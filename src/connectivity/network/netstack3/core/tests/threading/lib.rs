// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

extern crate netstack3_core_loom as netstack3_core;

use std::num::NonZeroU16;

use assert_matches::assert_matches;
use const_unwrap::const_unwrap_option;
use ip_test_macro::ip_test;
use loom::sync::Arc;
use net_declare::{net_ip_v4, net_ip_v6, net_mac, net_subnet_v4, net_subnet_v6};
use net_types::{
    ethernet::Mac,
    ip::{Ip, Ipv4, Ipv6, Subnet},
    SpecifiedAddr, UnicastAddr, Witness as _, ZonedAddr,
};
use netstack3_core::{
    device::{
        ndp::testutil::{neighbor_advertisement_ip_packet, neighbor_solicitation_ip_packet},
        socket::{Protocol, TargetDevice},
    },
    ip::types::{AddableEntry, AddableMetric, RawMetric},
    sync::Mutex,
    testutil::{FakeCtx, FakeEventDispatcherBuilder},
};
use packet::{Buf, InnerPacketBuilder as _, ParseBuffer as _, Serializer as _};
use packet_formats::{
    arp::{ArpOp, ArpPacketBuilder},
    ethernet::{
        EtherType, EthernetFrameBuilder, EthernetFrameLengthCheck, ETHERNET_MIN_BODY_LEN_NO_TAG,
    },
    ip::IpProto,
    testutil::parse_ip_packet_in_ethernet_frame,
    udp::{UdpPacket, UdpParseArgs},
};

#[test]
fn packet_socket_change_device_and_protocol_atomic() {
    const DEVICE_MAC: Mac = net_mac!("22:33:44:55:66:77");
    const SRC_MAC: Mac = net_mac!("88:88:88:88:88:88");

    let make_ethernet_frame = |ethertype| {
        Buf::new(vec![1; 10], ..)
            .encapsulate(EthernetFrameBuilder::new(SRC_MAC, DEVICE_MAC, ethertype, 0))
            .serialize_vec_outer()
            .unwrap()
            .into_inner()
    };
    let first_proto: NonZeroU16 = NonZeroU16::new(EtherType::Ipv4.into()).unwrap();
    let second_proto: NonZeroU16 = NonZeroU16::new(EtherType::Ipv6.into()).unwrap();

    loom::model(move || {
        let mut builder = FakeEventDispatcherBuilder::default();
        let dev_indexes =
            [(); 2].map(|()| builder.add_device(UnicastAddr::new(DEVICE_MAC).unwrap()));
        let (FakeCtx { sync_ctx, non_sync_ctx }, indexes_to_device_ids) = builder.build();

        let sync_ctx = Arc::new(sync_ctx);
        let devs = dev_indexes.map(|i| indexes_to_device_ids[i].clone());
        drop(indexes_to_device_ids);

        let socket = netstack3_core::device::socket::create(&*sync_ctx, Mutex::new(Vec::new()));
        netstack3_core::device::socket::set_device_and_protocol(
            &*sync_ctx,
            &socket,
            TargetDevice::SpecificDevice(&devs[0].clone().into()),
            Protocol::Specific(first_proto),
        );

        let thread_vars = (sync_ctx.clone(), non_sync_ctx.clone(), devs.clone());
        let deliver = loom::thread::spawn(move || {
            let (sync_ctx, mut non_sync_ctx, devs) = thread_vars;
            for (device, ethertype) in [
                (&devs[0], first_proto.get().into()),
                (&devs[0], second_proto.get().into()),
                (&devs[1], first_proto.get().into()),
                (&devs[1], second_proto.get().into()),
            ] {
                netstack3_core::device::receive_frame(
                    &*sync_ctx,
                    &mut non_sync_ctx,
                    &device,
                    make_ethernet_frame(ethertype),
                );
            }
        });

        let thread_vars = (sync_ctx.clone(), devs[1].clone(), socket.clone());

        let change_device = loom::thread::spawn(move || {
            let (sync_ctx, dev, socket) = thread_vars;
            netstack3_core::device::socket::set_device_and_protocol(
                &*sync_ctx,
                &socket,
                TargetDevice::SpecificDevice(&dev.clone().into()),
                Protocol::Specific(second_proto),
            );
        });

        deliver.join().unwrap();
        change_device.join().unwrap();

        // These are all the matching frames. Depending on how the threads
        // interleave, one or both of them should be delivered.
        let matched_frames = [
            (
                devs[0].downgrade().into(),
                make_ethernet_frame(first_proto.get().into()).into_inner(),
            ),
            (
                devs[1].downgrade().into(),
                make_ethernet_frame(second_proto.get().into()).into_inner(),
            ),
        ];
        let received_frames = socket.socket_state().lock();
        match &received_frames[..] {
            [one] => {
                assert!(one == &matched_frames[0] || one == &matched_frames[1]);
            }
            [one, two] => {
                assert_eq!(one, &matched_frames[0]);
                assert_eq!(two, &matched_frames[1]);
            }
            other => panic!("unexpected received frames {other:?}"),
        }
    });
}

const DEVICE_MAC: Mac = net_mac!("22:33:44:55:66:77");
const NEIGHBOR_MAC: Mac = net_mac!("88:88:88:88:88:88");

trait TestIpExt: netstack3_core::ip::IpExt {
    const DEVICE_ADDR: Self::Addr;
    const DEVICE_SUBNET: Subnet<Self::Addr>;
    const DEVICE_GATEWAY: Subnet<Self::Addr>;
    const NEIGHBOR_ADDR: Self::Addr;

    fn make_neighbor_solicitation() -> Buf<Vec<u8>>;
    fn make_neighbor_confirmation() -> Buf<Vec<u8>>;
}

impl TestIpExt for Ipv4 {
    const DEVICE_ADDR: Self::Addr = net_ip_v4!("192.0.2.0");
    const DEVICE_SUBNET: Subnet<Self::Addr> = net_subnet_v4!("192.0.2.0/32");
    const DEVICE_GATEWAY: Subnet<Self::Addr> = net_subnet_v4!("192.0.2.0/24");
    const NEIGHBOR_ADDR: Self::Addr = net_ip_v4!("192.0.2.1");

    fn make_neighbor_solicitation() -> Buf<Vec<u8>> {
        ArpPacketBuilder::new(
            ArpOp::Request,
            DEVICE_MAC,
            Self::DEVICE_ADDR,
            Mac::BROADCAST,
            Self::NEIGHBOR_ADDR,
        )
        .into_serializer()
        .encapsulate(EthernetFrameBuilder::new(DEVICE_MAC, Mac::BROADCAST, EtherType::Arp, 0))
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b()
    }

    fn make_neighbor_confirmation() -> Buf<Vec<u8>> {
        ArpPacketBuilder::new(
            ArpOp::Response,
            NEIGHBOR_MAC,
            Self::NEIGHBOR_ADDR,
            DEVICE_MAC,
            Self::DEVICE_ADDR,
        )
        .into_serializer()
        .encapsulate(EthernetFrameBuilder::new(
            NEIGHBOR_MAC,
            DEVICE_MAC,
            EtherType::Arp,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b()
    }
}

impl TestIpExt for Ipv6 {
    const DEVICE_ADDR: Self::Addr = net_ip_v6!("2001:db8::1");
    const DEVICE_SUBNET: Subnet<Self::Addr> = net_subnet_v6!("2001:db8::1/128");
    const DEVICE_GATEWAY: Subnet<Self::Addr> = net_subnet_v6!("2001:db8::/64");
    const NEIGHBOR_ADDR: Self::Addr = net_ip_v6!("2001:db8::2");

    fn make_neighbor_solicitation() -> Buf<Vec<u8>> {
        let snmc = Self::NEIGHBOR_ADDR.to_solicited_node_address();
        neighbor_solicitation_ip_packet(
            Self::DEVICE_ADDR,
            snmc.get(),
            Self::NEIGHBOR_ADDR,
            DEVICE_MAC,
        )
        .encapsulate(EthernetFrameBuilder::new(
            DEVICE_MAC,
            Mac::from(&snmc),
            EtherType::Ipv6,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b()
    }

    fn make_neighbor_confirmation() -> Buf<Vec<u8>> {
        neighbor_advertisement_ip_packet(
            Self::NEIGHBOR_ADDR,
            Self::DEVICE_ADDR,
            false, /* router_flag */
            true,  /* solicited_flag */
            false, /* override_flag */
            NEIGHBOR_MAC,
        )
        .encapsulate(EthernetFrameBuilder::new(
            NEIGHBOR_MAC,
            DEVICE_MAC,
            EtherType::Ipv6,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b()
    }
}

#[ip_test]
fn neighbor_resolution_and_send_queued_packets_atomic<I: Ip + TestIpExt>() {
    // Per the loom docs [1], it can take a significant amount of time to
    // exhaustively check complex models. Rather than running a completely
    // exhaustive check, you can configure loom to skip executions that it deems
    // unlikely to catch more issues, by setting a "thread pre-emption bound".
    // In practice, this bound can be set quite low (2 or 3) while still
    // allowing loom to catch most bugs.
    //
    // When writing this regression test, we verified that it reproduces the
    // race condition even with a pre-emption bound of 3. On the other hand,
    // when the pre-emption bound is left unset, the IPv6 variant of this test
    // takes over 2 minutes to run when built in release mode. So we set this
    // pre-emption bound to keep the runtime within a reasonable limit while
    // still catching most possible race conditions.
    //
    // [1]: https://docs.rs/loom/0.6.0/loom/index.html#large-models
    let mut model = loom::model::Builder::new();
    model.preemption_bound = Some(3);

    model.check(move || {
        let mut builder = FakeEventDispatcherBuilder::default();
        let dev_index = builder.add_device_with_ip(
            UnicastAddr::new(DEVICE_MAC).unwrap(),
            I::DEVICE_ADDR,
            I::DEVICE_SUBNET,
        );
        let (FakeCtx { sync_ctx, mut non_sync_ctx }, indexes_to_device_ids) = builder.build();
        let device = indexes_to_device_ids.into_iter().nth(dev_index).unwrap();

        netstack3_core::add_route(
            &sync_ctx,
            &mut non_sync_ctx,
            AddableEntry::with_gateway(
                I::DEVICE_GATEWAY,
                Some(device.clone().into()),
                SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            )
            .into(),
        )
        .unwrap();

        const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(22222_));
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(33333_));

        // Bind a UDP socket to the device we added so we can trigger link
        // resolution by sending IP packets.
        let socket = netstack3_core::transport::udp::create_udp::<I, _>(&sync_ctx);
        netstack3_core::transport::udp::listen_udp(
            &sync_ctx,
            &mut non_sync_ctx,
            &socket,
            None,
            Some(LOCAL_PORT),
        )
        .unwrap();
        netstack3_core::transport::udp::set_udp_device(
            &sync_ctx,
            &mut non_sync_ctx,
            &socket,
            Some(&device.clone().into()),
        )
        .unwrap();

        // Trigger creation of an INCOMPLETE neighbor entry by queueing an
        // outgoing packet to that neighbor's IP address.
        netstack3_core::transport::udp::send_udp_to(
            &sync_ctx,
            &mut non_sync_ctx,
            &socket,
            ZonedAddr::Unzoned(SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap()),
            REMOTE_PORT,
            Buf::new([1], ..),
        )
        .unwrap();

        // Expect the netstack to send a neighbor probe to resolve the link
        // address of the neighbor.
        let frames = non_sync_ctx.take_frames();
        let (sent_device, frame) = assert_matches!(&frames[..], [frame] => frame);
        assert_eq!(sent_device, &device.downgrade());
        assert_eq!(frame, &I::make_neighbor_solicitation().into_inner());

        let sync_ctx = Arc::new(sync_ctx);

        // Race the following:
        //  - Receipt of a neighbor confirmation for the INCOMPLETE neighbor
        //    entry, which causes it to move into COMPLETE.
        //  - Queueing of another packet to that neighbor.

        let thread_vars = (sync_ctx.clone(), non_sync_ctx.clone(), device.clone());
        let resolve_neighbor = loom::thread::spawn(move || {
            let (sync_ctx, mut non_sync_ctx, device) = thread_vars;

            netstack3_core::device::receive_frame(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                I::make_neighbor_confirmation(),
            );
        });

        let thread_vars = (sync_ctx.clone(), non_sync_ctx.clone(), socket.clone());
        let queue_packet = loom::thread::spawn(move || {
            let (sync_ctx, mut non_sync_ctx, socket) = thread_vars;

            netstack3_core::transport::udp::send_udp_to(
                &sync_ctx,
                &mut non_sync_ctx,
                &socket,
                ZonedAddr::Unzoned(SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap()),
                REMOTE_PORT,
                Buf::new([2], ..),
            )
            .unwrap();
        });

        resolve_neighbor.join().unwrap();
        queue_packet.join().unwrap();

        for (i, (sent_device, frame)) in non_sync_ctx.take_frames().into_iter().enumerate() {
            assert_eq!(device, sent_device);

            let (mut body, src_mac, dst_mac, src_ip, dst_ip, proto, ttl) =
                parse_ip_packet_in_ethernet_frame::<I>(&frame, EthernetFrameLengthCheck::NoCheck)
                    .unwrap();
            assert_eq!(src_mac, DEVICE_MAC);
            assert_eq!(dst_mac, NEIGHBOR_MAC);
            assert_eq!(src_ip, I::DEVICE_ADDR);
            assert_eq!(dst_ip, I::NEIGHBOR_ADDR);
            assert_eq!(proto, IpProto::Udp.into());
            assert_eq!(ttl, 64);

            let udp_packet =
                body.parse_with::<_, UdpPacket<_>>(UdpParseArgs::new(src_ip, dst_ip)).unwrap();
            let body = udp_packet.body();
            let expected_payload = i as u8 + 1;
            assert_eq!(body, [expected_payload], "frame was sent out of order!");
        }

        // Remove the device so that existing NUD timers get cleaned up;
        // otherwise, they would hold dangling references to the device when the
        // sync context is dropped at the end of the test.
        netstack3_core::device::remove_ethernet_device(&sync_ctx, &mut non_sync_ctx, device);
    })
}
