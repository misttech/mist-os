// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of traits defined in foreign modules for the types defined
//! in the pure_ip module.

use alloc::{fmt::Debug, vec::Vec};
use lock_order::{
    lock::{LockLevelFor, UnlockedAccessMarkerFor},
    relation::LockBefore,
    wrap::LockedWrapperApi,
};
use net_types::ip::{Ip, Ipv4, Ipv6};
use packet::{Buf, BufferMut};

use crate::{
    context::{ReceivableFrameMeta, ResourceCounterContext},
    device::{
        config::DeviceConfigurationContext,
        pure_ip::{
            DynamicPureIpDeviceState, PureIpDevice, PureIpDeviceCounters, PureIpDeviceId,
            PureIpDeviceStateContext, PureIpDeviceTxQueueFrameMetadata, PureIpPrimaryDeviceId,
            PureIpWeakDeviceId,
        },
        queue::{
            tx::{
                BufVecU8Allocator, TransmitDequeueContext, TransmitQueueCommon,
                TransmitQueueContext, TransmitQueueState,
            },
            DequeueState,
        },
        socket::{IpFrame, ParseSentFrameError, SentFrame},
        state::IpLinkDeviceState,
        DeviceCollectionContext, DeviceCounters, DeviceIdContext, DeviceLayerEventDispatcher,
        DeviceSendFrameError, RecvIpFrameMeta,
    },
    neighbor::NudUserConfig,
    BindingsContext, BindingsTypes, CoreCtx,
};

impl<BT: BindingsTypes, L> DeviceIdContext<PureIpDevice> for CoreCtx<'_, BT, L> {
    type DeviceId = PureIpDeviceId<BT>;
    type WeakDeviceId = PureIpWeakDeviceId<BT>;
}

impl<'a, BT, L> DeviceCollectionContext<PureIpDevice, BT> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
    L: LockBefore<crate::lock_ordering::DeviceLayerState>,
{
    fn insert(&mut self, device: PureIpPrimaryDeviceId<BT>) {
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        let strong = device.clone_strong();
        assert!(devices.pure_ip.insert(strong, device).is_none());
    }

    fn remove(&mut self, device: &PureIpDeviceId<BT>) -> Option<PureIpPrimaryDeviceId<BT>> {
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.pure_ip.remove(device)
    }
}

impl<'a, BT, L> DeviceConfigurationContext<PureIpDevice> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
{
    fn with_nud_config<I: Ip, O, F: FnOnce(Option<&NudUserConfig>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        // PureIp doesn't support NUD.
        f(None)
    }

    fn with_nud_config_mut<I: Ip, O, F: FnOnce(Option<&mut NudUserConfig>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        // PureIp doesn't support NUD.
        f(None)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceRxDequeue>>
    ReceivableFrameMeta<CoreCtx<'_, BC, L>, BC> for RecvIpFrameMeta<PureIpDeviceId<BC>, Ipv4>
{
    fn receive_meta<B: BufferMut + Debug>(
        self,
        core_ctx: &mut CoreCtx<'_, BC, L>,
        bindings_ctx: &mut BC,
        frame: B,
    ) {
        let Self { device, frame_dst, _marker: _ } = self;
        core_ctx.increment(&device, |counters: &DeviceCounters| &counters.recv_ipv4_delivered);
        crate::ip::receive_ipv4_packet(core_ctx, bindings_ctx, &device.into(), frame_dst, frame);
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceRxDequeue>>
    ReceivableFrameMeta<CoreCtx<'_, BC, L>, BC> for RecvIpFrameMeta<PureIpDeviceId<BC>, Ipv6>
{
    fn receive_meta<B: BufferMut + Debug>(
        self,
        core_ctx: &mut CoreCtx<'_, BC, L>,
        bindings_ctx: &mut BC,
        frame: B,
    ) {
        let Self { device, frame_dst, _marker: _ } = self;
        core_ctx.increment(&device, |counters: &DeviceCounters| &counters.recv_ipv6_delivered);
        crate::ip::receive_ipv6_packet(core_ctx, bindings_ctx, &device.into(), frame_dst, frame);
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceTxQueue>>
    TransmitQueueCommon<PureIpDevice, BC> for CoreCtx<'_, BC, L>
{
    type Meta = PureIpDeviceTxQueueFrameMetadata;
    type Allocator = BufVecU8Allocator;
    type Buffer = Buf<Vec<u8>>;

    fn parse_outgoing_frame<'a, 'b>(
        buf: &'a [u8],
        meta: &'b Self::Meta,
    ) -> Result<SentFrame<&'a [u8]>, ParseSentFrameError> {
        let PureIpDeviceTxQueueFrameMetadata { ip_version } = meta;
        // NB: For conformance with Linux, don't verify that the contents of
        // of the buffer are a valid IPv4/IPv6 packet. Device sockets are
        // allowed to receive malformed packets.
        Ok(SentFrame::Ip(IpFrame { ip_version: *ip_version, body: buf }))
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceTxQueue>>
    TransmitQueueContext<PureIpDevice, BC> for CoreCtx<'_, BC, L>
{
    fn with_transmit_queue_mut<
        O,
        F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::integration::with_device_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::PureIpDeviceTxQueue>();
            cb(&mut x)
        })
    }

    fn send_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        buf: Self::Buffer,
    ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>> {
        let PureIpDeviceTxQueueFrameMetadata { ip_version } = meta;
        DeviceLayerEventDispatcher::send_ip_packet(bindings_ctx, device_id, buf, ip_version)
            .map_err(|DeviceSendFrameError::DeviceNotReady(buf)| {
                DeviceSendFrameError::DeviceNotReady((meta, buf))
            })
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceTxDequeue>>
    TransmitDequeueContext<PureIpDevice, BC> for CoreCtx<'_, BC, L>
{
    type TransmitQueueCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::PureIpDeviceTxDequeue>;

    fn with_dequed_packets_and_tx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::TransmitQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::integration::with_device_state_and_core_ctx(
            self,
            device_id,
            |mut core_ctx_and_resource| {
                let (mut x, mut locked) = core_ctx_and_resource
                    .lock_with_and::<crate::lock_ordering::PureIpDeviceTxDequeue, _>(
                    |c| c.right(),
                );
                cb(&mut x, &mut locked.cast_core_ctx())
            },
        )
    }
}

impl<BT: BindingsTypes> LockLevelFor<IpLinkDeviceState<PureIpDevice, BT>>
    for crate::lock_ordering::PureIpDeviceDynamicState
{
    type Data = DynamicPureIpDeviceState;
}

impl<BT: BindingsTypes> LockLevelFor<IpLinkDeviceState<PureIpDevice, BT>>
    for crate::lock_ordering::PureIpDeviceTxQueue
{
    type Data =
        TransmitQueueState<PureIpDeviceTxQueueFrameMetadata, Buf<Vec<u8>>, BufVecU8Allocator>;
}

impl<BT: BindingsTypes> LockLevelFor<IpLinkDeviceState<PureIpDevice, BT>>
    for crate::lock_ordering::PureIpDeviceTxDequeue
{
    type Data = DequeueState<PureIpDeviceTxQueueFrameMetadata, Buf<Vec<u8>>>;
}

impl<BT: BindingsTypes> UnlockedAccessMarkerFor<IpLinkDeviceState<PureIpDevice, BT>>
    for crate::lock_ordering::PureIpDeviceCounters
{
    type Data = PureIpDeviceCounters;
    fn unlocked_access(t: &IpLinkDeviceState<PureIpDevice, BT>) -> &Self::Data {
        &t.link.counters
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceDynamicState>>
    PureIpDeviceStateContext for CoreCtx<'_, BC, L>
{
    fn with_pure_ip_state<O, F: FnOnce(&DynamicPureIpDeviceState) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::integration::with_device_state(self, device_id, |mut state| {
            let dynamic_state = state.read_lock::<crate::lock_ordering::PureIpDeviceDynamicState>();
            cb(&dynamic_state)
        })
    }

    fn with_pure_ip_state_mut<O, F: FnOnce(&mut DynamicPureIpDeviceState) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::integration::with_device_state(self, device_id, |mut state| {
            let mut dynamic_state =
                state.write_lock::<crate::lock_ordering::PureIpDeviceDynamicState>();
            cb(&mut dynamic_state)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_types::{
        ip::{AddrSubnet, IpAddress as _, IpVersion, Mtu},
        Witness, ZonedAddr,
    };
    use packet::Serializer as _;
    use packet_formats::ip::{IpPacketBuilder, IpProto};
    use test_case::test_case;

    use crate::{
        device::{
            pure_ip::{PureIpDeviceCreationProperties, PureIpDeviceReceiveFrameMetadata},
            DeviceId, TransmitQueueConfiguration,
        },
        ip::IpLayerIpExt,
        sync::RemoveResourceResult,
        testutil::{
            CtxPairExt as _, FakeBindingsCtx, PureIpDeviceAndIpVersion, TestIpExt,
            DEFAULT_INTERFACE_METRIC,
        },
        types::WorkQueueReport,
        StackState,
    };

    const MTU: Mtu = Mtu::new(1234);
    const TTL: u8 = 64;

    fn default_ip_packet<I: Ip + TestIpExt>() -> Buf<Vec<u8>> {
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
        let mut ctx = crate::testutil::FakeCtx::default();
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
        let mut ctx = crate::testutil::FakeCtx::default();
        let mut device_api = ctx.core_api().device::<PureIpDevice>();
        let device = device_api.add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        let mut tx_queue_api = ctx.core_api().transmit_queue::<PureIpDevice>();
        tx_queue_api.set_configuration(&device, TransmitQueueConfiguration::Fifo);
    }

    #[ip_test]
    fn receive_frame<I: Ip + TestIpExt + crate::IpExt>() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx.core_api().device::<PureIpDevice>().add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        crate::device::testutil::enable_device(&mut ctx, &device.clone().into());

        fn check_frame_counters<I: IpLayerIpExt>(
            stack_state: &StackState<FakeBindingsCtx>,
            count: u64,
        ) {
            assert_eq!(stack_state.ip_counters::<I>().receive_ip_packet.get(), count);
            assert_eq!(stack_state.device_counters().recv_frame.get(), count);
            match I::VERSION {
                IpVersion::V4 => {
                    assert_eq!(stack_state.device_counters().recv_ipv4_delivered.get(), count)
                }
                IpVersion::V6 => {
                    assert_eq!(stack_state.device_counters().recv_ipv6_delivered.get(), count)
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

    #[ip_test]
    #[test_case(TransmitQueueConfiguration::None; "no queue")]
    #[test_case(TransmitQueueConfiguration::Fifo; "fifo queue")]
    fn send_frame<I: Ip + TestIpExt + crate::IpExt>(tx_queue_config: TransmitQueueConfiguration) {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx.core_api().device::<PureIpDevice>().add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        crate::device::testutil::enable_device(&mut ctx, &device.clone().into());
        let has_tx_queue = match tx_queue_config {
            TransmitQueueConfiguration::None => false,
            TransmitQueueConfiguration::Fifo => true,
        };
        ctx.core_api().transmit_queue::<PureIpDevice>().set_configuration(&device, tx_queue_config);

        fn check_frame_counters<I: IpLayerIpExt>(
            stack_state: &StackState<FakeBindingsCtx>,
            count: u64,
        ) {
            assert_eq!(stack_state.device_counters().send_total_frames.get(), count);
            assert_eq!(stack_state.device_counters().send_frame.get(), count);
            match I::VERSION {
                IpVersion::V4 => {
                    assert_eq!(stack_state.device_counters().send_ipv4_frame.get(), count)
                }
                IpVersion::V6 => {
                    assert_eq!(stack_state.device_counters().send_ipv6_frame.get(), count)
                }
            }
        }

        assert_matches!(ctx.bindings_ctx.take_ip_frames()[..], [], "unexpected sent IP frame");
        check_frame_counters::<I>(&ctx.core_ctx, 0);

        {
            let (mut core_ctx, bindings_ctx) = ctx.contexts();
            crate::device::pure_ip::send_ip_frame::<_, _, I, _>(
                &mut core_ctx,
                bindings_ctx,
                &device,
                default_ip_packet::<I>(),
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
                .transmit_queued_frames(&device)
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

    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    #[ip_test]
    // Verify that a socket can listen on an IP address that is assigned to a
    // pure IP device.
    fn available_to_socket_layer<I: Ip + TestIpExt + crate::IpExt>() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<PureIpDevice>()
            .add_device_with_default_state(
                PureIpDeviceCreationProperties { mtu: MTU },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);

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

        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx.core_api().device::<PureIpDevice>().add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU1 },
            DEFAULT_INTERFACE_METRIC,
        );
        assert_eq!(crate::device::pure_ip::get_mtu(&mut ctx.core_ctx(), &device), MTU1);
        crate::device::pure_ip::set_mtu(&mut ctx.core_ctx(), &device, MTU2);
        assert_eq!(crate::device::pure_ip::get_mtu(&mut ctx.core_ctx(), &device), MTU2);
    }
}
