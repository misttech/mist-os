// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The loopback device.

use alloc::vec::Vec;

use lock_order::{
    lock::{LockFor, RwLockFor},
    relation::LockBefore,
    Locked,
};
use net_types::{
    ethernet::Mac,
    ip::{IpAddress, Mtu},
    SpecifiedAddr,
};
use packet::{Buf, Buffer as _, BufferMut, Serializer};
use packet_formats::ethernet::{
    EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck, EthernetIpExt,
};
use tracing::trace;

use crate::{
    context::{CounterContext, SendFrameContext},
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
            DatagramHeader, DeviceSocketHandler, DeviceSocketMetadata, HeldDeviceSockets,
            ParseSentFrameError, ReceivedFrame, SentFrame,
        },
        state::{DeviceStateSpec, IpLinkDeviceState},
        Device, DeviceCounters, DeviceIdContext, DeviceLayerEventDispatcher, DeviceLayerTypes,
        DeviceSendFrameError, FrameDestination,
    },
    ip::types::RawMetric,
    BindingsContext, SyncCtx,
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
    const IS_LOOPBACK: bool = true;
    const DEBUG_TYPE: &'static str = "Loopback";
}

impl<BC: BindingsContext, L> DeviceIdContext<LoopbackDevice> for Locked<&SyncCtx<BC>, L> {
    type DeviceId = LoopbackDeviceId<BC>;
    type WeakDeviceId = LoopbackWeakDeviceId<BC>;
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        device_id.downgrade()
    }
    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        weak_device_id.upgrade()
    }
}

/// State for a loopback device.
pub struct LoopbackDeviceState {
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

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    SendFrameContext<BC, DeviceSocketMetadata<LoopbackDeviceId<BC>>> for Locked<&SyncCtx<BC>, L>
{
    fn send_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: DeviceSocketMetadata<LoopbackDeviceId<BC>>,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        let DeviceSocketMetadata { device_id, header } = metadata;
        match header {
            Some(DatagramHeader { dest_addr, protocol }) => send_as_ethernet_frame_to_dst(
                self,
                bindings_ctx,
                &device_id,
                body,
                protocol,
                dest_addr,
            ),
            None => send_ethernet_frame(self, bindings_ctx, &device_id, body),
        }
    }
}

pub(super) fn send_ip_frame<BC, A, S, L>(
    core_ctx: &mut Locked<&SyncCtx<BC>, L>,
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
    core_ctx: &mut Locked<&SyncCtx<BC>, L>,
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
    core_ctx: &mut Locked<&SyncCtx<BC>, L>,
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
    core_ctx.with_counters(|counters: &DeviceCounters| {
        counters.loopback.common.send_total_frames.increment();
    });
    match TransmitQueueHandler::<LoopbackDevice, _>::queue_tx_frame(
        core_ctx,
        bindings_ctx,
        device_id,
        (),
        frame,
    ) {
        Ok(()) => {
            core_ctx.with_counters(|counters: &DeviceCounters| {
                counters.loopback.common.send_frame.increment();
            });
            Ok(())
        }
        Err(TransmitQueueFrameError::NoQueue(_)) => {
            unreachable!("loopback never fails to send a frame")
        }
        Err(TransmitQueueFrameError::QueueFull(s)) => {
            core_ctx.with_counters(|counters: &DeviceCounters| {
                counters.loopback.common.send_queue_full.increment();
            });
            Err(s)
        }
        Err(TransmitQueueFrameError::SerializeError(s)) => {
            core_ctx.with_counters(|counters: &DeviceCounters| {
                counters.loopback.common.send_serialize_error.increment();
            });
            Err(s)
        }
    }
}

/// Get the routing metric associated with this device.
pub(super) fn get_routing_metric<BC: BindingsContext, L>(
    core_ctx: &mut Locked<&SyncCtx<BC>, L>,
    device_id: &LoopbackDeviceId<BC>,
) -> RawMetric {
    device::integration::with_loopback_state(core_ctx, device_id, |mut state| {
        state.cast_with(|s| &s.link.metric).copied()
    })
}

/// Gets the MTU associated with this device.
pub(super) fn get_mtu<BC: BindingsContext, L>(
    core_ctx: &mut Locked<&SyncCtx<BC>, L>,
    device_id: &LoopbackDeviceId<BC>,
) -> Mtu {
    device::integration::with_loopback_state(core_ctx, device_id, |mut state| {
        state.cast_with(|s| &s.link.mtu).copied()
    })
}

impl<BC: BindingsContext> ReceiveQueueBindingsContext<LoopbackDevice, LoopbackDeviceId<BC>> for BC {
    fn wake_rx_task(&mut self, device_id: &LoopbackDeviceId<BC>) {
        DeviceLayerEventDispatcher::wake_rx_task(self, device_id)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueTypes<LoopbackDevice, BC> for Locked<&SyncCtx<BC>, L>
{
    type Meta = ();
    type Buffer = Buf<Vec<u8>>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueContext<LoopbackDevice, BC> for Locked<&SyncCtx<BC>, L>
{
    fn with_receive_queue_mut<
        O,
        F: FnOnce(&mut ReceiveQueueState<Self::Meta, Self::Buffer>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_loopback_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackRxQueue>();
            cb(&mut x)
        })
    }
}

impl<BC: BindingsContext> ReceiveDequeFrameContext<LoopbackDevice, BC>
    for Locked<&SyncCtx<BC>, crate::lock_ordering::LoopbackRxDequeue>
{
    fn handle_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &LoopbackDeviceId<BC>,
        (): Self::Meta,
        mut buf: Buf<Vec<u8>>,
    ) {
        self.with_counters(|counters: &DeviceCounters| {
            counters.loopback.common.recv_frame.increment();
        });
        let (frame, whole_body) =
            match buf.parse_with_view::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::NoCheck) {
                Err(e) => {
                    self.with_counters(|counters: &DeviceCounters| {
                        counters.loopback.common.recv_parse_error.increment();
                    });
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
                self.with_counters(|counters: &DeviceCounters| {
                    counters.loopback.recv_no_ethertype.increment();
                });
                trace!("dropping ethernet frame without ethertype");
                return;
            }
        };

        match ethertype {
            EtherType::Ipv4 => {
                self.with_counters(|counters: &DeviceCounters| {
                    counters.loopback.common.recv_ip_delivered.increment();
                });
                crate::ip::receive_ipv4_packet(
                    self,
                    bindings_ctx,
                    &device_id.clone().into(),
                    frame_dest,
                    buf,
                )
            }
            EtherType::Ipv6 => {
                self.with_counters(|counters: &DeviceCounters| {
                    counters.loopback.common.recv_ip_delivered.increment();
                });
                crate::ip::receive_ipv6_packet(
                    self,
                    bindings_ctx,
                    &device_id.clone().into(),
                    frame_dest,
                    buf,
                )
            }
            ethertype @ EtherType::Arp | ethertype @ EtherType::Other(_) => {
                self.with_counters(|counters: &DeviceCounters| {
                    counters.loopback.common.recv_unsupported_ethertype.increment();
                });
                trace!("not handling loopback frame of type {:?}", ethertype)
            }
        }
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxDequeue>>
    ReceiveDequeContext<LoopbackDevice, BC> for Locked<&SyncCtx<BC>, L>
{
    type ReceiveQueueCtx<'a> = Locked<&'a SyncCtx<BC>, crate::lock_ordering::LoopbackRxDequeue>;

    fn with_dequed_frames_and_rx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<(), Buf<Vec<u8>>>, &mut Self::ReceiveQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_loopback_state_and_core_ctx(
            self,
            device_id,
            |mut state, core_ctx| {
                let mut x = state.lock::<crate::lock_ordering::LoopbackRxDequeue>();
                let mut locked = core_ctx.cast_locked();
                cb(&mut x, &mut locked)
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
    TransmitQueueCommon<LoopbackDevice, BC> for Locked<&SyncCtx<BC>, L>
{
    type Meta = ();
    type Allocator = BufVecU8Allocator;
    type Buffer = Buf<Vec<u8>>;

    fn parse_outgoing_frame(buf: &[u8]) -> Result<SentFrame<&[u8]>, ParseSentFrameError> {
        SentFrame::try_parse_as_ethernet(buf)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    TransmitQueueContext<LoopbackDevice, BC> for Locked<&SyncCtx<BC>, L>
{
    fn with_transmit_queue_mut<
        O,
        F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_loopback_state(self, device_id, |mut state| {
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
    TransmitDequeueContext<LoopbackDevice, BC> for Locked<&SyncCtx<BC>, L>
{
    type TransmitQueueCtx<'a> = Locked<&'a SyncCtx<BC>, crate::lock_ordering::LoopbackTxDequeue>;

    fn with_dequed_packets_and_tx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::TransmitQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        device::integration::with_loopback_state_and_core_ctx(
            self,
            device_id,
            |mut state, core_ctx| {
                let mut x = state.lock::<crate::lock_ordering::LoopbackTxDequeue>();
                let mut locked = core_ctx.cast_locked();
                cb(&mut x, &mut locked)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use lock_order::{Locked, Unlocked};
    use net_types::ip::{AddrSubnet, AddrSubnetEither, Ip, Ipv4, Ipv6, Mtu};
    use packet::ParseBuffer;

    use crate::{
        device::DeviceId,
        error::NotFoundError,
        ip::device::{IpAddressId as _, IpDeviceIpExt, IpDeviceStateContext},
        testutil::{
            Ctx, FakeBindingsCtx, FakeEventDispatcherConfig, TestIpExt, DEFAULT_INTERFACE_METRIC,
        },
        SyncCtx,
    };

    use super::*;

    const MTU: Mtu = Mtu::new(66);

    #[test]
    fn loopback_mtu() {
        let Ctx { core_ctx, mut bindings_ctx } = crate::testutil::FakeCtx::default();
        let core_ctx = &core_ctx;
        let device = crate::device::add_loopback_device(&core_ctx, MTU, DEFAULT_INTERFACE_METRIC)
            .expect("error adding loopback device")
            .into();
        crate::device::testutil::enable_device(&core_ctx, &mut bindings_ctx, &device);

        assert_eq!(
            crate::ip::IpDeviceContext::<Ipv4, _>::get_mtu(&mut Locked::new(core_ctx), &device),
            MTU
        );
        assert_eq!(
            crate::ip::IpDeviceContext::<Ipv6, _>::get_mtu(&mut Locked::new(core_ctx), &device),
            MTU
        );
    }

    #[ip_test]
    fn test_loopback_add_remove_addrs<I: Ip + TestIpExt + IpDeviceIpExt>()
    where
        for<'a> Locked<&'a SyncCtx<FakeBindingsCtx>, Unlocked>:
            IpDeviceStateContext<I, FakeBindingsCtx, DeviceId = DeviceId<FakeBindingsCtx>>,
    {
        let Ctx { core_ctx, mut bindings_ctx } = crate::testutil::FakeCtx::default();
        let core_ctx = &core_ctx;
        let device = crate::device::add_loopback_device(&core_ctx, MTU, DEFAULT_INTERFACE_METRIC)
            .expect("error adding loopback device")
            .into();
        crate::device::testutil::enable_device(&core_ctx, &mut bindings_ctx, &device);

        let get_addrs = || {
            crate::ip::device::IpDeviceStateContext::<I, _>::with_address_ids(
                &mut Locked::new(core_ctx),
                &device,
                |addrs, _core_ctx| addrs.map(|a| a.addr()).collect::<Vec<_>>(),
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
            crate::device::add_ip_addr_subnet(
                core_ctx,
                &mut bindings_ctx,
                &device,
                AddrSubnetEither::from(addr)
            ),
            Ok(())
        );
        let addr = addr.addr();
        assert_eq!(&get_addrs()[..], [addr]);

        assert_eq!(crate::device::del_ip_addr(core_ctx, &mut bindings_ctx, &device, addr), Ok(()));
        assert_eq!(get_addrs(), []);

        assert_eq!(
            crate::device::del_ip_addr(core_ctx, &mut bindings_ctx, &device, addr),
            Err(NotFoundError)
        );
    }

    #[ip_test]
    fn loopback_sends_ethernet<I: Ip + TestIpExt>() {
        let Ctx { core_ctx, mut bindings_ctx } = crate::testutil::FakeCtx::default();
        let core_ctx = &core_ctx;
        let device = crate::device::add_loopback_device(&core_ctx, MTU, DEFAULT_INTERFACE_METRIC)
            .expect("error adding loopback device");
        crate::device::testutil::enable_device(
            &core_ctx,
            &mut bindings_ctx,
            &device.clone().into(),
        );

        let local_addr = I::FAKE_CONFIG.local_ip;
        const BODY: &[u8] = b"IP body".as_slice();

        let body = Buf::new(Vec::from(BODY), ..);
        send_ip_frame(&mut Locked::new(core_ctx), &mut bindings_ctx, &device, local_addr, body)
            .expect("can send");

        // There is no transmit queue so the frames will immediately go into the
        // receive queue.
        let mut frames = ReceiveQueueContext::<LoopbackDevice, _>::with_receive_queue_mut(
            &mut Locked::new(core_ctx),
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
