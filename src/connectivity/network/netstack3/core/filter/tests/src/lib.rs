// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]
#![warn(unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]
// TODO(https://fxbug.dev/339502691): Return to the default limit once lock
// ordering no longer causes overflows.
#![recursion_limit = "256"]

use std::num::NonZeroU16;
use std::sync::Arc;

use const_unwrap::const_unwrap_option;
use ip_test_macro::ip_test;
use net_types::ZonedAddr;
use packet::{Buf, Serializer as _};
use packet_formats::ethernet::{EtherType, EthernetFrameBuilder};
use packet_formats::ip::{IpPacketBuilder as _, IpProto};
use packet_formats::udp::UdpPacketBuilder;

use netstack3_base::testutil::{set_logger_for_test, TestDualStackIpExt, TestIpExt};
use netstack3_base::CtxPair;
use netstack3_core::device::{EthernetLinkDevice, RecvEthernetFrameMeta};
use netstack3_core::filter::{
    Action, Hook, NatRoutines, PacketMatcher, Routine, Routines, Rule, TransportProtocolMatcher,
};
use netstack3_core::testutil::{CtxPairExt as _, FakeBindingsCtx, FakeCtx, FakeCtxBuilder};
use netstack3_core::IpExt;

const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(22222));
const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(44444));

fn make_udp_reply_packet<I: TestIpExt>() -> Buf<Vec<u8>> {
    Buf::new([1], ..)
        .encapsulate(UdpPacketBuilder::new(
            *I::TEST_ADDRS.remote_ip,
            *I::TEST_ADDRS.local_ip,
            Some(REMOTE_PORT),
            LOCAL_PORT,
        ))
        .encapsulate(I::PacketBuilder::new(
            *I::TEST_ADDRS.remote_ip,
            *I::TEST_ADDRS.local_ip,
            u8::MAX, /* ttl */
            IpProto::Udp.into(),
        ))
        .encapsulate(EthernetFrameBuilder::new(
            *I::TEST_ADDRS.remote_mac,
            *I::TEST_ADDRS.local_mac,
            EtherType::from_ip_version(I::VERSION),
            0, /* min_body_len */
        ))
        .serialize_vec_outer()
        .unwrap()
        .unwrap_b()
}

fn masquerade<I: TestIpExt>() -> Routines<I, (), ()> {
    Routines {
        nat: NatRoutines {
            egress: Hook {
                routines: vec![Routine {
                    rules: vec![Rule {
                        matcher: PacketMatcher {
                            transport_protocol: Some(TransportProtocolMatcher {
                                proto: IpProto::Udp.into(),
                                src_port: None,
                                dst_port: None,
                            }),
                            ..Default::default()
                        },
                        action: Action::Masquerade { src_port: None },
                        validation_info: (),
                    }],
                }],
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

#[netstack3_core::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
fn crash<I: TestDualStackIpExt + IpExt>() {
    set_logger_for_test();

    let mut builder = FakeCtxBuilder::default();
    let dev_index = builder.add_device_with_ip(
        I::TEST_ADDRS.local_mac,
        *I::TEST_ADDRS.local_ip,
        I::TEST_ADDRS.subnet,
    );
    let (FakeCtx { core_ctx, bindings_ctx }, indexes_to_device_ids) = builder.build();
    let mut ctx = CtxPair { core_ctx: Arc::new(core_ctx), bindings_ctx };
    let device = indexes_to_device_ids.into_iter().nth(dev_index).unwrap();

    // Send a packet to a neighbor so that this flow is inserted in the connection
    // tracking table. It will not have NAT configured for it because no NAT rules
    // have been installed.
    let mut udp_api = ctx.core_api().udp::<I>();
    let socket = udp_api.create();
    udp_api
        .listen(&socket, Some(ZonedAddr::Unzoned(I::TEST_ADDRS.local_ip)), Some(LOCAL_PORT))
        .unwrap();
    ctx.core_api()
        .udp()
        .send_to(
            &socket,
            Some(ZonedAddr::Unzoned(I::TEST_ADDRS.remote_ip)),
            REMOTE_PORT.into(),
            Buf::new([1], ..),
        )
        .unwrap();

    // Now configure outgoing traffic to be masqueraded. This rule is a no-op (we
    // are already sending from the assigned address of the interface), but it will
    // cause NAT to be performed rather than skipped.
    ctx.core_api().filter().set_filter_state(masquerade(), masquerade()).unwrap();

    // Race two threads each of which receives an identical UDP packet replying to
    // one that was sent on the socket. The flow is already finalized in conntrack,
    // i.e. inserted in the connection tracking table, so both reply packets will
    // obtain a shared reference to the finalized connection.
    //
    // The NAT module will attempt to configure NAT as a no-op for both these
    // packets; if they both expect the state not to be configured when they update
    // it, the one that loses the race will panic.

    let thread_vars = (ctx.clone(), device.clone());
    let reply_packet_one = std::thread::spawn(move || {
        let (mut ctx, device_id) = thread_vars;
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id }, make_udp_reply_packet::<I>());
    });

    let thread_vars = (ctx.clone(), device);
    let reply_packet_two = std::thread::spawn(move || {
        let (mut ctx, device_id) = thread_vars;
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id }, make_udp_reply_packet::<I>());
    });

    reply_packet_one.join().unwrap();
    reply_packet_two.join().unwrap();

    // Remove the packets from the receive queue in bindings so that references to
    // core resources are cleaned up before the core context is dropped at the end
    // of the test.
    let _ = ctx.bindings_ctx.take_udp_received(&socket);
}
