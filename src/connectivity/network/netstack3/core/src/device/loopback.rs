// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The loopback device.

use alloc::vec::Vec;
use core::convert::Infallible as Never;

use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::{
    ethernet::Mac,
    ip::{Ip, IpAddress, Mtu},
    SpecifiedAddr,
};
use packet::{Buf, BufferMut, Serializer};
use packet_formats::ethernet::{EtherType, EthernetFrameBuilder, EthernetIpExt};

use crate::{
    context::{CoreTimerContext, ResourceCounterContext, SendableFrameMeta, TimerContext},
    device::{
        id::{BaseDeviceId, BasePrimaryDeviceId, BaseWeakDeviceId},
        queue::{
            rx::{ReceiveQueue, ReceiveQueueBindingsContext, ReceiveQueueState},
            tx::{
                BufVecU8Allocator, TransmitQueue, TransmitQueueBindingsContext,
                TransmitQueueHandler, TransmitQueueState,
            },
            DequeueState, TransmitQueueFrameError,
        },
        socket::{
            DeviceSocketMetadata, DeviceSocketSendTypes, EthernetHeaderParams, HeldDeviceSockets,
        },
        state::{DeviceStateSpec, IpLinkDeviceState},
        Device, DeviceCounters, DeviceIdContext, DeviceLayerEventDispatcher, DeviceLayerTypes,
        DeviceReceiveFrameSpec, EthernetDeviceCounters, WeakDeviceIdentifier,
    },
    sync::{Mutex, RwLock},
    BindingsContext,
};

pub(super) mod integration;

/// The MAC address corresponding to the loopback interface.
const LOOPBACK_MAC: Mac = Mac::UNSPECIFIED;

/// A weak device ID identifying a loopback device.
///
/// This device ID is like [`WeakDeviceId`] but specifically for loopback
/// devices.
///
/// [`WeakDeviceId`]: crate::device::WeakDeviceId
pub type LoopbackWeakDeviceId<BT> = BaseWeakDeviceId<LoopbackDevice, BT>;

/// A strong device ID identifying a loopback device.
///
/// This device ID is like [`DeviceId`] but specifically for loopback devices.
///
/// [`DeviceId`]: crate::device::DeviceId
pub type LoopbackDeviceId<BT> = BaseDeviceId<LoopbackDevice, BT>;

/// The primary reference for a loopback device.
pub(crate) type LoopbackPrimaryDeviceId<BT> = BasePrimaryDeviceId<LoopbackDevice, BT>;

/// Loopback device domain.
#[derive(Copy, Clone)]
pub enum LoopbackDevice {}

impl Device for LoopbackDevice {}

impl DeviceStateSpec for LoopbackDevice {
    type Link<BT: DeviceLayerTypes> = LoopbackDeviceState;
    type External<BT: DeviceLayerTypes> = BT::LoopbackDeviceState;
    type CreationProperties = LoopbackCreationProperties;
    type Counters = EthernetDeviceCounters;
    type TimerId<D: WeakDeviceIdentifier> = Never;

    fn new_link_state<
        CC: CoreTimerContext<Self::TimerId<CC::WeakDeviceId>, BC> + DeviceIdContext<Self>,
        BC: DeviceLayerTypes + TimerContext,
    >(
        _bindings_ctx: &mut BC,
        _self_id: CC::WeakDeviceId,
        LoopbackCreationProperties { mtu }: Self::CreationProperties,
    ) -> Self::Link<BC> {
        LoopbackDeviceState {
            counters: Default::default(),
            mtu,
            rx_queue: Default::default(),
            tx_queue: Default::default(),
        }
    }

    const IS_LOOPBACK: bool = true;
    const DEBUG_TYPE: &'static str = "Loopback";
}

/// Properties used to create a loopback device.
#[derive(Debug)]
pub struct LoopbackCreationProperties {
    /// The device's MTU.
    pub mtu: Mtu,
}

/// State for a loopback device.
pub struct LoopbackDeviceState {
    counters: EthernetDeviceCounters,
    mtu: Mtu,
    rx_queue: ReceiveQueue<LoopbackRxQueueMeta, Buf<Vec<u8>>>,
    tx_queue: TransmitQueue<LoopbackTxQueueMeta, Buf<Vec<u8>>, BufVecU8Allocator>,
}

pub struct LoopbackTxQueueMeta;
pub struct LoopbackRxQueueMeta;
impl From<LoopbackTxQueueMeta> for LoopbackRxQueueMeta {
    fn from(LoopbackTxQueueMeta: LoopbackTxQueueMeta) -> Self {
        Self
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<ReceiveQueueState<LoopbackRxQueueMeta, Buf<Vec<u8>>>>
    for IpLinkDeviceState<LoopbackDevice, BT>
{
    type Lock = Mutex<ReceiveQueueState<LoopbackRxQueueMeta, Buf<Vec<u8>>>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.rx_queue.queue)
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<DequeueState<LoopbackRxQueueMeta, Buf<Vec<u8>>>>
    for IpLinkDeviceState<LoopbackDevice, BT>
{
    type Lock = Mutex<DequeueState<LoopbackRxQueueMeta, Buf<Vec<u8>>>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.rx_queue.deque)
    }
}

impl<BT: DeviceLayerTypes>
    OrderedLockAccess<TransmitQueueState<LoopbackTxQueueMeta, Buf<Vec<u8>>, BufVecU8Allocator>>
    for IpLinkDeviceState<LoopbackDevice, BT>
{
    type Lock = Mutex<TransmitQueueState<LoopbackTxQueueMeta, Buf<Vec<u8>>, BufVecU8Allocator>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.tx_queue.queue)
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<DequeueState<LoopbackTxQueueMeta, Buf<Vec<u8>>>>
    for IpLinkDeviceState<LoopbackDevice, BT>
{
    type Lock = Mutex<DequeueState<LoopbackTxQueueMeta, Buf<Vec<u8>>>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.tx_queue.deque)
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<HeldDeviceSockets<BT>>
    for IpLinkDeviceState<LoopbackDevice, BT>
{
    type Lock = RwLock<HeldDeviceSockets<BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.sockets)
    }
}

impl DeviceSocketSendTypes for LoopbackDevice {
    /// When `None`, data will be sent as a raw Ethernet frame without any
    /// system-applied headers.
    type Metadata = Option<EthernetHeaderParams>;
}

impl<CC, BC> SendableFrameMeta<CC, BC> for DeviceSocketMetadata<LoopbackDevice, CC::DeviceId>
where
    CC: TransmitQueueHandler<LoopbackDevice, BC, Meta = LoopbackTxQueueMeta>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
{
    fn send_meta<S>(self, core_ctx: &mut CC, bindings_ctx: &mut BC, body: S) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        let Self { device_id, metadata } = self;
        match metadata {
            Some(EthernetHeaderParams { dest_addr, protocol }) => send_as_ethernet_frame_to_dst(
                core_ctx,
                bindings_ctx,
                &device_id,
                body,
                protocol,
                dest_addr,
            ),
            None => send_ethernet_frame(core_ctx, bindings_ctx, &device_id, body),
        }
    }
}

pub(super) fn send_ip_frame<CC, BC, A, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    _local_addr: SpecifiedAddr<A>,
    packet: S,
) -> Result<(), S>
where
    CC: TransmitQueueHandler<LoopbackDevice, BC, Meta = LoopbackTxQueueMeta>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
    A: IpAddress,
    A::Version: EthernetIpExt,
    S: Serializer,
    S::Buffer: BufferMut,
{
    core_ctx.with_counters(|counters: &DeviceCounters| {
        let () = A::Version::map_ip(
            (),
            |()| counters.send_ipv4_frame.increment(),
            |()| counters.send_ipv6_frame.increment(),
        );
    });
    send_as_ethernet_frame_to_dst(
        core_ctx,
        bindings_ctx,
        device_id,
        packet,
        <A::Version as EthernetIpExt>::ETHER_TYPE,
        LOOPBACK_MAC,
    )
}

fn send_as_ethernet_frame_to_dst<CC, BC, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    packet: S,
    protocol: EtherType,
    dst_mac: Mac,
) -> Result<(), S>
where
    CC: TransmitQueueHandler<LoopbackDevice, BC, Meta = LoopbackTxQueueMeta>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
    S: Serializer,
    S::Buffer: BufferMut,
{
    /// The minimum length of bodies of Ethernet frames sent over the loopback
    /// device.
    ///
    /// Use zero since the frames are never sent out a physical device, so it
    /// doesn't matter if they are shorter than would be required.
    const MIN_BODY_LEN: usize = 0;

    let frame = packet.encapsulate(EthernetFrameBuilder::new(
        LOOPBACK_MAC,
        dst_mac,
        protocol,
        MIN_BODY_LEN,
    ));

    send_ethernet_frame(core_ctx, bindings_ctx, device_id, frame).map_err(|s| s.into_inner())
}

fn send_ethernet_frame<CC, BC, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    frame: S,
) -> Result<(), S>
where
    CC: TransmitQueueHandler<LoopbackDevice, BC, Meta = LoopbackTxQueueMeta>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
    S: Serializer,
    S::Buffer: BufferMut,
{
    core_ctx.increment(device_id, |counters: &DeviceCounters| &counters.send_total_frames);
    match TransmitQueueHandler::<LoopbackDevice, _>::queue_tx_frame(
        core_ctx,
        bindings_ctx,
        device_id,
        LoopbackTxQueueMeta,
        frame,
    ) {
        Ok(()) => {
            core_ctx.increment(device_id, |counters: &DeviceCounters| &counters.send_frame);
            Ok(())
        }
        Err(TransmitQueueFrameError::NoQueue(_)) => {
            unreachable!("loopback never fails to send a frame")
        }
        Err(TransmitQueueFrameError::QueueFull(s)) => {
            core_ctx.increment(device_id, |counters: &DeviceCounters| &counters.send_queue_full);
            Err(s)
        }
        Err(TransmitQueueFrameError::SerializeError(s)) => {
            core_ctx
                .increment(device_id, |counters: &DeviceCounters| &counters.send_serialize_error);
            Err(s)
        }
    }
}

impl DeviceReceiveFrameSpec for LoopbackDevice {
    // Loopback never receives frames from bindings, so make it impossible to
    // instantiate it.
    type FrameMetadata<D> = Never;
}

// TODO(https://fxbug.dev/338448926): This implementation won't hold post-crate
// split. We need bindings to implement ReceiveQueueBindingsContext directly.
impl<BC: BindingsContext> ReceiveQueueBindingsContext<LoopbackDevice, LoopbackDeviceId<BC>> for BC {
    fn wake_rx_task(&mut self, device_id: &LoopbackDeviceId<BC>) {
        DeviceLayerEventDispatcher::wake_rx_task(self, device_id)
    }
}

// TODO(https://fxbug.dev/338448926): This implementation won't hold post-crate
// split. We need bindings to implement TransmitQueueBindingsContext directly.
impl<BC: BindingsContext> TransmitQueueBindingsContext<LoopbackDevice, LoopbackDeviceId<BC>>
    for BC
{
    fn wake_tx_task(&mut self, device_id: &LoopbackDeviceId<BC>) {
        DeviceLayerEventDispatcher::wake_tx_task(self, &device_id.clone().into())
    }
}

#[cfg(test)]
mod tests {

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;

    use net_types::ip::{AddrSubnet, Ipv4, Ipv6};
    use packet::ParseBuffer;
    use packet_formats::ethernet::{EthernetFrame, EthernetFrameLengthCheck};

    use crate::{
        device::queue::rx::ReceiveQueueContext,
        error::NotFoundError,
        ip::device::IpAddressId as _,
        testutil::{
            CtxPairExt as _, FakeBindingsCtx, TestAddrs, TestIpExt, DEFAULT_INTERFACE_METRIC,
        },
    };

    use super::*;

    const MTU: Mtu = Mtu::new(66);

    #[test]
    fn loopback_mtu() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<LoopbackDevice>()
            .add_device_with_default_state(
                LoopbackCreationProperties { mtu: MTU },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);

        assert_eq!(
            crate::ip::IpDeviceContext::<Ipv4, _>::get_mtu(&mut ctx.core_ctx(), &device),
            MTU
        );
        assert_eq!(
            crate::ip::IpDeviceContext::<Ipv6, _>::get_mtu(&mut ctx.core_ctx(), &device),
            MTU
        );
    }

    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    #[ip_test]
    fn test_loopback_add_remove_addrs<I: Ip + TestIpExt + crate::IpExt>() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<LoopbackDevice>()
            .add_device_with_default_state(
                LoopbackCreationProperties { mtu: MTU },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);

        let get_addrs = |ctx: &mut crate::testutil::FakeCtx| {
            crate::ip::device::IpDeviceStateContext::<I, _>::with_address_ids(
                &mut ctx.core_ctx(),
                &device,
                |addrs, _core_ctx| addrs.map(|a| SpecifiedAddr::from(a.addr())).collect::<Vec<_>>(),
            )
        };

        let TestAddrs { subnet, local_ip, local_mac: _, remote_ip: _, remote_mac: _ } =
            I::TEST_ADDRS;
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

        assert_matches!(
            ctx.core_api().device_ip::<I>().del_ip_addr(&device, addr),
            Err(NotFoundError)
        );
    }

    #[ip_test]
    fn loopback_sends_ethernet<I: Ip + TestIpExt + crate::IpExt>() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx.core_api().device::<LoopbackDevice>().add_device_with_default_state(
            LoopbackCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        crate::device::testutil::enable_device(&mut ctx, &device.clone().into());
        let crate::testutil::FakeCtx { core_ctx, bindings_ctx } = &mut ctx;

        let local_addr = I::TEST_ADDRS.local_ip;
        const BODY: &[u8] = b"IP body".as_slice();

        let body = Buf::new(Vec::from(BODY), ..);
        send_ip_frame(&mut core_ctx.context(), bindings_ctx, &device, local_addr, body)
            .expect("can send");

        // There is no transmit queue so the frames will immediately go into the
        // receive queue.
        let mut frames = ReceiveQueueContext::<LoopbackDevice, _>::with_receive_queue_mut(
            &mut core_ctx.context(),
            &device,
            |queue_state| {
                queue_state
                    .take_frames()
                    .map(|(LoopbackRxQueueMeta, frame)| frame)
                    .collect::<Vec<_>>()
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
}
