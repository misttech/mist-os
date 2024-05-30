// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::vec::Vec;
use assert_matches::assert_matches;
use ip_test_macro::ip_test;

use net_types::{
    ethernet::Mac,
    ip::{AddrSubnet, Ip, Ipv4, Ipv6, Mtu},
    SpecifiedAddr,
};
use packet::{Buf, ParseBuffer as _};
use packet_formats::ethernet::{EthernetFrame, EthernetFrameLengthCheck};

use crate::{
    device::{
        loopback::{self, LoopbackCreationProperties, LoopbackDevice, LoopbackRxQueueMeta},
        queue::ReceiveQueueContext,
    },
    error::NotFoundError,
    ip::{self, device::IpAddressId as _},
    testutil::{
        CtxPairExt as _, FakeBindingsCtx, FakeCtx, TestAddrs, TestIpExt, DEFAULT_INTERFACE_METRIC,
    },
    IpExt,
};

const MTU: Mtu = Mtu::new(66);

#[test]
fn loopback_mtu() {
    let mut ctx = FakeCtx::default();
    let device = ctx
        .core_api()
        .device::<LoopbackDevice>()
        .add_device_with_default_state(
            LoopbackCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);

    assert_eq!(ip::IpDeviceContext::<Ipv4, _>::get_mtu(&mut ctx.core_ctx(), &device), MTU);
    assert_eq!(ip::IpDeviceContext::<Ipv6, _>::get_mtu(&mut ctx.core_ctx(), &device), MTU);
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
#[ip_test]
fn test_loopback_add_remove_addrs<I: Ip + TestIpExt + IpExt>() {
    let mut ctx = FakeCtx::default();
    let device = ctx
        .core_api()
        .device::<LoopbackDevice>()
        .add_device_with_default_state(
            LoopbackCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);

    let get_addrs = |ctx: &mut FakeCtx| {
        ip::device::IpDeviceStateContext::<I, _>::with_address_ids(
            &mut ctx.core_ctx(),
            &device,
            |addrs, _core_ctx| addrs.map(|a| SpecifiedAddr::from(a.addr())).collect::<Vec<_>>(),
        )
    };

    let TestAddrs { subnet, local_ip, local_mac: _, remote_ip: _, remote_mac: _ } = I::TEST_ADDRS;
    let addr_sub =
        AddrSubnet::from_witness(local_ip, subnet.prefix()).expect("error creating AddrSubnet");

    assert_eq!(get_addrs(&mut ctx), []);

    assert_eq!(ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, addr_sub), Ok(()));
    let addr = addr_sub.addr();
    assert_eq!(&get_addrs(&mut ctx)[..], [addr]);

    assert_eq!(
        ctx.core_api().device_ip::<I>().del_ip_addr(&device, addr).unwrap().into_removed(),
        addr_sub
    );
    assert_eq!(get_addrs(&mut ctx), []);

    assert_matches!(ctx.core_api().device_ip::<I>().del_ip_addr(&device, addr), Err(NotFoundError));
}

#[ip_test]
fn loopback_sends_ethernet<I: Ip + TestIpExt + IpExt>() {
    let mut ctx = FakeCtx::default();
    let device = ctx.core_api().device::<LoopbackDevice>().add_device_with_default_state(
        LoopbackCreationProperties { mtu: MTU },
        DEFAULT_INTERFACE_METRIC,
    );
    ctx.test_api().enable_device(&device.clone().into());
    let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;

    let local_addr = I::TEST_ADDRS.local_ip;
    const BODY: &[u8] = b"IP body".as_slice();

    let body = Buf::new(Vec::from(BODY), ..);
    loopback::send_ip_frame(&mut core_ctx.context(), bindings_ctx, &device, local_addr, body)
        .expect("can send");

    // There is no transmit queue so the frames will immediately go into the
    // receive queue.
    let mut frames = ReceiveQueueContext::<LoopbackDevice, _>::with_receive_queue_mut(
        &mut core_ctx.context(),
        &device,
        |queue_state| {
            queue_state.take_frames().map(|(LoopbackRxQueueMeta, frame)| frame).collect::<Vec<_>>()
        },
    );

    let frame = assert_matches!(frames.as_mut_slice(), [frame] => frame);

    let eth = frame
        .parse_with::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::NoCheck)
        .expect("is ethernet");
    assert_eq!(eth.src_mac(), Mac::UNSPECIFIED);
    assert_eq!(eth.dst_mac(), Mac::UNSPECIFIED);
    assert_eq!(eth.ethertype(), Some(I::ETHER_TYPE));

    // Trim the body to account for ethernet padding.
    assert_eq!(&frame.as_ref()[..BODY.len()], BODY);

    // Clear all device references.
    ctx.bindings_ctx.state_mut().rx_available.clear();
    ctx.core_api().device().remove_device(device).into_removed();
}
