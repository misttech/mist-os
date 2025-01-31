// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::vec::Vec;

use assert_matches::assert_matches;
use ip_test_macro::ip_test;
use net_types::ip::{AddrSubnet, IpAddress as _, IpVersion, Mtu};
use net_types::{Witness, ZonedAddr};
use netstack3_base::testutil::TestIpExt;
use netstack3_core::sync::RemoveResourceResult;
use netstack3_core::testutil::{
    CtxPairExt as _, FakeBindingsCtx, FakeCtx, PureIpDeviceAndIpVersion, DEFAULT_INTERFACE_METRIC,
};
use netstack3_core::types::WorkQueueReport;
use netstack3_core::{IpExt, StackState};
use netstack3_device::pure_ip::{
    self, PureIpDevice, PureIpDeviceCreationProperties, PureIpDeviceReceiveFrameMetadata,
};
use netstack3_device::queue::TransmitQueueConfiguration;
use netstack3_device::DeviceId;
use netstack3_ip::IpPacketDestination;
use packet::{Buf, Serializer as _};
use packet_formats::ip::{IpPacketBuilder, IpProto};
use test_case::test_case;

const MTU: Mtu = Mtu::new(1234);
const TTL: u8 = 64;

fn default_ip_packet<I: TestIpExt>() -> Buf<Vec<u8>> {
    Buf::new(Vec::new(), ..)
        .encapsulate(I::PacketBuilder::new(
            I::TEST_ADDRS.remote_ip.get(),
            I::TEST_ADDRS.local_ip.get(),
            TTL,
            IpProto::Udp.into(),
        ))
        .serialize_vec_outer()
        .ok()
        .unwrap()
        .unwrap_b()
}

#[test]
// Smoke test verifying [`PureIpDevice`] implements the traits required to
// satisfy the [`DeviceApi`].
fn add_remove_pure_ip_device() {
    let mut ctx = FakeCtx::default();
    let mut device_api = ctx.core_api().device::<PureIpDevice>();
    let device = device_api.add_device_with_default_state(
        PureIpDeviceCreationProperties { mtu: MTU },
        DEFAULT_INTERFACE_METRIC,
    );
    assert_matches!(device_api.remove_device(device), RemoveResourceResult::Removed(_));
}

#[test]
// Smoke test verifying [`PureIpDevice`] implements the traits required to
// satisfy the [`TransmitQueueApi`].
fn update_tx_queue_config() {
    let mut ctx = FakeCtx::default();
    let mut device_api = ctx.core_api().device::<PureIpDevice>();
    let device = device_api.add_device_with_default_state(
        PureIpDeviceCreationProperties { mtu: MTU },
        DEFAULT_INTERFACE_METRIC,
    );
    let mut tx_queue_api = ctx.core_api().transmit_queue::<PureIpDevice>();
    tx_queue_api.set_configuration(&device, TransmitQueueConfiguration::Fifo);
}

#[ip_test(I)]
fn receive_frame<I: TestIpExt + IpExt>() {
    let mut ctx = FakeCtx::default();
    let device = ctx.core_api().device::<PureIpDevice>().add_device_with_default_state(
        PureIpDeviceCreationProperties { mtu: MTU },
        DEFAULT_INTERFACE_METRIC,
    );
    ctx.test_api().enable_device(&device.clone().into());

    fn check_frame_counters<I: IpExt>(stack_state: &StackState<FakeBindingsCtx>, count: u64) {
        assert_eq!(stack_state.common_ip::<I>().counters().receive_ip_packet.get(), count);
        assert_eq!(stack_state.device().counters.recv_frame.get(), count);
        match I::VERSION {
            IpVersion::V4 => {
                assert_eq!(stack_state.device().counters.recv_ipv4_delivered.get(), count)
            }
            IpVersion::V6 => {
                assert_eq!(stack_state.device().counters.recv_ipv6_delivered.get(), count)
            }
        }
    }

    // Receive a frame from the network and verify delivery to the IP layer.
    check_frame_counters::<I>(&ctx.core_ctx, 0);
    ctx.core_api().device::<PureIpDevice>().receive_frame(
        PureIpDeviceReceiveFrameMetadata { device_id: device, ip_version: I::VERSION },
        default_ip_packet::<I>(),
    );
    check_frame_counters::<I>(&ctx.core_ctx, 1);
}

#[ip_test(I)]
#[test_case(TransmitQueueConfiguration::None; "no queue")]
#[test_case(TransmitQueueConfiguration::Fifo; "fifo queue")]
fn send_frame<I: TestIpExt + IpExt>(tx_queue_config: TransmitQueueConfiguration) {
    let mut ctx = FakeCtx::default();
    let device = ctx.core_api().device::<PureIpDevice>().add_device_with_default_state(
        PureIpDeviceCreationProperties { mtu: MTU },
        DEFAULT_INTERFACE_METRIC,
    );
    ctx.test_api().enable_device(&device.clone().into());
    let has_tx_queue = match tx_queue_config {
        TransmitQueueConfiguration::None => false,
        TransmitQueueConfiguration::Fifo => true,
    };
    ctx.core_api().transmit_queue::<PureIpDevice>().set_configuration(&device, tx_queue_config);

    fn check_frame_counters<I: IpExt>(stack_state: &StackState<FakeBindingsCtx>, count: u64) {
        assert_eq!(stack_state.device().counters.send_total_frames.get(), count);
        assert_eq!(stack_state.device().counters.send_frame.get(), count);
        match I::VERSION {
            IpVersion::V4 => {
                assert_eq!(stack_state.device().counters.send_ipv4_frame.get(), count)
            }
            IpVersion::V6 => {
                assert_eq!(stack_state.device().counters.send_ipv6_frame.get(), count)
            }
        }
    }

    assert_matches!(ctx.bindings_ctx.take_ip_frames()[..], [], "unexpected sent IP frame");
    check_frame_counters::<I>(&ctx.core_ctx, 0);

    {
        let (mut core_ctx, bindings_ctx) = ctx.contexts();
        pure_ip::send_ip_frame(
            &mut core_ctx,
            bindings_ctx,
            &device,
            IpPacketDestination::<I, _>::from_addr(I::TEST_ADDRS.local_ip),
            default_ip_packet::<I>(),
            Default::default(),
        )
        .expect("send should succeed");
    }
    check_frame_counters::<I>(&ctx.core_ctx, 1);

    if has_tx_queue {
        // When a queuing configuration is set, there shouldn't be any sent
        // frames until the queue is explicitly drained.
        assert_matches!(ctx.bindings_ctx.take_ip_frames()[..], [], "unexpected sent IP frame");
        let result = ctx
            .core_api()
            .transmit_queue::<PureIpDevice>()
            .transmit_queued_frames(&device, Default::default(), &mut ())
            .expect("drain queue should succeed");
        assert_eq!(result, WorkQueueReport::AllDone);
        // Expect the PureIpDevice TX task to have been woken.
        assert_eq!(
            core::mem::take(&mut ctx.bindings_ctx.state_mut().tx_available),
            [DeviceId::PureIp(device.clone())]
        );
    }

    let (PureIpDeviceAndIpVersion { device: found_device, version }, packet) = {
        let mut frames = ctx.bindings_ctx.take_ip_frames();
        let frame = frames.pop().expect("exactly one IP frame should have been sent");
        assert_matches!(frames[..], [], "unexpected sent IP frame");
        frame
    };
    assert_eq!(found_device, device.downgrade());
    assert_eq!(version, I::VERSION);
    assert_eq!(packet, default_ip_packet::<I>().into_inner());
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test(I)]
// Verify that a socket can listen on an IP address that is assigned to a
// pure IP device.
fn available_to_socket_layer<I: TestIpExt + IpExt>() {
    let mut ctx = FakeCtx::default();
    let device = ctx
        .core_api()
        .device::<PureIpDevice>()
        .add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);

    let prefix = I::Addr::BYTES * 8;
    let addr = AddrSubnet::new(I::TEST_ADDRS.local_ip.get(), prefix).unwrap();
    ctx.core_api()
        .device_ip::<I>()
        .add_ip_addr_subnet(&device, addr)
        .expect("add address should succeed");

    let socket = ctx.core_api().udp::<I>().create();
    ctx.core_api()
        .udp::<I>()
        .listen(&socket, Some(ZonedAddr::Unzoned(I::TEST_ADDRS.local_ip)), None)
        .expect("listen should succeed");
}

#[test]
fn get_set_mtu() {
    const MTU1: Mtu = Mtu::new(1);
    const MTU2: Mtu = Mtu::new(2);

    let mut ctx = FakeCtx::default();
    let device = ctx.core_api().device::<PureIpDevice>().add_device_with_default_state(
        PureIpDeviceCreationProperties { mtu: MTU1 },
        DEFAULT_INTERFACE_METRIC,
    );
    assert_eq!(pure_ip::get_mtu(&mut ctx.core_ctx(), &device), MTU1);
    pure_ip::set_mtu(&mut ctx.core_ctx(), &device, MTU2);
    assert_eq!(pure_ip::get_mtu(&mut ctx.core_ctx(), &device), MTU2);
}
