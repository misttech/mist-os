// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The loopback device.

use alloc::vec::Vec;
use core::{convert::Infallible as Never, fmt::Debug};

use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::{
    ethernet::Mac,
    ip::{Ip, IpAddress, Ipv4, Ipv6, Mtu},
    SpecifiedAddr,
};
use netstack3_base::{
    sync::Mutex, CoreTimerContext, Device, DeviceIdContext, FrameDestination, RecvFrameContext,
    RecvIpFrameMeta, ResourceCounterContext, SendableFrameMeta, TimerContext, WeakDeviceIdentifier,
};
use packet::{Buf, Buffer as _, BufferMut, Serializer};
use packet_formats::ethernet::{
    EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck, EthernetIpExt,
};
use tracing::trace;

use crate::internal::{
    base::{DeviceCounters, DeviceLayerTypes, DeviceReceiveFrameSpec, EthernetDeviceCounters},
    id::{BaseDeviceId, BasePrimaryDeviceId, BaseWeakDeviceId},
    queue::{
        rx::{ReceiveDequeFrameContext, ReceiveQueue, ReceiveQueueState, ReceiveQueueTypes},
        tx::{BufVecU8Allocator, TransmitQueue, TransmitQueueHandler, TransmitQueueState},
        DequeueState, TransmitQueueFrameError,
    },
    socket::{
        DeviceSocketHandler, DeviceSocketMetadata, DeviceSocketSendTypes, EthernetHeaderParams,
        ReceivedFrame,
    },
    state::{DeviceStateSpec, IpLinkDeviceState},
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
pub type LoopbackPrimaryDeviceId<BT> = BasePrimaryDeviceId<LoopbackDevice, BT>;

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
    /// Loopback device counters.
    pub counters: EthernetDeviceCounters,
    /// The MTU this device was created with (immutable).
    pub mtu: Mtu,
    /// Loopback device receive queue.
    pub rx_queue: ReceiveQueue<LoopbackRxQueueMeta, Buf<Vec<u8>>>,
    /// Loopback device transmit queue.
    pub tx_queue: TransmitQueue<LoopbackTxQueueMeta, Buf<Vec<u8>>, BufVecU8Allocator>,
}

/// Metadata associated with a frame in the Loopback TX queue.
pub struct LoopbackTxQueueMeta;
/// Metadata associated with a frame in the Loopback RX queue.
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

impl DeviceSocketSendTypes for LoopbackDevice {
    /// When `None`, data will be sent as a raw Ethernet frame without any
    /// system-applied headers.
    type Metadata = Option<EthernetHeaderParams>;
}

impl<CC, BC> ReceiveDequeFrameContext<LoopbackDevice, BC> for CC
where
    CC: DeviceIdContext<LoopbackDevice>
        + RecvFrameContext<RecvIpFrameMeta<CC::DeviceId, Ipv4>, BC>
        + RecvFrameContext<RecvIpFrameMeta<CC::DeviceId, Ipv6>, BC>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>
        + ResourceCounterContext<CC::DeviceId, EthernetDeviceCounters>
        + DeviceSocketHandler<LoopbackDevice, BC>
        + ReceiveQueueTypes<LoopbackDevice, BC, Meta = LoopbackRxQueueMeta>,
    CC::Buffer: BufferMut + Debug,
{
    fn handle_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        LoopbackRxQueueMeta: Self::Meta,
        mut buf: Self::Buffer,
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
                self.receive_frame(
                    bindings_ctx,
                    RecvIpFrameMeta::<_, Ipv4>::new(device_id.clone(), Some(frame_dest)),
                    buf,
                );
            }
            EtherType::Ipv6 => {
                self.increment(device_id, |counters: &DeviceCounters| {
                    &counters.recv_ipv6_delivered
                });
                self.receive_frame(
                    bindings_ctx,
                    RecvIpFrameMeta::<_, Ipv6>::new(device_id.clone(), Some(frame_dest)),
                    buf,
                );
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

/// Sends an IP frame `packet` over `device_id`.
pub fn send_ip_frame<CC, BC, A, S>(
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