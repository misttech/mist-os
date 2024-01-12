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
    device_socket::{Protocol, TargetDevice},
    routes::{AddableEntry, AddableMetric, RawMetric},
    sync::Mutex,
    testutil::{
        ndp::{neighbor_advertisement_ip_packet, neighbor_solicitation_ip_packet},
        ContextPair, FakeBindingsCtx, FakeCtx, FakeEventDispatcherBuilder,
    },
    CoreApi,
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
        let (FakeCtx { core_ctx, bindings_ctx }, indexes_to_device_ids) = builder.build();
        let mut ctx = ContextPair { core_ctx: Arc::new(core_ctx), bindings_ctx };

        let devs = dev_indexes.map(|i| indexes_to_device_ids[i].clone());
        drop(indexes_to_device_ids);

        let socket = ctx.core_api().device_socket().create(Mutex::new(Vec::new()));
        ctx.core_api().device_socket().set_device_and_protocol(
            &socket,
            TargetDevice::SpecificDevice(&devs[0].clone().into()),
            Protocol::Specific(first_proto),
        );

        let thread_vars = (ctx.clone(), devs.clone());
        let deliver = loom::thread::spawn(move || {
            let (ContextPair { core_ctx, mut bindings_ctx }, devs) = thread_vars;
            for (device, ethertype) in [
                (&devs[0], first_proto.get().into()),
                (&devs[0], second_proto.get().into()),
                (&devs[1], first_proto.get().into()),
                (&devs[1], second_proto.get().into()),
            ] {
                netstack3_core::device::receive_frame(
                    &*core_ctx,
                    &mut bindings_ctx,
                    &device,
                    make_ethernet_frame(ethertype),
                );
            }
        });

        let thread_vars = (ctx.clone(), devs[1].clone(), socket.clone());

        let change_device = loom::thread::spawn(move || {
            let (mut ctx, dev, socket) = thread_vars;
            ctx.core_api().device_socket().set_device_and_protocol(
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

trait TestIpExt: netstack3_core::IpExt {
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
#[netstack3_core::context_ip_bounds(I, FakeBindingsCtx)]
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
        let (FakeCtx { core_ctx, mut bindings_ctx }, indexes_to_device_ids) = builder.build();
        let device = indexes_to_device_ids.into_iter().nth(dev_index).unwrap();

        netstack3_core::testutil::add_route(
            &core_ctx,
            &mut bindings_ctx,
            AddableEntry::with_gateway(
                I::DEVICE_GATEWAY,
                device.clone().into(),
                SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            )
            .into(),
        )
        .unwrap();

        const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(22222));
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(33333));

        // Bind a UDP socket to the device we added so we can trigger link
        // resolution by sending IP packets.
        let mut udp_api = CoreApi::with_contexts(&core_ctx, &mut bindings_ctx).udp::<I>();
        let socket = udp_api.create();
        udp_api
            .listen(
                &socket,
                SpecifiedAddr::new(I::DEVICE_ADDR).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            )
            .unwrap();
        udp_api.set_device(&socket, Some(&device.clone().into())).unwrap();

        // Trigger creation of an INCOMPLETE neighbor entry by queueing an
        // outgoing packet to that neighbor's IP address.
        udp_api
            .send_to(
                &socket,
                Some(ZonedAddr::Unzoned(SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap()).into()),
                REMOTE_PORT.into(),
                Buf::new([1], ..),
            )
            .unwrap();

        // Expect the netstack to send a neighbor probe to resolve the link
        // address of the neighbor.
        let frames = bindings_ctx.take_frames();
        let (sent_device, frame) = assert_matches!(&frames[..], [frame] => frame);
        assert_eq!(sent_device, &device.downgrade());
        assert_eq!(frame, &I::make_neighbor_solicitation().into_inner());

        let core_ctx = Arc::new(core_ctx);

        // Race the following:
        //  - Receipt of a neighbor confirmation for the INCOMPLETE neighbor
        //    entry, which causes it to move into COMPLETE.
        //  - Queueing of another packet to that neighbor.

        let thread_vars = (core_ctx.clone(), bindings_ctx.clone(), device.clone());
        let resolve_neighbor = loom::thread::spawn(move || {
            let (core_ctx, mut bindings_ctx, device) = thread_vars;

            netstack3_core::device::receive_frame(
                &core_ctx,
                &mut bindings_ctx,
                &device,
                I::make_neighbor_confirmation(),
            );
        });

        let thread_vars = (core_ctx.clone(), bindings_ctx.clone(), socket.clone());
        let queue_packet = loom::thread::spawn(move || {
            let (core_ctx, mut bindings_ctx, socket) = thread_vars;
            CoreApi::with_contexts(&core_ctx, &mut bindings_ctx)
                .udp()
                .send_to(
                    &socket,
                    Some(ZonedAddr::Unzoned(SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap()).into()),
                    REMOTE_PORT.into(),
                    Buf::new([2], ..),
                )
                .unwrap();
        });

        resolve_neighbor.join().unwrap();
        queue_packet.join().unwrap();

        for (i, (sent_device, frame)) in bindings_ctx.take_frames().into_iter().enumerate() {
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
        // core context is dropped at the end of the test.
        netstack3_core::testutil::clear_routes_and_remove_ethernet_device(
            &core_ctx,
            &mut bindings_ctx,
            device,
        );
    })
}

#[ip_test]
#[netstack3_core::context_ip_bounds(I, FakeBindingsCtx)]
fn new_incomplete_neighbor_schedule_timer_atomic<I: Ip + TestIpExt>() {
    loom::model(move || {
        let mut builder = FakeEventDispatcherBuilder::default();
        let dev_index = builder.add_device_with_ip(
            UnicastAddr::new(DEVICE_MAC).unwrap(),
            I::DEVICE_ADDR,
            I::DEVICE_SUBNET,
        );
        let (FakeCtx { core_ctx, mut bindings_ctx }, indexes_to_device_ids) = builder.build();
        let device = indexes_to_device_ids.into_iter().nth(dev_index).unwrap();

        netstack3_core::testutil::add_route(
            &core_ctx,
            &mut bindings_ctx,
            AddableEntry::with_gateway(
                I::DEVICE_GATEWAY,
                device.clone().into(),
                SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            )
            .into(),
        )
        .unwrap();

        const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(22222));
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(33333));

        // Bind a UDP socket to the device we added so we can trigger link
        // resolution by sending IP packets.
        let mut udp_api = CoreApi::with_contexts(&core_ctx, &mut bindings_ctx).udp::<I>();
        let socket = udp_api.create();
        udp_api
            .listen(
                &socket,
                SpecifiedAddr::new(I::DEVICE_ADDR).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            )
            .unwrap();
        udp_api.set_device(&socket, Some(&device.clone().into())).unwrap();

        let core_ctx = Arc::new(core_ctx);

        // Race the following:
        //  - Creation of an INCOMPLETE neighbor entry, caused by queueing an
        //    outgoing packet to that neighbor's IP address.
        //  - Adding a static entry for that neighbor, which will cancel the
        //    entry's existing timer.
        //
        // If the entry is added as INCOMPLETE, but its timer is not scheduled
        // atomically with entry insertion, the subsequent timer cancelation
        // will cause a panic.

        let thread_vars = (core_ctx.clone(), bindings_ctx.clone(), socket.clone());
        let create_incomplete_neighbor = loom::thread::spawn(move || {
            let (core_ctx, mut bindings_ctx, socket) = thread_vars;
            CoreApi::with_contexts(&core_ctx, &mut bindings_ctx)
                .udp()
                .send_to(
                    &socket,
                    Some(ZonedAddr::Unzoned(SpecifiedAddr::new(I::NEIGHBOR_ADDR).unwrap()).into()),
                    REMOTE_PORT.into(),
                    Buf::new([1], ..),
                )
                .unwrap()
        });

        let thread_vars = (core_ctx.clone(), bindings_ctx.clone(), device.clone());
        let set_static_neighbor = loom::thread::spawn(move || {
            let (core_ctx, mut bindings_ctx, device) = thread_vars;

            netstack3_core::device::insert_static_neighbor_entry::<I, _>(
                &core_ctx,
                &mut bindings_ctx,
                &device.into(),
                I::NEIGHBOR_ADDR,
                NEIGHBOR_MAC,
            )
            .unwrap();
        });

        create_incomplete_neighbor.join().unwrap();
        set_static_neighbor.join().unwrap();

        // Remove the device so that existing references to it get cleaned up
        // before the core context is dropped at the end of the test.
        netstack3_core::testutil::clear_routes_and_remove_ethernet_device(
            &core_ctx,
            &mut bindings_ctx,
            device,
        );
    })
}
