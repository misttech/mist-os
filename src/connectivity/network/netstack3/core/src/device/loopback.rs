// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The loopback device.

use alloc::vec::Vec;
use core::fmt::{self, Debug, Display, Formatter};

use derivative::Derivative;
use lock_order::{
    lock::{LockFor, RwLockFor},
    relation::LockBefore,
    Locked,
};
use net_types::{ethernet::Mac, ip::IpAddress, SpecifiedAddr};
use packet::{Buf, Buffer as _, BufferMut, Serializer};
use packet_formats::ethernet::{
    EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck, EthernetIpExt,
};
use tracing::trace;

use crate::{
    context::SendFrameContext,
    device::{
        queue::{
            rx::{
                BufferReceiveQueueHandler, ReceiveDequeContext, ReceiveDequeFrameContext,
                ReceiveQueue, ReceiveQueueContext, ReceiveQueueNonSyncContext, ReceiveQueueState,
                ReceiveQueueTypes,
            },
            tx::{
                BufVecU8Allocator, BufferTransmitQueueHandler, TransmitDequeueContext,
                TransmitQueue, TransmitQueueCommon, TransmitQueueContext,
                TransmitQueueNonSyncContext, TransmitQueueState,
            },
            DequeueState, ReceiveQueueFullError, TransmitQueueFrameError,
        },
        socket::{
            BufferSocketHandler, DatagramHeader, DeviceSocketMetadata, HeldDeviceSockets,
            ParseSentFrameError, ReceivedFrame, SentFrame,
        },
        state::IpLinkDeviceState,
        with_loopback_state, with_loopback_state_and_sync_ctx, Device, DeviceIdContext,
        DeviceLayerEventDispatcher, DeviceLayerTypes, DeviceSendFrameError, FrameDestination,
    },
    device::{Id, Mtu, StrongId, WeakId},
    ip::types::RawMetric,
    sync::{StrongRc, WeakRc},
    NonSyncContext, SyncCtx,
};

/// The MAC address corresponding to the loopback interface.
const LOOPBACK_MAC: Mac = Mac::UNSPECIFIED;

/// A weak device ID identifying a loopback device.
///
/// This device ID is like [`WeakDeviceId`] but specifically for loopback
/// devices.
///
/// [`WeakDeviceId`]: crate::device::WeakDeviceId
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub struct LoopbackWeakDeviceId<C: DeviceLayerTypes>(
    pub(super) WeakRc<IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>>,
);

impl<C: DeviceLayerTypes> PartialEq for LoopbackWeakDeviceId<C> {
    fn eq(&self, LoopbackWeakDeviceId(other): &LoopbackWeakDeviceId<C>) -> bool {
        let LoopbackWeakDeviceId(me) = self;
        WeakRc::ptr_eq(me, other)
    }
}

impl<C: DeviceLayerTypes> PartialEq<LoopbackDeviceId<C>> for LoopbackWeakDeviceId<C> {
    fn eq(&self, other: &LoopbackDeviceId<C>) -> bool {
        <LoopbackDeviceId<C> as PartialEq<LoopbackWeakDeviceId<C>>>::eq(other, self)
    }
}

impl<C: DeviceLayerTypes> Eq for LoopbackWeakDeviceId<C> {}

impl<C: DeviceLayerTypes> Debug for LoopbackWeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<C: DeviceLayerTypes> Display for LoopbackWeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Weak Loopback")
    }
}

impl<C: DeviceLayerTypes> WeakId for LoopbackWeakDeviceId<C>
where
    C::LoopbackDeviceState: Send + Sync,
{
    type Strong = LoopbackDeviceId<C>;
}

impl<C: DeviceLayerTypes> Id for LoopbackWeakDeviceId<C>
where
    C::LoopbackDeviceState: Send + Sync,
{
    fn is_loopback(&self) -> bool {
        true
    }
}

impl<C: DeviceLayerTypes> LoopbackWeakDeviceId<C> {
    /// Attempts to upgrade the ID to a [`LoopbackDeviceId`], failing if the
    /// device no longer exists.
    pub fn upgrade(&self) -> Option<LoopbackDeviceId<C>> {
        let Self(rc) = self;
        rc.upgrade().map(LoopbackDeviceId)
    }
}

/// A strong device ID identifying a loopback device.
///
/// This device ID is like [`DeviceId`] but specifically for loopback devices.
///
/// [`DeviceId`]: crate::device::DeviceId
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub struct LoopbackDeviceId<C: DeviceLayerTypes>(
    pub(super) StrongRc<IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>>,
);

impl<C: DeviceLayerTypes> PartialEq for LoopbackDeviceId<C> {
    fn eq(&self, LoopbackDeviceId(other): &LoopbackDeviceId<C>) -> bool {
        let LoopbackDeviceId(me) = self;
        StrongRc::ptr_eq(me, other)
    }
}

impl<C: DeviceLayerTypes> PartialEq<LoopbackWeakDeviceId<C>> for LoopbackDeviceId<C> {
    fn eq(&self, LoopbackWeakDeviceId(other): &LoopbackWeakDeviceId<C>) -> bool {
        let LoopbackDeviceId(me) = self;
        StrongRc::weak_ptr_eq(me, other)
    }
}

impl<C: DeviceLayerTypes> Eq for LoopbackDeviceId<C> {}

impl<C: DeviceLayerTypes> Debug for LoopbackDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<C: DeviceLayerTypes> Display for LoopbackDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Loopback")
    }
}

impl<C: DeviceLayerTypes> StrongId for LoopbackDeviceId<C>
where
    C::LoopbackDeviceState: Send + Sync,
{
    type Weak = LoopbackWeakDeviceId<C>;
}

impl<C: DeviceLayerTypes> Id for LoopbackDeviceId<C>
where
    C::LoopbackDeviceState: Send + Sync,
{
    fn is_loopback(&self) -> bool {
        true
    }
}

impl<C: DeviceLayerTypes> LoopbackDeviceId<C> {
    /// Returns a reference to the external state for the device.
    pub fn external_state(&self) -> &C::LoopbackDeviceState {
        let Self(rc) = self;
        &rc.external_state
    }

    /// Returns a weak loopback device ID.
    pub fn downgrade(&self) -> LoopbackWeakDeviceId<C> {
        let Self(rc) = self;
        LoopbackWeakDeviceId(StrongRc::downgrade(rc))
    }

    pub(super) fn removed(&self) -> bool {
        let Self(rc) = self;
        StrongRc::marked_for_destruction(rc)
    }
}

#[derive(Copy, Clone)]
pub(super) enum LoopbackDevice {}

impl Device for LoopbackDevice {}

impl<NonSyncCtx: NonSyncContext, L> DeviceIdContext<LoopbackDevice>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type DeviceId = LoopbackDeviceId<NonSyncCtx>;
    type WeakDeviceId = LoopbackWeakDeviceId<NonSyncCtx>;
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        device_id.downgrade()
    }
    fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool {
        !device_id.removed()
    }
    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        weak_device_id.upgrade()
    }
}

pub(super) struct LoopbackDeviceState {
    mtu: Mtu,
    /// The routing metric of the loopback device this state is for.
    metric: RawMetric,
    rx_queue: ReceiveQueue<(), Buf<Vec<u8>>>,
    tx_queue: TransmitQueue<(), Buf<Vec<u8>>, BufVecU8Allocator>,
}

impl LoopbackDeviceState {
    pub(super) fn new(mtu: Mtu, metric: RawMetric) -> LoopbackDeviceState {
        LoopbackDeviceState {
            mtu,
            metric,
            rx_queue: Default::default(),
            tx_queue: Default::default(),
        }
    }
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::LoopbackRxQueue>
    for IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>
{
    type Data<'l> = crate::sync::LockGuard<'l, ReceiveQueueState<(), Buf<Vec<u8>>>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Data<'_> {
        self.link.rx_queue.queue.lock()
    }
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::LoopbackRxDequeue>
    for IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>
{
    type Data<'l> = crate::sync::LockGuard<'l, DequeueState<(), Buf<Vec<u8>>>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Data<'_> {
        self.link.rx_queue.deque.lock()
    }
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::LoopbackTxQueue>
    for IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>
{
    type Data<'l> = crate::sync::LockGuard<'l, TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Data<'_> {
        self.link.tx_queue.queue.lock()
    }
}

impl<C: NonSyncContext> LockFor<crate::lock_ordering::LoopbackTxDequeue>
    for IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>
{
    type Data<'l> = crate::sync::LockGuard<'l, DequeueState<(), Buf<Vec<u8>>>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Data<'_> {
        self.link.tx_queue.deque.lock()
    }
}

impl<C: NonSyncContext> RwLockFor<crate::lock_ordering::DeviceSockets>
    for IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>
{
    type ReadData<'l> = crate::sync::RwLockReadGuard<'l, HeldDeviceSockets<C>>
        where
            Self: 'l ;
    type WriteData<'l> = crate::sync::RwLockWriteGuard<'l, HeldDeviceSockets<C>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadData<'_> {
        self.sockets.read()
    }
    fn write_lock(&self) -> Self::WriteData<'_> {
        self.sockets.write()
    }
}

impl<C: NonSyncContext, B: BufferMut, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    SendFrameContext<C, B, DeviceSocketMetadata<LoopbackDeviceId<C>>> for Locked<&SyncCtx<C>, L>
{
    fn send_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        metadata: DeviceSocketMetadata<LoopbackDeviceId<C>>,
        body: S,
    ) -> Result<(), S> {
        let DeviceSocketMetadata { device_id, header } = metadata;
        match header {
            Some(DatagramHeader { dest_addr, protocol }) => {
                send_as_ethernet_frame_to_dst(self, ctx, &device_id, body, protocol, dest_addr)
            }
            None => send_ethernet_frame(self, ctx, &device_id, body),
        }
    }
}

pub(super) fn send_ip_frame<
    B: BufferMut,
    NonSyncCtx: NonSyncContext,
    A: IpAddress,
    S: Serializer<Buffer = B>,
    L: LockBefore<crate::lock_ordering::LoopbackTxQueue>,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device_id: &LoopbackDeviceId<NonSyncCtx>,
    _local_addr: SpecifiedAddr<A>,
    packet: S,
) -> Result<(), S>
where
    A::Version: EthernetIpExt,
{
    send_as_ethernet_frame_to_dst(
        sync_ctx,
        ctx,
        device_id,
        packet,
        <A::Version as EthernetIpExt>::ETHER_TYPE,
        LOOPBACK_MAC,
    )
}

fn send_as_ethernet_frame_to_dst<
    B: BufferMut,
    NonSyncCtx: NonSyncContext,
    S: Serializer<Buffer = B>,
    L: LockBefore<crate::lock_ordering::LoopbackTxQueue>,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device_id: &LoopbackDeviceId<NonSyncCtx>,
    packet: S,
    protocol: EtherType,
    dst_mac: Mac,
) -> Result<(), S> {
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

    send_ethernet_frame(sync_ctx, ctx, device_id, frame).map_err(|s| s.into_inner())
}

fn send_ethernet_frame<
    L: LockBefore<crate::lock_ordering::LoopbackTxQueue>,
    S: Serializer<Buffer = B>,
    B: BufferMut,
    NonSyncCtx: NonSyncContext,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device_id: &LoopbackDeviceId<NonSyncCtx>,
    frame: S,
) -> Result<(), S> {
    match BufferTransmitQueueHandler::<LoopbackDevice, _, _>::queue_tx_frame(
        sync_ctx,
        ctx,
        device_id,
        (),
        frame,
    ) {
        Ok(()) => Ok(()),
        Err(TransmitQueueFrameError::NoQueue(_)) => {
            unreachable!("loopback never fails to send a frame")
        }
        Err(TransmitQueueFrameError::QueueFull(s) | TransmitQueueFrameError::SerializeError(s)) => {
            Err(s)
        }
    }
}

/// Get the routing metric associated with this device.
pub(super) fn get_routing_metric<NonSyncCtx: NonSyncContext, L>(
    ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device_id: &LoopbackDeviceId<NonSyncCtx>,
) -> RawMetric {
    with_loopback_state(ctx, device_id, |mut state| state.cast_with(|s| &s.link.metric).copied())
}

/// Gets the MTU associated with this device.
pub(super) fn get_mtu<NonSyncCtx: NonSyncContext, L>(
    ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device_id: &LoopbackDeviceId<NonSyncCtx>,
) -> Mtu {
    with_loopback_state(ctx, device_id, |mut state| state.cast_with(|s| &s.link.mtu).copied())
}

impl<C: NonSyncContext> ReceiveQueueNonSyncContext<LoopbackDevice, LoopbackDeviceId<C>> for C {
    fn wake_rx_task(&mut self, device_id: &LoopbackDeviceId<C>) {
        DeviceLayerEventDispatcher::wake_rx_task(self, device_id)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueTypes<LoopbackDevice, C> for Locked<&SyncCtx<C>, L>
{
    type Meta = ();
    type Buffer = Buf<Vec<u8>>;
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueContext<LoopbackDevice, C> for Locked<&SyncCtx<C>, L>
{
    fn with_receive_queue_mut<
        O,
        F: FnOnce(&mut ReceiveQueueState<Self::Meta, Self::Buffer>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<C>,
        cb: F,
    ) -> O {
        with_loopback_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackRxQueue>();
            cb(&mut x)
        })
    }
}

impl<C: NonSyncContext> ReceiveDequeFrameContext<LoopbackDevice, C>
    for Locked<&SyncCtx<C>, crate::lock_ordering::LoopbackRxDequeue>
{
    fn handle_frame(
        &mut self,
        ctx: &mut C,
        device_id: &LoopbackDeviceId<C>,
        (): Self::Meta,
        mut buf: Buf<Vec<u8>>,
    ) {
        let (frame, whole_body) =
            match buf.parse_with_view::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::NoCheck) {
                Err(e) => {
                    trace!("dropping invalid ethernet frame over loopback: {:?}", e);
                    return;
                }
                Ok(e) => e,
            };

        let frame_dest = FrameDestination::from_dest(frame.dst_mac(), Mac::UNSPECIFIED);
        let ethertype = frame.ethertype();

        BufferSocketHandler::<LoopbackDevice, _>::handle_frame(
            self,
            ctx,
            device_id,
            ReceivedFrame::from_ethernet(frame, frame_dest).into(),
            whole_body,
        );

        let ethertype = match ethertype {
            Some(e) => e,
            None => {
                trace!("dropping ethernet frame without ethertype");
                return;
            }
        };

        match ethertype {
            EtherType::Ipv4 => crate::ip::receive_ipv4_packet(
                self,
                ctx,
                &device_id.clone().into(),
                frame_dest,
                buf,
            ),
            EtherType::Ipv6 => crate::ip::receive_ipv6_packet(
                self,
                ctx,
                &device_id.clone().into(),
                frame_dest,
                buf,
            ),
            ethertype @ EtherType::Arp | ethertype @ EtherType::Other(_) => {
                trace!("not handling loopback frame of type {:?}", ethertype)
            }
        }
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::LoopbackRxDequeue>>
    ReceiveDequeContext<LoopbackDevice, C> for Locked<&SyncCtx<C>, L>
{
    type ReceiveQueueCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::LoopbackRxDequeue>;

    fn with_dequed_frames_and_rx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<(), Buf<Vec<u8>>>, &mut Self::ReceiveQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<C>,
        cb: F,
    ) -> O {
        with_loopback_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackRxDequeue>();
            let mut locked = sync_ctx.cast_locked();
            cb(&mut x, &mut locked)
        })
    }
}

impl<C: NonSyncContext> TransmitQueueNonSyncContext<LoopbackDevice, LoopbackDeviceId<C>> for C {
    fn wake_tx_task(&mut self, device_id: &LoopbackDeviceId<C>) {
        DeviceLayerEventDispatcher::wake_tx_task(self, &device_id.clone().into())
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    TransmitQueueCommon<LoopbackDevice, C> for Locked<&SyncCtx<C>, L>
{
    type Meta = ();
    type Allocator = BufVecU8Allocator;
    type Buffer = Buf<Vec<u8>>;

    fn parse_outgoing_frame(buf: &[u8]) -> Result<SentFrame<&[u8]>, ParseSentFrameError> {
        SentFrame::try_parse_as_ethernet(buf)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    TransmitQueueContext<LoopbackDevice, C> for Locked<&SyncCtx<C>, L>
{
    fn with_transmit_queue_mut<
        O,
        F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<C>,
        cb: F,
    ) -> O {
        with_loopback_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackTxQueue>();
            cb(&mut x)
        })
    }

    fn send_frame(
        &mut self,
        ctx: &mut C,
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
        match BufferReceiveQueueHandler::queue_rx_frame(self, ctx, device_id, meta, buf) {
            Ok(()) => {}
            Err(ReceiveQueueFullError(((), _frame))) => {
                // RX queue is full - there is nothing further we can do here.
                tracing::error!("dropped RX frame on loopback device due to full RX queue")
            }
        }

        Ok(())
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::LoopbackTxDequeue>>
    TransmitDequeueContext<LoopbackDevice, C> for Locked<&SyncCtx<C>, L>
{
    type TransmitQueueCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::LoopbackTxDequeue>;

    fn with_dequed_packets_and_tx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::TransmitQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_loopback_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackTxDequeue>();
            let mut locked = sync_ctx.cast_locked();
            cb(&mut x, &mut locked)
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use lock_order::{Locked, Unlocked};
    use net_types::ip::{AddrSubnet, Ip, Ipv4, Ipv6};
    use packet::ParseBuffer;

    use crate::{
        device::{DeviceId, Mtu},
        error::NotFoundError,
        ip::device::{IpAddressId as _, IpDeviceIpExt, IpDeviceStateContext},
        testutil::{
            Ctx, FakeEventDispatcherConfig, FakeNonSyncCtx, TestIpExt, DEFAULT_INTERFACE_METRIC,
        },
        SyncCtx,
    };

    use super::*;

    const MTU: Mtu = Mtu::new(66);

    #[test]
    fn loopback_mtu() {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let sync_ctx = &sync_ctx;
        let device = crate::device::add_loopback_device(&sync_ctx, MTU, DEFAULT_INTERFACE_METRIC)
            .expect("error adding loopback device")
            .into();
        crate::device::testutil::enable_device(&sync_ctx, &mut non_sync_ctx, &device);

        assert_eq!(
            crate::ip::IpDeviceContext::<Ipv4, _>::get_mtu(&mut Locked::new(sync_ctx), &device),
            MTU
        );
        assert_eq!(
            crate::ip::IpDeviceContext::<Ipv6, _>::get_mtu(&mut Locked::new(sync_ctx), &device),
            MTU
        );
    }

    #[ip_test]
    fn test_loopback_add_remove_addrs<I: Ip + TestIpExt + IpDeviceIpExt>()
    where
        for<'a> Locked<&'a SyncCtx<FakeNonSyncCtx>, Unlocked>:
            IpDeviceStateContext<I, FakeNonSyncCtx, DeviceId = DeviceId<FakeNonSyncCtx>>,
    {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let sync_ctx = &sync_ctx;
        let device = crate::device::add_loopback_device(&sync_ctx, MTU, DEFAULT_INTERFACE_METRIC)
            .expect("error adding loopback device")
            .into();
        crate::device::testutil::enable_device(&sync_ctx, &mut non_sync_ctx, &device);

        let get_addrs = || {
            crate::ip::device::IpDeviceStateContext::<I, _>::with_address_ids(
                &mut Locked::new(sync_ctx),
                &device,
                |addrs| addrs.map(|a| a.addr()).collect::<Vec<_>>(),
            )
        };

        let FakeEventDispatcherConfig {
            subnet,
            local_ip,
            local_mac: _,
            remote_ip: _,
            remote_mac: _,
        } = I::FAKE_CONFIG;
        let addr =
            AddrSubnet::from_witness(local_ip, subnet.prefix()).expect("error creating AddrSubnet");

        assert_eq!(get_addrs(), []);

        assert_eq!(
            crate::device::add_ip_addr_subnet(sync_ctx, &mut non_sync_ctx, &device, addr),
            Ok(())
        );
        let addr = addr.addr();
        assert_eq!(&get_addrs()[..], [addr]);

        assert_eq!(crate::device::del_ip_addr(sync_ctx, &mut non_sync_ctx, &device, &addr), Ok(()));
        assert_eq!(get_addrs(), []);

        assert_eq!(
            crate::device::del_ip_addr(sync_ctx, &mut non_sync_ctx, &device, &addr),
            Err(NotFoundError)
        );
    }

    #[ip_test]
    fn loopback_sends_ethernet<I: Ip + TestIpExt>() {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let sync_ctx = &sync_ctx;
        let device = crate::device::add_loopback_device(&sync_ctx, MTU, DEFAULT_INTERFACE_METRIC)
            .expect("error adding loopback device");
        crate::device::testutil::enable_device(
            &sync_ctx,
            &mut non_sync_ctx,
            &device.clone().into(),
        );

        let local_addr = I::FAKE_CONFIG.local_ip;
        const BODY: &[u8] = b"IP body".as_slice();

        let body = Buf::new(Vec::from(BODY), ..);
        send_ip_frame(&mut Locked::new(sync_ctx), &mut non_sync_ctx, &device, local_addr, body)
            .expect("can send");

        // There is no transmit queue so the frames will immediately go into the
        // receive queue.
        let mut frames = ReceiveQueueContext::<LoopbackDevice, _>::with_receive_queue_mut(
            &mut Locked::new(sync_ctx),
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
    }
}
