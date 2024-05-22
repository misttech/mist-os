// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use const_unwrap::const_unwrap_option;
use core::num::NonZeroU16;

use assert_matches::assert_matches;

use ip_test_macro::ip_test;
use net_types::{
    ip::{Ip, Ipv4, Ipv6},
    ZonedAddr,
};
use packet::Buf;
use test_case::test_case;

use crate::{
    testutil::{set_logger_for_test, CtxPairExt as _, FakeBindingsCtx, FakeCtxBuilder, TestIpExt},
    IpExt,
};

const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(100));

#[ip_test]
#[test_case(true; "bind to device")]
#[test_case(false; "no bind to device")]
#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
fn loopback_bind_to_device<I: Ip + IpExt + TestIpExt>(bind_to_device: bool) {
    set_logger_for_test();
    const HELLO: &'static [u8] = b"Hello";
    let (mut ctx, local_device_ids) = FakeCtxBuilder::with_addrs(I::TEST_ADDRS).build();

    let _loopback_device_id = ctx.test_api().add_loopback();
    let mut api = ctx.core_api().udp::<I>();
    let socket = api.create();
    api.listen(&socket, None, Some(LOCAL_PORT)).unwrap();
    if bind_to_device {
        api.set_device(&socket, Some(&local_device_ids[0].clone().into())).unwrap();
    }
    api.send_to(
        &socket,
        Some(ZonedAddr::Unzoned(I::TEST_ADDRS.local_ip)),
        LOCAL_PORT.into(),
        Buf::new(HELLO.to_vec(), ..),
    )
    .unwrap();

    assert!(ctx.test_api().handle_queued_rx_packets());

    // TODO(https://fxbug.dev/42084713): They should both be non-empty. The
    // socket map should allow a looped back packet to be delivered despite
    // it being bound to a device other than loopback.
    if bind_to_device {
        assert_matches!(&ctx.bindings_ctx.take_udp_received(&socket)[..], []);
    } else {
        assert_matches!(
            &ctx.bindings_ctx.take_udp_received(&socket)[..],
            [packet] => assert_eq!(packet, HELLO)
        );
    }
}
