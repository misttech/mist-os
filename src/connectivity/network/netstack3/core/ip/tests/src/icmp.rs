// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Debug;
use core::num::NonZeroU16;

use assert_matches::assert_matches;
use net_types::ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use net_types::{SpecifiedAddr, Witness};
use packet::{Buf, Serializer};
use packet_formats::ethernet::EthernetFrameLengthCheck;
use packet_formats::icmp::{
    IcmpDestUnreachable, IcmpEchoRequest, IcmpMessage, IcmpPacket, IcmpPacketBuilder,
    IcmpTimeExceeded, IcmpUnusedCode, Icmpv4DestUnreachableCode, Icmpv4TimeExceededCode,
    Icmpv4TimestampRequest, Icmpv6DestUnreachableCode, Icmpv6TimeExceededCode, MessageBody,
    OriginalPacket,
};
use packet_formats::ip::{FragmentOffset, IpPacketBuilder as _, IpProto, Ipv4Proto, Ipv6Proto};
use packet_formats::testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame;
use packet_formats::udp::UdpPacketBuilder;

use netstack3_base::testutil::{set_logger_for_test, TestIpExt, TEST_ADDRS_V4, TEST_ADDRS_V6};
use netstack3_base::FrameDestination;
use netstack3_core::device::DeviceId;
use netstack3_core::testutil::{Ctx, CtxPairExt as _, FakeBindingsCtx, FakeCtxBuilder};
use netstack3_core::{IpExt, StackStateBuilder};
use netstack3_ip::icmp::Icmpv4StateBuilder;

/// Test that receiving a particular IP packet results in a particular ICMP
/// response.
///
/// Test that receiving an IP packet from remote host
/// `I::TEST_ADDRS.remote_ip` to host `dst_ip` with `ttl` and `proto`
/// results in all of the counters in `assert_counters` being triggered at
/// least once.
///
/// If `expect_message_code` is `Some`, expect that exactly one ICMP packet
/// was sent in response with the given message and code, and invoke the
/// given function `f` on the packet. Otherwise, if it is `None`, expect
/// that no response was sent.
///
/// `modify_packet_builder` is invoked on the `PacketBuilder` before the
/// packet is serialized.
///
/// `modify_stack_state_builder` is invoked on the `StackStateBuilder`
/// before it is used to build the context.
///
/// The state is initialized to `I::TEST_ADDRS` when testing.
#[allow(clippy::too_many_arguments)]
#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
fn test_receive_ip_packet<
    I: TestIpExt + IpExt,
    C: PartialEq + Debug,
    M: IcmpMessage<I, Code = C> + PartialEq + Debug,
    PBF: FnOnce(&mut <I as packet_formats::ip::IpExt>::PacketBuilder),
    SSBF: FnOnce(&mut StackStateBuilder),
    F: for<'a> FnOnce(&IcmpPacket<I, &'a [u8], M>),
>(
    modify_packet_builder: PBF,
    modify_stack_state_builder: SSBF,
    body: &mut [u8],
    dst_ip: SpecifiedAddr<I::Addr>,
    ttl: u8,
    proto: I::Proto,
    assert_counters: &[&str],
    expect_message_code: Option<(M, C)>,
    f: F,
) {
    set_logger_for_test();
    let mut pb = <I as packet_formats::ip::IpExt>::PacketBuilder::new(
        *I::TEST_ADDRS.remote_ip,
        dst_ip.get(),
        ttl,
        proto,
    );
    modify_packet_builder(&mut pb);
    let buffer = Buf::new(body, ..).encapsulate(pb).serialize_vec_outer().unwrap();

    let (mut ctx, device_ids) = FakeCtxBuilder::with_addrs(I::TEST_ADDRS)
        .build_with_modifications(modify_stack_state_builder);

    let device: DeviceId<_> = device_ids[0].clone().into();
    ctx.test_api().set_unicast_forwarding_enabled::<I>(&device, true);
    ctx.test_api().receive_ip_packet::<I, _>(
        &device,
        Some(FrameDestination::Individual { local: true }),
        buffer,
    );

    let Ctx { core_ctx, bindings_ctx } = &mut ctx;
    for counter in assert_counters {
        // TODO(https://fxbug.dev/42084333): Redesign iterating through
        // assert_counters once CounterContext is removed.
        let count = match *counter {
            "send_ipv4_packet" => core_ctx.ipv4().inner.counters().send_ip_packet.get(),
            "send_ipv6_packet" => core_ctx.ipv6().inner.counters().send_ip_packet.get(),
            "echo_request" => core_ctx.common_icmp::<I>().rx_counters.echo_request.get(),
            "timestamp_request" => core_ctx.common_icmp::<I>().rx_counters.timestamp_request.get(),
            "protocol_unreachable" => {
                core_ctx.common_icmp::<I>().tx_counters.protocol_unreachable.get()
            }
            "port_unreachable" => core_ctx.common_icmp::<I>().tx_counters.port_unreachable.get(),
            "net_unreachable" => core_ctx.common_icmp::<I>().tx_counters.net_unreachable.get(),
            "ttl_expired" => core_ctx.common_icmp::<I>().tx_counters.ttl_expired.get(),
            "packet_too_big" => core_ctx.common_icmp::<I>().tx_counters.packet_too_big.get(),
            "parameter_problem" => core_ctx.common_icmp::<I>().tx_counters.parameter_problem.get(),
            "dest_unreachable" => core_ctx.common_icmp::<I>().tx_counters.dest_unreachable.get(),
            "error" => core_ctx.common_icmp::<I>().tx_counters.error.get(),
            c => panic!("unrecognized counter: {c}"),
        };
        assert!(count > 0, "counter at zero: {counter}");
    }

    if let Some((expect_message, expect_code)) = expect_message_code {
        let frames = bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        assert_eq!(frames.len(), 1);
        let (src_mac, dst_mac, src_ip, dst_ip, _, message, code) =
            parse_icmp_packet_in_ip_packet_in_ethernet_frame::<I, _, M, _>(
                &frame,
                EthernetFrameLengthCheck::NoCheck,
                f,
            )
            .unwrap();

        assert_eq!(src_mac, I::TEST_ADDRS.local_mac.get());
        assert_eq!(dst_mac, I::TEST_ADDRS.remote_mac.get());
        assert_eq!(src_ip, I::TEST_ADDRS.local_ip.get());
        assert_eq!(dst_ip, I::TEST_ADDRS.remote_ip.get());
        assert_eq!(message, expect_message);
        assert_eq!(code, expect_code);
    } else {
        assert_matches!(bindings_ctx.take_ethernet_frames()[..], []);
    }
}

#[test]
fn test_receive_echo() {
    set_logger_for_test();

    // Test that, when receiving an echo request, we respond with an echo
    // reply with the appropriate parameters.

    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
    fn test<I: TestIpExt + IpExt>(assert_counters: &[&str]) {
        let req = IcmpEchoRequest::new(0, 0);
        let req_body = &[1, 2, 3, 4];
        let mut buffer = Buf::new(req_body.to_vec(), ..)
            .encapsulate(IcmpPacketBuilder::<I, _>::new(
                I::TEST_ADDRS.remote_ip.get(),
                I::TEST_ADDRS.local_ip.get(),
                IcmpUnusedCode,
                req,
            ))
            .serialize_vec_outer()
            .unwrap();
        test_receive_ip_packet::<I, _, _, _, _, _>(
            |_| {},
            |_| {},
            buffer.as_mut(),
            I::TEST_ADDRS.local_ip,
            64,
            I::ICMP_IP_PROTO,
            assert_counters,
            Some((req.reply(), IcmpUnusedCode)),
            |packet| {
                let (inner_header, inner_body) = packet.original_packet().bytes();
                assert!(inner_body.is_none());
                assert_eq!(inner_header, req_body)
            },
        );
    }

    test::<Ipv4>(&["echo_request", "send_ipv4_packet"]);
    test::<Ipv6>(&["echo_request", "send_ipv6_packet"]);
}

#[test]
fn test_receive_timestamp() {
    set_logger_for_test();

    let req = Icmpv4TimestampRequest::new(1, 2, 3);
    let mut buffer = Buf::new(Vec::new(), ..)
        .encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
            TEST_ADDRS_V4.remote_ip,
            TEST_ADDRS_V4.local_ip,
            IcmpUnusedCode,
            req,
        ))
        .serialize_vec_outer()
        .unwrap();
    test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
        |_| {},
        |builder| {
            let _: &mut Icmpv4StateBuilder =
                builder.ipv4_builder().icmpv4_builder().send_timestamp_reply(true);
        },
        buffer.as_mut(),
        TEST_ADDRS_V4.local_ip,
        64,
        Ipv4Proto::Icmp,
        &["timestamp_request", "send_ipv4_packet"],
        Some((req.reply(0x80000000, 0x80000000), IcmpUnusedCode)),
        |_| {},
    );
}

#[test]
fn test_protocol_unreachable() {
    // Test receiving an IP packet for an unreachable protocol. Check to
    // make sure that we respond with the appropriate ICMP message.
    //
    // Currently, for IPv4, we test for all unreachable protocols, while for
    // IPv6, we only test for IGMP and TCP. See the comment below for why
    // that limitation exists. Once the limitation is fixed, we should test
    // with all unreachable protocols for both versions.

    for proto in 0u8..=255 {
        let v4proto = Ipv4Proto::from(proto);
        match v4proto {
            Ipv4Proto::Other(_) | Ipv4Proto::Proto(IpProto::Reserved) => {
                test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
                    |_| {},
                    |_| {},
                    &mut [0u8; 128],
                    TEST_ADDRS_V4.local_ip,
                    64,
                    v4proto,
                    &["protocol_unreachable"],
                    Some((
                        IcmpDestUnreachable::default(),
                        Icmpv4DestUnreachableCode::DestProtocolUnreachable,
                    )),
                    // Ensure packet is truncated to the right length.
                    |packet| assert_eq!(packet.original_packet().len(), 84),
                );
            }
            Ipv4Proto::Icmp
            | Ipv4Proto::Igmp
            | Ipv4Proto::Proto(IpProto::Udp)
            | Ipv4Proto::Proto(IpProto::Tcp) => {}
        }

        // TODO(https://fxbug.dev/42124756): We seem to fail to parse an IPv6 packet if
        // its Next Header value is unrecognized (rather than treating this
        // as a valid parsing but then replying with a parameter problem
        // error message). We should a) fix this and, b) expand this test to
        // ensure we don't regress.
        let v6proto = Ipv6Proto::from(proto);
        match v6proto {
            Ipv6Proto::Icmpv6
            | Ipv6Proto::NoNextHeader
            | Ipv6Proto::Proto(IpProto::Udp)
            | Ipv6Proto::Proto(IpProto::Tcp)
            | Ipv6Proto::Other(_)
            | Ipv6Proto::Proto(IpProto::Reserved) => {}
        }
    }
}

#[test]
fn test_port_unreachable() {
    // TODO(joshlf): Test TCP as well.

    // Receive an IP packet for an unreachable UDP port (1234). Check to
    // make sure that we respond with the appropriate ICMP message. Then, do
    // the same for a stack which has the UDP `send_port_unreachable` option
    // disable, and make sure that we DON'T respond with an ICMP message.

    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
    fn test<I: TestIpExt + IpExt, C: PartialEq + Debug>(
        code: C,
        assert_counters: &[&str],
        original_packet_len: usize,
    ) where
        IcmpDestUnreachable:
            for<'a> IcmpMessage<I, Code = C, Body<&'a [u8]> = OriginalPacket<&'a [u8]>>,
    {
        let mut buffer = Buf::new(vec![0; 128], ..)
            .encapsulate(UdpPacketBuilder::new(
                I::TEST_ADDRS.remote_ip.get(),
                I::TEST_ADDRS.local_ip.get(),
                None,
                NonZeroU16::new(1234).unwrap(),
            ))
            .serialize_vec_outer()
            .unwrap();
        test_receive_ip_packet::<I, _, _, _, _, _>(
            |_| {},
            // Enable the `send_port_unreachable` feature.
            |builder| {
                let _builder: &mut _ =
                    builder.transport_builder().udp_builder().send_port_unreachable(true);
            },
            buffer.as_mut(),
            I::TEST_ADDRS.local_ip,
            64,
            IpProto::Udp.into(),
            assert_counters,
            Some((IcmpDestUnreachable::default(), code)),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().len(), original_packet_len),
        );
        test_receive_ip_packet::<I, C, IcmpDestUnreachable, _, _, _>(
            |_| {},
            // Leave the `send_port_unreachable` feature disabled.
            |_: &mut StackStateBuilder| {},
            buffer.as_mut(),
            I::TEST_ADDRS.local_ip,
            64,
            IpProto::Udp.into(),
            &[],
            None,
            |_| {},
        );
    }

    test::<Ipv4, _>(Icmpv4DestUnreachableCode::DestPortUnreachable, &["port_unreachable"], 84);
    test::<Ipv6, _>(Icmpv6DestUnreachableCode::PortUnreachable, &["port_unreachable"], 176);
}

#[test]
fn test_net_unreachable() {
    // Receive an IP packet for an unreachable destination address. Check to
    // make sure that we respond with the appropriate ICMP message.
    test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
        |_| {},
        |_: &mut StackStateBuilder| {},
        &mut [0u8; 128],
        SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
        64,
        IpProto::Udp.into(),
        &["net_unreachable"],
        Some((IcmpDestUnreachable::default(), Icmpv4DestUnreachableCode::DestNetworkUnreachable)),
        // Ensure packet is truncated to the right length.
        |packet| assert_eq!(packet.original_packet().len(), 84),
    );
    test_receive_ip_packet::<Ipv6, _, _, _, _, _>(
        |_| {},
        |_: &mut StackStateBuilder| {},
        &mut [0u8; 128],
        SpecifiedAddr::new(Ipv6Addr::from_bytes([1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8]))
            .unwrap(),
        64,
        IpProto::Udp.into(),
        &["net_unreachable"],
        Some((IcmpDestUnreachable::default(), Icmpv6DestUnreachableCode::NoRoute)),
        // Ensure packet is truncated to the right length.
        |packet| assert_eq!(packet.original_packet().len(), 168),
    );
    // Same test for IPv4 but with a non-initial fragment. No ICMP error
    // should be sent.
    test_receive_ip_packet::<Ipv4, _, IcmpDestUnreachable, _, _, _>(
        |pb| pb.fragment_offset(FragmentOffset::new(64).unwrap()),
        |_: &mut StackStateBuilder| {},
        &mut [0u8; 128],
        SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
        64,
        IpProto::Udp.into(),
        &[],
        None,
        |_| {},
    );
}

#[test]
fn test_ttl_expired() {
    // Receive an IP packet with an expired TTL. Check to make sure that we
    // respond with the appropriate ICMP message.
    test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
        |_| {},
        |_: &mut StackStateBuilder| {},
        &mut [0u8; 128],
        TEST_ADDRS_V4.remote_ip,
        1,
        IpProto::Udp.into(),
        &["ttl_expired"],
        Some((IcmpTimeExceeded::default(), Icmpv4TimeExceededCode::TtlExpired)),
        // Ensure packet is truncated to the right length.
        |packet| assert_eq!(packet.original_packet().len(), 84),
    );
    test_receive_ip_packet::<Ipv6, _, _, _, _, _>(
        |_| {},
        |_: &mut StackStateBuilder| {},
        &mut [0u8; 128],
        TEST_ADDRS_V6.remote_ip,
        1,
        IpProto::Udp.into(),
        &["ttl_expired"],
        Some((IcmpTimeExceeded::default(), Icmpv6TimeExceededCode::HopLimitExceeded)),
        // Ensure packet is truncated to the right length.
        |packet| assert_eq!(packet.original_packet().len(), 168),
    );
    // Same test for IPv4 but with a non-initial fragment. No ICMP error
    // should be sent.
    test_receive_ip_packet::<Ipv4, _, IcmpTimeExceeded, _, _, _>(
        |pb| pb.fragment_offset(FragmentOffset::new(64).unwrap()),
        |_: &mut StackStateBuilder| {},
        &mut [0u8; 128],
        SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
        64,
        IpProto::Udp.into(),
        &[],
        None,
        |_| {},
    );
}
