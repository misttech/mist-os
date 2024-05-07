// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The loopback device.

use alloc::vec::Vec;
use core::convert::Infallible as Never;

use lock_order::{
    lock::{LockFor, RwLockFor, UnlockedAccess},
    relation::LockBefore,
    wrap::prelude::*,
};
use net_types::{
    ethernet::Mac,
    ip::{Ip, IpAddress, Mtu},
    SpecifiedAddr,
};
use packet::{Buf, Buffer as _, BufferMut, Serializer};
use packet_formats::ethernet::{
    EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck, EthernetIpExt,
};
use tracing::trace;

use crate::{
    context::{
        CoreTimerContext, CounterContext, ResourceCounterContext, SendableFrameMeta, TimerContext,
    },
    device::{
        self,
        id::{BaseDeviceId, BasePrimaryDeviceId, BaseWeakDeviceId},
        queue::{
            rx::{
                ReceiveDequeContext, ReceiveDequeFrameContext, ReceiveQueue,
                ReceiveQueueBindingsContext, ReceiveQueueContext, ReceiveQueueHandler,
                ReceiveQueueState, ReceiveQueueTypes,
            },
            tx::{
                BufVecU8Allocator, TransmitDequeueContext, TransmitQueue,
                TransmitQueueBindingsContext, TransmitQueueCommon, TransmitQueueContext,
                TransmitQueueHandler, TransmitQueueState,
            },
            DequeueState, ReceiveQueueFullError, TransmitQueueFrameError,
        },
        socket::{
            DeviceSocketHandler, DeviceSocketMetadata, DeviceSocketSendTypes, EthernetHeaderParams,
            HeldDeviceSockets, ParseSentFrameError, ReceivedFrame, SentFrame,
        },
        state::{DeviceStateSpec, IpLinkDeviceState},
        Device, DeviceCounters, DeviceIdContext, DeviceLayerEventDispatcher, DeviceLayerTypes,
        DeviceReceiveFrameSpec, DeviceSendFrameError, EthernetDeviceCounters, FrameDestination,
    },
    BindingsContext, BindingsTypes, CoreCtx,
};

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
    type TimerId<D: device::WeakId> = Never;

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

impl<BT: BindingsTypes, L> DeviceIdContext<LoopbackDevice> for CoreCtx<'_, BT, L> {
    type DeviceId = LoopbackDeviceId<BT>;
    type WeakDeviceId = LoopbackWeakDeviceId<BT>;
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
    rx_queue: ReceiveQueue<(), Buf<Vec<u8>>>,
    tx_queue: TransmitQueue<(), Buf<Vec<u8>>, BufVecU8Allocator>,
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::EthernetDeviceCounters>
    for IpLinkDeviceState<LoopbackDevice, BC>
{
    type Data = EthernetDeviceCounters;
    type Guard<'l> = &'l EthernetDeviceCounters
        where
            Self: 'l ;
    fn access(&self) -> Self::Guard<'_> {
        &self.link.counters
    }
}

impl<BC: BindingsContext> LockFor<crate::lock_ordering::LoopbackRxQueue>
    for IpLinkDeviceState<LoopbackDevice, BC>
{
    type Data = ReceiveQueueState<(), Buf<Vec<u8>>>;
    type Guard<'l> = crate::sync::LockGuard<'l, ReceiveQueueState<(), Buf<Vec<u8>>>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.link.rx_queue.queue.lock()
    }
}

impl<BC: BindingsContext> LockFor<crate::lock_ordering::LoopbackRxDequeue>
    for IpLinkDeviceState<LoopbackDevice, BC>
{
    type Data = DequeueState<(), Buf<Vec<u8>>>;
    type Guard<'l> = crate::sync::LockGuard<'l, DequeueState<(), Buf<Vec<u8>>>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.link.rx_queue.deque.lock()
    }
}

impl<BC: BindingsContext> LockFor<crate::lock_ordering::LoopbackTxQueue>
    for IpLinkDeviceState<LoopbackDevice, BC>
{
    type Data = TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>;
    type Guard<'l> = crate::sync::LockGuard<'l, TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.link.tx_queue.queue.lock()
    }
}

impl<BC: BindingsContext> LockFor<crate::lock_ordering::LoopbackTxDequeue>
    for IpLinkDeviceState<LoopbackDevice, BC>
{
    type Data = DequeueState<(), Buf<Vec<u8>>>;
    type Guard<'l> = crate::sync::LockGuard<'l, DequeueState<(), Buf<Vec<u8>>>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.link.tx_queue.deque.lock()
    }
}

impl<BC: BindingsContext> RwLockFor<crate::lock_ordering::DeviceSockets>
    for IpLinkDeviceState<LoopbackDevice, BC>
{
    type Data = HeldDeviceSockets<BC>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, HeldDeviceSockets<BC>>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, HeldDeviceSockets<BC>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.sockets.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.sockets.write()
    }
}

impl DeviceSocketSendTypes for LoopbackDevice {
    /// When `None`, data will be sent as a raw Ethernet frame without any
    /// system-applied headers.
    type Metadata = Option<EthernetHeaderParams>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    SendableFrameMeta<CoreCtx<'_, BC, L>, BC>
    for DeviceSocketMetadata<LoopbackDevice, LoopbackDeviceId<BC>>
{
    fn send_meta<S>(
        self,
        core_ctx: &mut CoreCtx<'_, BC, L>,
        bindings_ctx: &mut BC,
        body: S,
    ) -> Result<(), S>
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

pub(super) fn send_ip_frame<BC, A, S, L>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    bindings_ctx: &mut BC,
    device_id: &LoopbackDeviceId<BC>,
    _local_addr: SpecifiedAddr<A>,
    packet: S,
) -> Result<(), S>
where
    BC: BindingsContext,
    A: IpAddress,
    S: Serializer,
    S::Buffer: BufferMut,
    L: LockBefore<crate::lock_ordering::LoopbackTxQueue>,
    A::Version: EthernetIpExt,
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

fn send_as_ethernet_frame_to_dst<BC, S, L>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    bindings_ctx: &mut BC,
    device_id: &LoopbackDeviceId<BC>,
    packet: S,
    protocol: EtherType,
    dst_mac: Mac,
) -> Result<(), S>
where
    BC: BindingsContext,
    S: Serializer,
    S::Buffer: BufferMut,
    L: LockBefore<crate::lock_ordering::LoopbackTxQueue>,
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

fn send_ethernet_frame<L, S, BC>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    bindings_ctx: &mut BC,
    device_id: &LoopbackDeviceId<BC>,
    frame: S,
) -> Result<(), S>
where
    L: LockBefore<crate::lock_ordering::LoopbackTxQueue>,
    S: Serializer,
    S::Buffer: BufferMut,
    BC: BindingsContext,
{
    core_ctx.increment(device_id, |counters: &DeviceCounters| &counters.send_total_frames);
    match TransmitQueueHandler::<LoopbackDevice, _>::queue_tx_frame(
        core_ctx,
        bindings_ctx,
        device_id,
        (),
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

/// Gets the MTU associated with this device.
pub(super) fn get_mtu<BC: BindingsContext, L>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    device_id: &LoopbackDeviceId<BC>,
) -> Mtu {
    device::integration::with_device_state(core_ctx, device_id, |mut state| {
        state.cast_with(|s| &s.link.mtu).copied()
    })
}

impl DeviceReceiveFrameSpec for LoopbackDevice {
    // Loopback never receives frames from bindings, so make it impossible to
    // instantiate it.
    type FrameMetadata<D> = Never;
}

impl<BC: BindingsContext> ReceiveQueueBindingsContext<LoopbackDevice, LoopbackDeviceId<BC>> for BC {
    fn wake_rx_task(&mut self, device_id: &LoopbackDeviceId<BC>) {
        DeviceLayerEventDispatcher::wake_rx_task(self, device_id)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueTypes<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type Meta = ();
    type Buffer = Buf<Vec<u8>>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    fn with_receive_queue_mut<
        O,
        F: FnOnce(&mut ReceiveQueueState<Self::Meta, Self::Buffer>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_device_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackRxQueue>();
            cb(&mut x)
        })
    }
}

impl<BC: BindingsContext> ReceiveDequeFrameContext<LoopbackDevice, BC>
    for CoreCtx<'_, BC, crate::lock_ordering::LoopbackRxDequeue>
{
    fn handle_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &LoopbackDeviceId<BC>,
        (): Self::Meta,
        mut buf: Buf<Vec<u8>>,
    ) {
        self.increment(device_id, |counters: &DeviceCounters| &counters.recv_frame);
        let (frame, whole_body) = match buf
            .parse_with_view::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::NoCheck)
        {
            Err(e) => {
                self.increment(device_id, |counters: &DeviceCounters| &counters.recv_parse_error);
                trace!("dropping invalid ethernet frame over loopback: {:?}", e);
                return;
            }
            Ok(e) => e,
        };

        let frame_dest = FrameDestination::from_dest(frame.dst_mac(), Mac::UNSPECIFIED);
        let ethertype = frame.ethertype();

        DeviceSocketHandler::<LoopbackDevice, _>::handle_frame(
            self,
            bindings_ctx,
            device_id,
            ReceivedFrame::from_ethernet(frame, frame_dest).into(),
            whole_body,
        );

        let ethertype = match ethertype {
            Some(e) => e,
            None => {
                self.increment(device_id, |counters: &EthernetDeviceCounters| {
                    &counters.recv_no_ethertype
                });
                trace!("dropping ethernet frame without ethertype");
                return;
            }
        };

        match ethertype {
            EtherType::Ipv4 => {
                self.increment(device_id, |counters: &DeviceCounters| {
                    &counters.recv_ipv4_delivered
                });
                crate::ip::receive_ipv4_packet(
                    self,
                    bindings_ctx,
                    &device_id.clone().into(),
                    Some(frame_dest),
                    buf,
                )
            }
            EtherType::Ipv6 => {
                self.increment(device_id, |counters: &DeviceCounters| {
                    &counters.recv_ipv6_delivered
                });
                crate::ip::receive_ipv6_packet(
                    self,
                    bindings_ctx,
                    &device_id.clone().into(),
                    Some(frame_dest),
                    buf,
                )
            }
            ethertype @ EtherType::Arp | ethertype @ EtherType::Other(_) => {
                self.increment(device_id, |counters: &EthernetDeviceCounters| {
                    &counters.recv_unsupported_ethertype
                });
                trace!("not handling loopback frame of type {:?}", ethertype)
            }
        }
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxDequeue>>
    ReceiveDequeContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type ReceiveQueueCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::LoopbackRxDequeue>;

    fn with_dequed_frames_and_rx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<(), Buf<Vec<u8>>>, &mut Self::ReceiveQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_device_state_and_core_ctx(
            self,
            device_id,
            |mut core_ctx_and_resource| {
                let (mut x, mut locked) =
                    core_ctx_and_resource
                        .lock_with_and::<crate::lock_ordering::LoopbackRxDequeue, _>(|c| c.right());
                cb(&mut x, &mut locked.cast_core_ctx())
            },
        )
    }
}

impl<BC: BindingsContext> TransmitQueueBindingsContext<LoopbackDevice, LoopbackDeviceId<BC>>
    for BC
{
    fn wake_tx_task(&mut self, device_id: &LoopbackDeviceId<BC>) {
        DeviceLayerEventDispatcher::wake_tx_task(self, &device_id.clone().into())
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    TransmitQueueCommon<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type Meta = ();
    type Allocator = BufVecU8Allocator;
    type Buffer = Buf<Vec<u8>>;

    fn parse_outgoing_frame<'a, 'b>(
        buf: &'a [u8],
        (): &'b Self::Meta,
    ) -> Result<SentFrame<&'a [u8]>, ParseSentFrameError> {
        SentFrame::try_parse_as_ethernet(buf)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    TransmitQueueContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    fn with_transmit_queue_mut<
        O,
        F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_device_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackTxQueue>();
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
        // Never handle frames synchronously with the send path - always queue
        // the frame to be received by the loopback device into a queue which
        // a dedicated RX task will kick to handle the queued packet.
        //
        // This is done so that a socket lock may be held while sending a packet
        // which may need to be delivered to the sending socket itself. Without
        // this decoupling of RX/TX paths, sending a packet while holding onto
        // the socket lock will result in a deadlock.
        match ReceiveQueueHandler::queue_rx_frame(self, bindings_ctx, device_id, meta, buf) {
            Ok(()) => {}
            Err(ReceiveQueueFullError(((), _frame))) => {
                // RX queue is full - there is nothing further we can do here.
                tracing::error!("dropped RX frame on loopback device due to full RX queue")
            }
        }

        Ok(())
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxDequeue>>
    TransmitDequeueContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type TransmitQueueCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::LoopbackTxDequeue>;

    fn with_dequed_packets_and_tx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::TransmitQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        device::integration::with_device_state_and_core_ctx(
            self,
            device_id,
            |mut core_ctx_and_resource| {
                let (mut x, mut locked) =
                    core_ctx_and_resource
                        .lock_with_and::<crate::lock_ordering::LoopbackTxDequeue, _>(|c| c.right());
                cb(&mut x, &mut locked.cast_core_ctx())
            },
        )
    }
}

#[cfg(test)]
mod tests {

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;

    use net_types::ip::{AddrSubnet, Ipv4, Ipv6};
    use packet::ParseBuffer;

    use crate::{
        error::NotFoundError,
        ip::device::IpAddressId as _,
        testutil::{
            FakeBindingsCtx, FakeEventDispatcherConfig, TestIpExt, DEFAULT_INTERFACE_METRIC,
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

        let FakeEventDispatcherConfig {
            subnet,
            local_ip,
            local_mac: _,
            remote_ip: _,
            remote_mac: _,
        } = I::FAKE_CONFIG;
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
    fn loopback_sends_ethernet<I: Ip + TestIpExt>() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx.core_api().device::<LoopbackDevice>().add_device_with_default_state(
            LoopbackCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        crate::device::testutil::enable_device(&mut ctx, &device.clone().into());
        let crate::testutil::FakeCtx { core_ctx, bindings_ctx } = &mut ctx;

        let local_addr = I::FAKE_CONFIG.local_ip;
        const BODY: &[u8] = b"IP body".as_slice();

        let body = Buf::new(Vec::from(BODY), ..);
        send_ip_frame(&mut core_ctx.context(), bindings_ctx, &device, local_addr, body)
            .expect("can send");

        // There is no transmit queue so the frames will immediately go into the
        // receive queue.
        let mut frames = ReceiveQueueContext::<LoopbackDevice, _>::with_receive_queue_mut(
            &mut core_ctx.context(),
            &device,
            |queue_state| queue_state.take_frames().map(|((), frame)| frame).collect::<Vec<_>>(),
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
