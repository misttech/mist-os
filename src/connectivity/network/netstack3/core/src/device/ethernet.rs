// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Ethernet protocol.

use alloc::vec::Vec;
use core::{fmt::Debug, num::NonZeroU32};
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};

use const_unwrap::const_unwrap_option;
use net_types::{
    ethernet::Mac,
    ip::{GenericOverIp, Ip, IpAddress, IpMarked, Ipv4, Ipv6, Mtu},
    BroadcastAddress, MulticastAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{Buf, BufferMut, Nested, Serializer};
use packet_formats::{
    arp::{peek_arp_types, ArpHardwareType, ArpNetworkType},
    ethernet::{
        EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck, EthernetIpExt,
        ETHERNET_HDR_LEN_NO_TAG,
    },
};
use tracing::trace;

use crate::{
    context::{
        CoreTimerContext, HandleableTimer, NestedIntoCoreTimerCtx, ReceivableFrameMeta,
        RecvFrameContext, ResourceCounterContext, RngContext, SendableFrameMeta, TimerContext,
        TimerHandler,
    },
    data_structures::ref_counted_hash_map::{InsertResult, RefCountedHashSet, RemoveResult},
    device::{
        arp::{ArpFrameMetadata, ArpPacketHandler, ArpState, ArpTimerId},
        link::LinkDevice,
        queue::{
            tx::{BufVecU8Allocator, TransmitQueue, TransmitQueueHandler, TransmitQueueState},
            DequeueState, TransmitQueueFrameError,
        },
        socket::{
            DeviceSocketBindingsContext, DeviceSocketHandler, DeviceSocketMetadata,
            DeviceSocketSendTypes, EthernetHeaderParams, ReceivedFrame,
        },
        state::{DeviceStateSpec, IpLinkDeviceState},
        Device, DeviceCounters, DeviceIdContext, DeviceLayerTypes, DeviceReceiveFrameSpec,
        EthernetDeviceCounters, EthernetDeviceId, FrameDestination, RecvIpFrameMeta,
        WeakDeviceIdentifier,
    },
    ip::{
        device::nud::{
            LinkResolutionContext, NudBindingsTypes, NudHandler, NudState, NudTimerId,
            NudUserConfig,
        },
        types::IpTypesIpExt,
    },
    routes::WrapBroadcastMarker,
    sync::{Mutex, RwLock},
    trace_duration, TracingContext,
};

pub(super) mod integration;

const ETHERNET_HDR_LEN_NO_TAG_U32: u32 = ETHERNET_HDR_LEN_NO_TAG as u32;

/// The execution context for an Ethernet device provided by bindings.
pub(crate) trait EthernetIpLinkDeviceBindingsContext:
    RngContext + TimerContext + DeviceLayerTypes
{
}
impl<BC: RngContext + TimerContext + DeviceLayerTypes> EthernetIpLinkDeviceBindingsContext for BC {}

/// Provides access to an ethernet device's static state.
pub(crate) trait EthernetIpLinkDeviceStaticStateContext:
    DeviceIdContext<EthernetLinkDevice>
{
    /// Calls the function with an immutable reference to the ethernet device's
    /// static state.
    fn with_static_ethernet_device_state<O, F: FnOnce(&StaticEthernetDeviceState) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// Provides access to an ethernet device's dynamic state.
pub(crate) trait EthernetIpLinkDeviceDynamicStateContext<BC: EthernetIpLinkDeviceBindingsContext>:
    EthernetIpLinkDeviceStaticStateContext
{
    /// Calls the function with the ethernet device's static state and immutable
    /// reference to the dynamic state.
    fn with_ethernet_state<
        O,
        F: FnOnce(&StaticEthernetDeviceState, &DynamicEthernetDeviceState) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with the ethernet device's static state and mutable
    /// reference to the dynamic state.
    fn with_ethernet_state_mut<
        O,
        F: FnOnce(&StaticEthernetDeviceState, &mut DynamicEthernetDeviceState) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

fn send_as_ethernet_frame_to_dst<S, BC, CC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    dst_mac: Mac,
    body: S,
    ether_type: EtherType,
) -> Result<(), S>
where
    S: Serializer,
    S::Buffer: BufferMut,
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>
        + TransmitQueueHandler<EthernetLinkDevice, BC, Meta = ()>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
{
    /// The minimum body length for the Ethernet frame.
    ///
    /// Using a frame length of 0 improves efficiency by avoiding unnecessary
    /// padding at this layer. The expectation is that the implementation of
    /// [`DeviceLayerEventDispatcher::send_frame`] will add any padding required
    /// by the implementation.
    const MIN_BODY_LEN: usize = 0;

    let local_mac = get_mac(core_ctx, device_id);
    let frame = body.encapsulate(EthernetFrameBuilder::new(
        local_mac.get(),
        dst_mac,
        ether_type,
        MIN_BODY_LEN,
    ));
    send_ethernet_frame(core_ctx, bindings_ctx, device_id, frame)
        .map_err(|frame| frame.into_inner())
}

fn send_ethernet_frame<S, BC, CC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    frame: S,
) -> Result<(), S>
where
    S: Serializer,
    S::Buffer: BufferMut,
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>
        + TransmitQueueHandler<EthernetLinkDevice, BC, Meta = ()>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
{
    core_ctx.increment(device_id, |counters| &counters.send_total_frames);
    match TransmitQueueHandler::<EthernetLinkDevice, _>::queue_tx_frame(
        core_ctx,
        bindings_ctx,
        device_id,
        (),
        frame,
    ) {
        Ok(()) => {
            core_ctx.increment(device_id, |counters| &counters.send_frame);
            Ok(())
        }
        Err(TransmitQueueFrameError::NoQueue(e)) => {
            core_ctx.increment(device_id, |counters| &counters.send_dropped_no_queue);
            tracing::error!("device {device_id:?} not ready to send frame: {e:?}");
            Ok(())
        }
        Err(TransmitQueueFrameError::QueueFull(s)) => {
            core_ctx.increment(device_id, |counters| &counters.send_queue_full);
            Err(s)
        }
        Err(TransmitQueueFrameError::SerializeError(s)) => {
            core_ctx.increment(device_id, |counters| &counters.send_serialize_error);
            Err(s)
        }
    }
}

/// The maximum frame size one ethernet device can send.
///
/// The frame size includes the ethernet header, the data payload, but excludes
/// the 4 bytes from FCS (frame check sequence) as we don't calculate CRC and it
/// is normally handled by the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaxEthernetFrameSize(NonZeroU32);

impl MaxEthernetFrameSize {
    /// The minimum ethernet frame size.
    ///
    /// We don't care about FCS, so the minimum frame size for us is 64 - 4.
    pub(crate) const MIN: MaxEthernetFrameSize =
        MaxEthernetFrameSize(const_unwrap_option(NonZeroU32::new(60)));

    /// Creates from the maximum size of ethernet header and ethernet payload,
    /// checks that it is valid, i.e., larger than the minimum frame size.
    pub const fn new(frame_size: u32) -> Option<Self> {
        if frame_size < Self::MIN.get().get() {
            return None;
        }
        Some(Self(const_unwrap_option(NonZeroU32::new(frame_size))))
    }

    const fn get(&self) -> NonZeroU32 {
        let Self(frame_size) = *self;
        frame_size
    }

    /// Converts the maximum frame size to its corresponding MTU.
    pub const fn as_mtu(&self) -> Mtu {
        // MTU must be positive because of the limit on minimum ethernet frame size
        Mtu::new(self.get().get().saturating_sub(ETHERNET_HDR_LEN_NO_TAG_U32))
    }

    /// Creates the maximum ethernet frame size from MTU.
    pub const fn from_mtu(mtu: Mtu) -> Option<MaxEthernetFrameSize> {
        let frame_size = mtu.get().saturating_add(ETHERNET_HDR_LEN_NO_TAG_U32);
        Self::new(frame_size)
    }
}

/// Base properties to create a new Ethernet device.
#[derive(Debug)]
pub struct EthernetCreationProperties {
    /// The device's MAC address.
    pub mac: UnicastAddr<Mac>,
    /// The maximum frame size this device supports.
    // TODO(https://fxbug.dev/42072516): Add a minimum frame size for all
    // Ethernet devices such that you can't create an `EthernetDeviceState`
    // with a `MaxEthernetFrameSize` smaller than the minimum. The absolute minimum
    // needs to be at least the minimum body size of an Ethernet frame. For
    // IPv6-capable devices, the minimum needs to be higher - the frame size
    // implied by the IPv6 minimum MTU. The easy path is to simply use that
    // frame size as the minimum in all cases, although we may at some point
    // want to figure out how to configure devices which don't support IPv6,
    // and allow smaller frame sizes for those devices.
    pub max_frame_size: MaxEthernetFrameSize,
}

pub(crate) struct DynamicEthernetDeviceState {
    /// The value this netstack assumes as the device's maximum frame size.
    max_frame_size: MaxEthernetFrameSize,

    /// A flag indicating whether the device will accept all ethernet frames
    /// that it receives, regardless of the ethernet frame's destination MAC
    /// address.
    promiscuous_mode: bool,

    /// Link multicast groups this device has joined.
    link_multicast_groups: RefCountedHashSet<MulticastAddr<Mac>>,
}

impl DynamicEthernetDeviceState {
    fn new(max_frame_size: MaxEthernetFrameSize) -> Self {
        Self { max_frame_size, promiscuous_mode: false, link_multicast_groups: Default::default() }
    }
}

pub(crate) struct StaticEthernetDeviceState {
    /// Mac address of the device this state is for.
    mac: UnicastAddr<Mac>,

    /// The maximum frame size allowed by the hardware.
    max_frame_size: MaxEthernetFrameSize,
}

/// The state associated with an Ethernet device.
pub struct EthernetDeviceState<BT: NudBindingsTypes<EthernetLinkDevice>> {
    /// Ethernet device counters.
    counters: EthernetDeviceCounters,

    /// IPv4 ARP state.
    ipv4_arp: Mutex<ArpState<EthernetLinkDevice, BT>>,

    /// IPv6 NUD state.
    ipv6_nud: Mutex<NudState<Ipv6, EthernetLinkDevice, BT>>,

    ipv4_nud_config: RwLock<IpMarked<Ipv4, NudUserConfig>>,

    ipv6_nud_config: RwLock<IpMarked<Ipv6, NudUserConfig>>,

    static_state: StaticEthernetDeviceState,

    dynamic_state: RwLock<DynamicEthernetDeviceState>,

    tx_queue: TransmitQueue<(), Buf<Vec<u8>>, BufVecU8Allocator>,
}

impl<BT: DeviceLayerTypes, I: Ip> OrderedLockAccess<IpMarked<I, NudUserConfig>>
    for IpLinkDeviceState<EthernetLinkDevice, BT>
{
    type Lock = RwLock<IpMarked<I, NudUserConfig>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(I::map_ip(
            (),
            |()| &self.link.ipv4_nud_config,
            |()| &self.link.ipv6_nud_config,
        ))
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<DynamicEthernetDeviceState>
    for IpLinkDeviceState<EthernetLinkDevice, BT>
{
    type Lock = RwLock<DynamicEthernetDeviceState>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.dynamic_state)
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<NudState<Ipv6, EthernetLinkDevice, BT>>
    for IpLinkDeviceState<EthernetLinkDevice, BT>
{
    type Lock = Mutex<NudState<Ipv6, EthernetLinkDevice, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.ipv6_nud)
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<ArpState<EthernetLinkDevice, BT>>
    for IpLinkDeviceState<EthernetLinkDevice, BT>
{
    type Lock = Mutex<ArpState<EthernetLinkDevice, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.ipv4_arp)
    }
}

impl<BT: DeviceLayerTypes>
    OrderedLockAccess<TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>>
    for IpLinkDeviceState<EthernetLinkDevice, BT>
{
    type Lock = Mutex<TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.tx_queue.queue)
    }
}

impl<BT: DeviceLayerTypes> OrderedLockAccess<DequeueState<(), Buf<Vec<u8>>>>
    for IpLinkDeviceState<EthernetLinkDevice, BT>
{
    type Lock = Mutex<DequeueState<(), Buf<Vec<u8>>>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.link.tx_queue.deque)
    }
}

/// Returns the type of frame if it should be delivered, otherwise `None`.
fn deliver_as(
    static_state: &StaticEthernetDeviceState,
    dynamic_state: &DynamicEthernetDeviceState,
    dst_mac: &Mac,
) -> Option<FrameDestination> {
    if dynamic_state.promiscuous_mode {
        return Some(FrameDestination::from_dest(*dst_mac, static_state.mac.get()));
    }
    UnicastAddr::new(*dst_mac)
        .and_then(|u| {
            (static_state.mac == u).then_some(FrameDestination::Individual { local: true })
        })
        .or_else(|| dst_mac.is_broadcast().then_some(FrameDestination::Broadcast))
        .or_else(|| {
            MulticastAddr::new(*dst_mac).and_then(|a| {
                dynamic_state
                    .link_multicast_groups
                    .contains(&a)
                    .then_some(FrameDestination::Multicast)
            })
        })
}

/// A timer ID for Ethernet devices.
///
/// `D` is the type of device ID that identifies different Ethernet devices.
#[derive(Clone, Eq, PartialEq, Debug, Hash, GenericOverIp)]
#[generic_over_ip()]
pub enum EthernetTimerId<D: WeakDeviceIdentifier> {
    Arp(ArpTimerId<EthernetLinkDevice, D>),
    Nudv6(NudTimerId<Ipv6, EthernetLinkDevice, D>),
}

impl<I: Ip, D: WeakDeviceIdentifier> From<NudTimerId<I, EthernetLinkDevice, D>>
    for EthernetTimerId<D>
{
    fn from(id: NudTimerId<I, EthernetLinkDevice, D>) -> EthernetTimerId<D> {
        I::map_ip(id, EthernetTimerId::Arp, EthernetTimerId::Nudv6)
    }
}

impl<CC, BC> HandleableTimer<CC, BC> for EthernetTimerId<CC::WeakDeviceId>
where
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>
        + TimerHandler<BC, NudTimerId<Ipv6, EthernetLinkDevice, CC::WeakDeviceId>>
        + TimerHandler<BC, ArpTimerId<EthernetLinkDevice, CC::WeakDeviceId>>,
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC) {
        match self {
            EthernetTimerId::Arp(id) => core_ctx.handle_timer(bindings_ctx, id),
            EthernetTimerId::Nudv6(id) => core_ctx.handle_timer(bindings_ctx, id),
        }
    }
}

/// Send an IP packet in an Ethernet frame.
///
/// `send_ip_frame` accepts a device ID, a local IP address, and a
/// serializer. It computes the routing information, serializes
/// the serializer, and sends the resulting buffer in a new Ethernet
/// frame.
pub(super) fn send_ip_frame<BC, CC, A, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    local_addr: SpecifiedAddr<A>,
    body: S,
    broadcast: Option<<A::Version as IpTypesIpExt>::BroadcastMarker>,
) -> Result<(), S>
where
    BC: EthernetIpLinkDeviceBindingsContext
        + DeviceSocketBindingsContext<CC::DeviceId>
        + LinkResolutionContext<EthernetLinkDevice>,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>
        + NudHandler<A::Version, EthernetLinkDevice, BC>
        + TransmitQueueHandler<EthernetLinkDevice, BC, Meta = ()>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
    A: IpAddress,
    S: Serializer,
    S::Buffer: BufferMut,
    A::Version: EthernetIpExt + IpTypesIpExt,
{
    fn increment_counter<I: Ip>(counters: &DeviceCounters) {
        let () = I::map_ip(
            (),
            |()| counters.send_ipv4_frame.increment(),
            |()| counters.send_ipv6_frame.increment(),
        );
    }
    core_ctx.with_per_resource_counters(device_id, increment_counter::<A::Version>);
    core_ctx.with_counters(increment_counter::<A::Version>);

    trace!(
        "ethernet::send_ip_frame: local_addr = {:?}; device = {:?}; broadcast = {:?}",
        local_addr,
        device_id,
        broadcast
    );

    let body = body.with_size_limit(get_mtu(core_ctx, device_id).get() as usize);

    match broadcast {
        Some(marker) => {
            <A::Version as Ip>::map_ip::<_, ()>(
                WrapBroadcastMarker(marker),
                |WrapBroadcastMarker(())| (),
                |WrapBroadcastMarker(never)| match never {},
            );
            send_as_ethernet_frame_to_dst(
                core_ctx,
                bindings_ctx,
                device_id,
                Mac::BROADCAST,
                body,
                A::Version::ETHER_TYPE,
            )
        }
        None => {
            if let Some(multicast) = MulticastAddr::new(local_addr.get()) {
                send_as_ethernet_frame_to_dst(
                    core_ctx,
                    bindings_ctx,
                    device_id,
                    Mac::from(&multicast),
                    body,
                    A::Version::ETHER_TYPE,
                )
            } else {
                NudHandler::<A::Version, _, _>::send_ip_packet_to_neighbor(
                    core_ctx,
                    bindings_ctx,
                    device_id,
                    local_addr,
                    body,
                )
            }
        }
    }
    .map_err(Nested::into_inner)
}

/// Metadata for received ethernet frames.
pub struct RecvEthernetFrameMeta<D> {
    /// The device a frame was received on.
    pub device_id: D,
}

impl DeviceReceiveFrameSpec for EthernetLinkDevice {
    type FrameMetadata<D> = RecvEthernetFrameMeta<D>;
}

impl<CC, BC> ReceivableFrameMeta<CC, BC> for RecvEthernetFrameMeta<CC::DeviceId>
where
    BC: EthernetIpLinkDeviceBindingsContext
        + DeviceSocketBindingsContext<CC::DeviceId>
        + TracingContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>
        + RecvFrameContext<BC, RecvIpFrameMeta<CC::DeviceId, Ipv4>>
        + RecvFrameContext<BC, RecvIpFrameMeta<CC::DeviceId, Ipv6>>
        + ArpPacketHandler<EthernetLinkDevice, BC>
        + DeviceSocketHandler<EthernetLinkDevice, BC>
        + ResourceCounterContext<CC::DeviceId, DeviceCounters>
        + ResourceCounterContext<CC::DeviceId, EthernetDeviceCounters>,
{
    fn receive_meta<B: BufferMut + Debug>(
        self,
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        mut buffer: B,
    ) {
        trace_duration!(bindings_ctx, c"device::ethernet::receive_frame");
        let Self { device_id } = self;
        trace!("ethernet::receive_frame: device_id = {:?}", device_id);
        core_ctx.increment(&device_id, |counters: &DeviceCounters| &counters.recv_frame);
        // NOTE(joshlf): We do not currently validate that the Ethernet frame
        // satisfies the minimum length requirement. We expect that if this
        // requirement is necessary (due to requirements of the physical medium),
        // the driver or hardware will have checked it, and that if this requirement
        // is not necessary, it is acceptable for us to operate on a smaller
        // Ethernet frame. If this becomes insufficient in the future, we may want
        // to consider making this behavior configurable (at compile time, at
        // runtime on a global basis, or at runtime on a per-device basis).
        let (ethernet, whole_frame) = if let Ok(frame) =
            buffer.parse_with_view::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::NoCheck)
        {
            frame
        } else {
            core_ctx.increment(&device_id, |counters: &DeviceCounters| &counters.recv_parse_error);
            trace!("ethernet::receive_frame: failed to parse ethernet frame");
            return;
        };

        let dst = ethernet.dst_mac();

        let frame_dest = core_ctx.with_ethernet_state(&device_id, |static_state, dynamic_state| {
            deliver_as(static_state, dynamic_state, &dst)
        });

        let frame_dst = match frame_dest {
            None => {
                core_ctx.increment(&device_id, |counters: &EthernetDeviceCounters| {
                    &counters.recv_ethernet_other_dest
                });
                trace!(
                    "ethernet::receive_frame: destination mac {:?} not for device {:?}",
                    dst,
                    device_id
                );
                return;
            }
            Some(frame_dest) => frame_dest,
        };
        let ethertype = ethernet.ethertype();

        core_ctx.handle_frame(
            bindings_ctx,
            &device_id,
            ReceivedFrame::from_ethernet(ethernet, frame_dst).into(),
            whole_frame,
        );

        match ethertype {
            Some(EtherType::Arp) => {
                let types = if let Ok(types) = peek_arp_types(buffer.as_ref()) {
                    types
                } else {
                    return;
                };
                match types {
                    (ArpHardwareType::Ethernet, ArpNetworkType::Ipv4) => {
                        ArpPacketHandler::handle_packet(
                            core_ctx,
                            bindings_ctx,
                            device_id,
                            frame_dst,
                            buffer,
                        )
                    }
                }
            }
            Some(EtherType::Ipv4) => {
                core_ctx.increment(&device_id, |counters: &DeviceCounters| {
                    &counters.recv_ipv4_delivered
                });
                core_ctx.receive_frame(
                    bindings_ctx,
                    RecvIpFrameMeta::<_, Ipv4>::new(device_id, Some(frame_dst)),
                    buffer,
                )
            }
            Some(EtherType::Ipv6) => {
                core_ctx.increment(&device_id, |counters: &DeviceCounters| {
                    &counters.recv_ipv6_delivered
                });
                core_ctx.receive_frame(
                    bindings_ctx,
                    RecvIpFrameMeta::<_, Ipv6>::new(device_id, Some(frame_dst)),
                    buffer,
                )
            }
            Some(EtherType::Other(_)) => {
                core_ctx.increment(&device_id, |counters: &EthernetDeviceCounters| {
                    &counters.recv_unsupported_ethertype
                });
            }
            None => {
                core_ctx.increment(&device_id, |counters: &EthernetDeviceCounters| {
                    &counters.recv_no_ethertype
                });
            }
        }
    }
}

/// Set the promiscuous mode flag on `device_id`.
// NB: We don't currently have a need to expose this function so it's defined as
// testonly.
#[cfg(test)]
pub(super) fn set_promiscuous_mode<
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
    enabled: bool,
) {
    core_ctx.with_ethernet_state_mut(device_id, |_static_state, dynamic_state| {
        dynamic_state.promiscuous_mode = enabled
    })
}

/// Add `device_id` to a link multicast group `multicast_addr`.
///
/// Calling `join_link_multicast` with the same `device_id` and `multicast_addr`
/// is completely safe. A counter will be kept for the number of times
/// `join_link_multicast` has been called with the same `device_id` and
/// `multicast_addr` pair. To completely leave a multicast group,
/// [`leave_link_multicast`] must be called the same number of times
/// `join_link_multicast` has been called for the same `device_id` and
/// `multicast_addr` pair. The first time `join_link_multicast` is called for a
/// new `device` and `multicast_addr` pair, the device will actually join the
/// multicast group.
///
/// `join_link_multicast` is different from [`join_ip_multicast`] as
/// `join_link_multicast` joins an L2 multicast group, whereas
/// `join_ip_multicast` joins an L3 multicast group.
pub(super) fn join_link_multicast<
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<Mac>,
) {
    core_ctx.with_ethernet_state_mut(device_id, |_static_state, dynamic_state| {
        let groups = &mut dynamic_state.link_multicast_groups;

        match groups.insert(multicast_addr) {
            InsertResult::Inserted(()) => {
                trace!(
                    "ethernet::join_link_multicast: joining link multicast {:?}",
                    multicast_addr
                );
            }
            InsertResult::AlreadyPresent => {
                trace!(
                    "ethernet::join_link_multicast: already joined link multicast {:?}",
                    multicast_addr,
                );
            }
        }
    })
}

/// Remove `device_id` from a link multicast group `multicast_addr`.
///
/// `leave_link_multicast` will attempt to remove `device_id` from the multicast
/// group `multicast_addr`. `device_id` may have "joined" the same multicast
/// address multiple times, so `device_id` will only leave the multicast group
/// once `leave_ip_multicast` has been called for each corresponding
/// [`join_link_multicast`]. That is, if `join_link_multicast` gets called 3
/// times and `leave_link_multicast` gets called two times (after all 3
/// `join_link_multicast` calls), `device_id` will still be in the multicast
/// group until the next (final) call to `leave_link_multicast`.
///
/// `leave_link_multicast` is different from [`leave_ip_multicast`] as
/// `leave_link_multicast` leaves an L2 multicast group, whereas
/// `leave_ip_multicast` leaves an L3 multicast group.
///
/// # Panics
///
/// If `device_id` is not in the multicast group `multicast_addr`.
pub(super) fn leave_link_multicast<
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<Mac>,
) {
    core_ctx.with_ethernet_state_mut(device_id, |_static_state, dynamic_state| {
        let groups = &mut dynamic_state.link_multicast_groups;

        match groups.remove(multicast_addr) {
            RemoveResult::Removed(()) => {
                trace!("ethernet::leave_link_multicast: leaving link multicast {:?}", multicast_addr);
            }
            RemoveResult::StillPresent => {
                trace!(
                    "ethernet::leave_link_multicast: not leaving link multicast {:?} as there are still listeners for it",
                    multicast_addr,
                );
            }
            RemoveResult::NotPresent => {
                panic!(
                    "ethernet::leave_link_multicast: device {:?} has not yet joined link multicast {:?}",
                    device_id,
                    multicast_addr,
                );
            }
        }
    })
}

/// Get the MTU associated with this device.
pub(super) fn get_mtu<
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> Mtu {
    core_ctx.with_ethernet_state(device_id, |_static_state, dynamic_state| {
        dynamic_state.max_frame_size.as_mtu()
    })
}

/// Enables a blanket implementation of [`SendableFrameData`] for
/// [`ArpFrameMetadata`].
///
/// Implementing this marker trait for a type enables a blanket implementation
/// of `SendableFrameData` given the other requirements are met.
pub trait UseArpFrameMetadataBlanket {}

impl<
        BC: EthernetIpLinkDeviceBindingsContext + DeviceSocketBindingsContext<CC::DeviceId>,
        CC: EthernetIpLinkDeviceDynamicStateContext<BC>
            + TransmitQueueHandler<EthernetLinkDevice, BC, Meta = ()>
            + ResourceCounterContext<CC::DeviceId, DeviceCounters>
            + UseArpFrameMetadataBlanket,
    > SendableFrameMeta<CC, BC> for ArpFrameMetadata<EthernetLinkDevice, CC::DeviceId>
{
    fn send_meta<S>(self, core_ctx: &mut CC, bindings_ctx: &mut BC, body: S) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        let Self { device_id, dst_addr } = self;
        send_as_ethernet_frame_to_dst(
            core_ctx,
            bindings_ctx,
            &device_id,
            dst_addr,
            body,
            EtherType::Arp,
        )
    }
}

impl DeviceSocketSendTypes for EthernetLinkDevice {
    /// When `None`, data will be sent as a raw Ethernet frame without any
    /// system-applied headers.
    type Metadata = Option<EthernetHeaderParams>;
}

impl<
        BC: EthernetIpLinkDeviceBindingsContext,
        CC: EthernetIpLinkDeviceDynamicStateContext<BC>
            + TransmitQueueHandler<EthernetLinkDevice, BC, Meta = ()>
            + ResourceCounterContext<CC::DeviceId, DeviceCounters>,
    > SendableFrameMeta<CC, BC> for DeviceSocketMetadata<EthernetLinkDevice, EthernetDeviceId<BC>>
where
    CC: DeviceIdContext<EthernetLinkDevice, DeviceId = EthernetDeviceId<BC>>,
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
                dest_addr,
                body,
                protocol,
            ),
            None => send_ethernet_frame(core_ctx, bindings_ctx, &device_id, body),
        }
    }
}

pub(super) fn get_mac<
    'a,
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>,
>(
    core_ctx: &'a mut CC,
    device_id: &CC::DeviceId,
) -> UnicastAddr<Mac> {
    core_ctx.with_static_ethernet_device_state(device_id, |state| state.mac)
}

pub(super) fn set_mtu<
    BC: EthernetIpLinkDeviceBindingsContext,
    CC: EthernetIpLinkDeviceDynamicStateContext<BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
    mtu: Mtu,
) {
    core_ctx.with_ethernet_state_mut(device_id, |static_state, dynamic_state| {
        if let Some(mut frame_size ) = MaxEthernetFrameSize::from_mtu(mtu) {
            // If `frame_size` is greater than what the device supports, set it
            // to maximum frame size the device supports.
            if frame_size > static_state.max_frame_size {
                trace!("ethernet::ndp_device::set_mtu: MTU of {:?} is greater than the device {:?}'s max MTU of {:?}, using device's max MTU instead", mtu, device_id, static_state.max_frame_size.as_mtu());
                frame_size = static_state.max_frame_size;
            }
            trace!("ethernet::ndp_device::set_mtu: setting link MTU to {:?}", mtu);
            dynamic_state.max_frame_size = frame_size;
        }
    })
}

/// An implementation of the [`LinkDevice`] trait for Ethernet devices.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum EthernetLinkDevice {}

impl Device for EthernetLinkDevice {}

impl LinkDevice for EthernetLinkDevice {
    type Address = Mac;
}

impl DeviceStateSpec for EthernetLinkDevice {
    type Link<BT: DeviceLayerTypes> = EthernetDeviceState<BT>;
    type External<BT: DeviceLayerTypes> = BT::EthernetDeviceState;
    type CreationProperties = EthernetCreationProperties;
    type Counters = EthernetDeviceCounters;
    type TimerId<D: WeakDeviceIdentifier> = EthernetTimerId<D>;

    fn new_link_state<
        CC: CoreTimerContext<Self::TimerId<CC::WeakDeviceId>, BC> + DeviceIdContext<Self>,
        BC: DeviceLayerTypes + TimerContext,
    >(
        bindings_ctx: &mut BC,
        self_id: CC::WeakDeviceId,
        EthernetCreationProperties { mac, max_frame_size }: Self::CreationProperties,
    ) -> Self::Link<BC> {
        let ipv4_arp = Mutex::new(ArpState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(
            bindings_ctx,
            self_id.clone(),
        ));
        let ipv6_nud =
            Mutex::new(NudState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx, self_id));
        EthernetDeviceState {
            counters: Default::default(),
            ipv4_arp,
            ipv6_nud,
            ipv4_nud_config: Default::default(),
            ipv6_nud_config: Default::default(),
            static_state: StaticEthernetDeviceState { mac, max_frame_size },
            dynamic_state: RwLock::new(DynamicEthernetDeviceState::new(max_frame_size)),
            tx_queue: Default::default(),
        }
    }
    const IS_LOOPBACK: bool = false;
    const DEBUG_TYPE: &'static str = "Ethernet";
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use assert_matches::assert_matches;

    use ip_test_macro::ip_test;
    use net_declare::net_mac;
    use net_types::ip::{AddrSubnet, IpAddr, IpVersion, Ipv4Addr, Ipv6Addr};
    use netstack3_base::IntoCoreTimerCtx;
    use packet_formats::{
        ethernet::ETHERNET_MIN_BODY_LEN_NO_TAG,
        icmp::IcmpDestUnreachable,
        ip::{IpExt, IpPacketBuilder, IpProto},
        testdata::{dns_request_v4, dns_request_v6},
        testutil::{
            parse_ethernet_frame, parse_icmp_packet_in_ip_packet_in_ethernet_frame,
            parse_ip_packet_in_ethernet_frame,
        },
    };
    use rand::Rng;
    use test_case::test_case;

    use super::*;
    use crate::{
        context::{testutil::FakeInstant, CounterContext, CtxPair},
        device::{
            arp::{ArpConfigContext, ArpContext, ArpCounters, ArpSenderContext},
            queue::tx::{TransmitQueueBindingsContext, TransmitQueueCommon, TransmitQueueContext},
            socket::{Frame, ParseSentFrameError, SentFrame},
            testutil::{set_forwarding_enabled, FakeDeviceId, FakeWeakDeviceId},
            DeviceId, DeviceSendFrameError,
        },
        error::NotFoundError,
        ip::device::{
            api::AddIpAddrSubnetError,
            config::{IpDeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate},
            nud::{self, api::NeighborApi, DynamicNeighborUpdateSource},
            slaac::SlaacConfiguration,
            IpAddressId as _,
        },
        testutil::{
            add_arp_or_ndp_table_entry, new_rng, CtxPairExt as _, FakeCtxBuilder, TestIpExt,
            DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE, TEST_ADDRS_V4,
        },
    };

    struct FakeEthernetCtx {
        static_state: StaticEthernetDeviceState,
        dynamic_state: DynamicEthernetDeviceState,
        tx_queue: TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>,
        counters: DeviceCounters,
        per_device_counters: DeviceCounters,
        ethernet_counters: EthernetDeviceCounters,
        arp_counters: ArpCounters,
    }

    impl FakeEthernetCtx {
        fn new(mac: UnicastAddr<Mac>, max_frame_size: MaxEthernetFrameSize) -> FakeEthernetCtx {
            FakeEthernetCtx {
                static_state: StaticEthernetDeviceState { max_frame_size, mac },
                dynamic_state: DynamicEthernetDeviceState::new(max_frame_size),
                tx_queue: Default::default(),
                counters: Default::default(),
                per_device_counters: Default::default(),
                ethernet_counters: Default::default(),
                arp_counters: Default::default(),
            }
        }
    }

    type FakeBindingsCtx = crate::context::testutil::FakeBindingsCtx<
        EthernetTimerId<FakeWeakDeviceId<FakeDeviceId>>,
        nud::Event<Mac, FakeDeviceId, Ipv4, FakeInstant>,
        (),
        (),
    >;

    type FakeInnerCtx =
        crate::context::testutil::FakeCoreCtx<FakeEthernetCtx, FakeDeviceId, FakeDeviceId>;

    struct FakeCoreCtx {
        arp_state: ArpState<EthernetLinkDevice, FakeBindingsCtx>,
        inner: FakeInnerCtx,
    }

    fn new_context() -> CtxPair<FakeCoreCtx, FakeBindingsCtx> {
        CtxPair::with_default_bindings_ctx(|bindings_ctx| FakeCoreCtx {
            arp_state: ArpState::new::<_, IntoCoreTimerCtx>(
                bindings_ctx,
                FakeWeakDeviceId(FakeDeviceId),
            ),
            inner: FakeInnerCtx::with_state(FakeEthernetCtx::new(
                TEST_ADDRS_V4.local_mac,
                IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            )),
        })
    }

    impl DeviceSocketHandler<EthernetLinkDevice, FakeBindingsCtx> for FakeCoreCtx {
        fn handle_frame(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtx,
            device: &Self::DeviceId,
            frame: Frame<&[u8]>,
            whole_frame: &[u8],
        ) {
            self.inner.handle_frame(bindings_ctx, device, frame, whole_frame)
        }
    }

    impl CounterContext<DeviceCounters> for FakeCoreCtx {
        fn with_counters<O, F: FnOnce(&DeviceCounters) -> O>(&self, cb: F) -> O {
            cb(&self.inner.state.counters)
        }
    }

    impl CounterContext<DeviceCounters> for FakeInnerCtx {
        fn with_counters<O, F: FnOnce(&DeviceCounters) -> O>(&self, cb: F) -> O {
            cb(&self.state.counters)
        }
    }

    impl ResourceCounterContext<FakeDeviceId, DeviceCounters> for FakeCoreCtx {
        fn with_per_resource_counters<O, F: FnOnce(&DeviceCounters) -> O>(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            cb(&self.inner.state.per_device_counters)
        }
    }

    impl ResourceCounterContext<FakeDeviceId, DeviceCounters> for FakeInnerCtx {
        fn with_per_resource_counters<O, F: FnOnce(&DeviceCounters) -> O>(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            cb(&self.state.per_device_counters)
        }
    }

    impl CounterContext<EthernetDeviceCounters> for FakeCoreCtx {
        fn with_counters<O, F: FnOnce(&EthernetDeviceCounters) -> O>(&self, cb: F) -> O {
            cb(&self.inner.state.ethernet_counters)
        }
    }

    impl CounterContext<EthernetDeviceCounters> for FakeInnerCtx {
        fn with_counters<O, F: FnOnce(&EthernetDeviceCounters) -> O>(&self, cb: F) -> O {
            cb(&self.state.ethernet_counters)
        }
    }

    impl DeviceSocketHandler<EthernetLinkDevice, FakeBindingsCtx> for FakeInnerCtx {
        fn handle_frame(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtx,
            _device: &Self::DeviceId,
            _frame: Frame<&[u8]>,
            _whole_frame: &[u8],
        ) {
            // No-op: don't deliver frames.
        }
    }

    impl EthernetIpLinkDeviceStaticStateContext for FakeCoreCtx {
        fn with_static_ethernet_device_state<O, F: FnOnce(&StaticEthernetDeviceState) -> O>(
            &mut self,
            device_id: &FakeDeviceId,
            cb: F,
        ) -> O {
            self.inner.with_static_ethernet_device_state(device_id, cb)
        }
    }

    impl EthernetIpLinkDeviceStaticStateContext for FakeInnerCtx {
        fn with_static_ethernet_device_state<O, F: FnOnce(&StaticEthernetDeviceState) -> O>(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            cb(&self.state.static_state)
        }
    }

    impl EthernetIpLinkDeviceDynamicStateContext<FakeBindingsCtx> for FakeCoreCtx {
        fn with_ethernet_state<
            O,
            F: FnOnce(&StaticEthernetDeviceState, &DynamicEthernetDeviceState) -> O,
        >(
            &mut self,
            device_id: &FakeDeviceId,
            cb: F,
        ) -> O {
            self.inner.with_ethernet_state(device_id, cb)
        }

        fn with_ethernet_state_mut<
            O,
            F: FnOnce(&StaticEthernetDeviceState, &mut DynamicEthernetDeviceState) -> O,
        >(
            &mut self,
            device_id: &FakeDeviceId,
            cb: F,
        ) -> O {
            self.inner.with_ethernet_state_mut(device_id, cb)
        }
    }

    impl EthernetIpLinkDeviceDynamicStateContext<FakeBindingsCtx> for FakeInnerCtx {
        fn with_ethernet_state<
            O,
            F: FnOnce(&StaticEthernetDeviceState, &DynamicEthernetDeviceState) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            let FakeEthernetCtx { static_state, dynamic_state, .. } = &self.state;
            cb(static_state, dynamic_state)
        }

        fn with_ethernet_state_mut<
            O,
            F: FnOnce(&StaticEthernetDeviceState, &mut DynamicEthernetDeviceState) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            let FakeEthernetCtx { static_state, dynamic_state, .. } = &mut self.state;
            cb(static_state, dynamic_state)
        }
    }

    impl NudHandler<Ipv6, EthernetLinkDevice, FakeBindingsCtx> for FakeCoreCtx {
        fn handle_neighbor_update(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtx,
            _device_id: &Self::DeviceId,
            _neighbor: SpecifiedAddr<Ipv6Addr>,
            _link_addr: Mac,
            _is_confirmation: DynamicNeighborUpdateSource,
        ) {
            unimplemented!()
        }

        fn flush(&mut self, _bindings_ctx: &mut FakeBindingsCtx, _device_id: &Self::DeviceId) {
            unimplemented!()
        }

        fn send_ip_packet_to_neighbor<S>(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtx,
            _device_id: &Self::DeviceId,
            _neighbor: SpecifiedAddr<Ipv6Addr>,
            _body: S,
        ) -> Result<(), S> {
            unimplemented!()
        }
    }

    struct FakeCoreCtxWithDeviceId<'a> {
        core_ctx: &'a mut FakeInnerCtx,
        device_id: &'a FakeDeviceId,
    }

    impl<'a> DeviceIdContext<EthernetLinkDevice> for FakeCoreCtxWithDeviceId<'a> {
        type DeviceId = FakeDeviceId;
        type WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>;
    }

    impl<'a> ArpConfigContext for FakeCoreCtxWithDeviceId<'a> {
        fn with_nud_user_config<O, F: FnOnce(&NudUserConfig) -> O>(&mut self, cb: F) -> O {
            cb(&NudUserConfig::default())
        }
    }

    impl UseArpFrameMetadataBlanket for FakeCoreCtx {}

    impl ArpContext<EthernetLinkDevice, FakeBindingsCtx> for FakeCoreCtx {
        type ConfigCtx<'a> = FakeCoreCtxWithDeviceId<'a>;

        type ArpSenderCtx<'a> = FakeCoreCtxWithDeviceId<'a>;

        fn with_arp_state_mut_and_sender_ctx<
            O,
            F: FnOnce(
                &mut ArpState<EthernetLinkDevice, FakeBindingsCtx>,
                &mut Self::ArpSenderCtx<'_>,
            ) -> O,
        >(
            &mut self,
            device_id: &Self::DeviceId,
            cb: F,
        ) -> O {
            let Self { arp_state, inner } = self;
            cb(arp_state, &mut FakeCoreCtxWithDeviceId { core_ctx: inner, device_id })
        }

        fn get_protocol_addr(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtx,
            _device_id: &Self::DeviceId,
        ) -> Option<Ipv4Addr> {
            unimplemented!()
        }

        fn get_hardware_addr(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtx,
            _device_id: &Self::DeviceId,
        ) -> UnicastAddr<Mac> {
            self.inner.state.static_state.mac
        }

        fn with_arp_state_mut<
            O,
            F: FnOnce(
                &mut ArpState<EthernetLinkDevice, FakeBindingsCtx>,
                &mut Self::ConfigCtx<'_>,
            ) -> O,
        >(
            &mut self,
            device_id: &Self::DeviceId,
            cb: F,
        ) -> O {
            let Self { arp_state, inner } = self;
            cb(arp_state, &mut FakeCoreCtxWithDeviceId { core_ctx: inner, device_id })
        }

        fn with_arp_state<O, F: FnOnce(&ArpState<EthernetLinkDevice, FakeBindingsCtx>) -> O>(
            &mut self,
            FakeDeviceId: &Self::DeviceId,
            cb: F,
        ) -> O {
            cb(&mut self.arp_state)
        }
    }

    impl ArpConfigContext for FakeInnerCtx {
        fn with_nud_user_config<O, F: FnOnce(&NudUserConfig) -> O>(&mut self, cb: F) -> O {
            cb(&NudUserConfig::default())
        }
    }

    impl<'a> ArpSenderContext<EthernetLinkDevice, FakeBindingsCtx> for FakeCoreCtxWithDeviceId<'a> {
        fn send_ip_packet_to_neighbor_link_addr<S>(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtx,
            link_addr: Mac,
            body: S,
        ) -> Result<(), S>
        where
            S: Serializer,
            S::Buffer: BufferMut,
        {
            let Self { core_ctx, device_id } = self;
            send_as_ethernet_frame_to_dst(
                *core_ctx,
                bindings_ctx,
                device_id,
                link_addr,
                body,
                EtherType::Ipv4,
            )
        }
    }

    impl TransmitQueueBindingsContext<FakeDeviceId> for FakeBindingsCtx {
        fn wake_tx_task(&mut self, FakeDeviceId: &FakeDeviceId) {
            unimplemented!("unused by tests")
        }
    }

    impl TransmitQueueCommon<EthernetLinkDevice, FakeBindingsCtx> for FakeCoreCtx {
        type Meta = ();
        type Allocator = BufVecU8Allocator;
        type Buffer = Buf<Vec<u8>>;

        fn parse_outgoing_frame<'a>(
            buf: &'a [u8],
            meta: &'a Self::Meta,
        ) -> Result<SentFrame<&'a [u8]>, ParseSentFrameError> {
            FakeInnerCtx::parse_outgoing_frame(buf, meta)
        }
    }

    impl TransmitQueueCommon<EthernetLinkDevice, FakeBindingsCtx> for FakeInnerCtx {
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

    impl TransmitQueueContext<EthernetLinkDevice, FakeBindingsCtx> for FakeCoreCtx {
        fn with_transmit_queue_mut<
            O,
            F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
        >(
            &mut self,
            device_id: &Self::DeviceId,
            cb: F,
        ) -> O {
            self.inner.with_transmit_queue_mut(device_id, cb)
        }

        fn send_frame(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtx,
            device_id: &Self::DeviceId,
            (): Self::Meta,
            buf: Self::Buffer,
        ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>> {
            TransmitQueueContext::send_frame(&mut self.inner, bindings_ctx, device_id, (), buf)
        }
    }

    impl TransmitQueueContext<EthernetLinkDevice, FakeBindingsCtx> for FakeInnerCtx {
        fn with_transmit_queue_mut<
            O,
            F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
        >(
            &mut self,
            _device_id: &Self::DeviceId,
            cb: F,
        ) -> O {
            cb(&mut self.state.tx_queue)
        }

        fn send_frame(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtx,
            device_id: &Self::DeviceId,
            (): Self::Meta,
            buf: Self::Buffer,
        ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>> {
            self.frames.push(device_id.clone(), buf.as_ref().to_vec());
            Ok(())
        }
    }

    impl DeviceIdContext<EthernetLinkDevice> for FakeCoreCtx {
        type DeviceId = FakeDeviceId;
        type WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>;
    }

    impl DeviceIdContext<EthernetLinkDevice> for FakeInnerCtx {
        type DeviceId = FakeDeviceId;
        type WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>;
    }

    impl CounterContext<ArpCounters> for FakeCoreCtx {
        fn with_counters<O, F: FnOnce(&ArpCounters) -> O>(&self, cb: F) -> O {
            cb(&self.inner.state.arp_counters)
        }
    }

    fn contains_addr<A: IpAddress>(
        core_ctx: &crate::testutil::FakeCoreCtx,
        device: &DeviceId<crate::testutil::FakeBindingsCtx>,
        addr: SpecifiedAddr<A>,
    ) -> bool {
        match addr.into() {
            IpAddr::V4(addr) => {
                crate::ip::device::IpDeviceStateContext::<Ipv4, _>::with_address_ids(
                    &mut core_ctx.context(),
                    device,
                    |mut addrs, _core_ctx| addrs.any(|a| a.addr().addr() == addr.get()),
                )
            }
            IpAddr::V6(addr) => {
                crate::ip::device::IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                    &mut core_ctx.context(),
                    device,
                    |mut addrs, _core_ctx| addrs.any(|a| a.addr().addr() == addr.get()),
                )
            }
        }
    }

    #[test]
    fn test_mtu() {
        // Test that we send an Ethernet frame whose size is less than the MTU,
        // and that we don't send an Ethernet frame whose size is greater than
        // the MTU.
        fn test(size: usize, expect_frames_sent: bool) {
            let mut ctx = new_context();
            NeighborApi::<Ipv4, EthernetLinkDevice, _>::new(ctx.as_mut())
                .insert_static_entry(
                    &FakeDeviceId,
                    TEST_ADDRS_V4.remote_ip.get(),
                    TEST_ADDRS_V4.remote_mac.get(),
                )
                .unwrap();
            let CtxPair { core_ctx, bindings_ctx } = &mut ctx;
            let result = send_ip_frame(
                core_ctx,
                bindings_ctx,
                &FakeDeviceId,
                TEST_ADDRS_V4.remote_ip,
                Buf::new(&mut vec![0; size], ..),
                None,
            )
            .map_err(|_serializer| ());
            let sent_frames = core_ctx.inner.frames().len();
            if expect_frames_sent {
                assert_eq!(sent_frames, 1);
                result.expect("should succeed");
            } else {
                assert_eq!(sent_frames, 0);
                result.expect_err("should fail");
            }
        }

        test(usize::try_from(u32::from(Ipv6::MINIMUM_LINK_MTU)).unwrap(), true);
        test(usize::try_from(u32::from(Ipv6::MINIMUM_LINK_MTU)).unwrap() + 1, false);
    }

    #[test]
    fn broadcast() {
        let mut ctx = new_context();
        let CtxPair { core_ctx, bindings_ctx } = &mut ctx;
        send_ip_frame(
            core_ctx,
            bindings_ctx,
            &FakeDeviceId,
            TEST_ADDRS_V4.remote_ip,
            Buf::new(&mut vec![0; 100], ..),
            /* broadcast */ Some(()),
        )
        .map_err(|_serializer| ())
        .expect("send_ip_frame should succeed");
        let sent_frames = core_ctx.inner.frames().len();
        assert_eq!(sent_frames, 1);
        let (FakeDeviceId, frame) = core_ctx.inner.frames()[0].clone();
        let (_body, _src_mac, dst_mac, _ether_type) =
            parse_ethernet_frame(&frame, EthernetFrameLengthCheck::NoCheck).unwrap();
        assert_eq!(dst_mac, Mac::BROADCAST);
    }

    #[ip_test]
    #[test_case(true; "enabled")]
    #[test_case(false; "disabled")]
    fn test_receive_ip_frame<I: Ip + TestIpExt + crate::IpExt>(enable: bool) {
        // Should only receive a frame if the device is enabled.

        let config = I::TEST_ADDRS;
        let mut ctx = crate::testutil::FakeCtx::default();
        let eth_device =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            );
        let device = eth_device.clone().into();
        let mut bytes = match I::VERSION {
            IpVersion::V4 => dns_request_v4::ETHERNET_FRAME,
            IpVersion::V6 => dns_request_v6::ETHERNET_FRAME,
        }
        .bytes
        .to_vec();

        let mac_bytes = config.local_mac.bytes();
        bytes[0..6].copy_from_slice(&mac_bytes);

        let expected_received = if enable {
            crate::device::testutil::enable_device(&mut ctx, &device);
            1
        } else {
            0
        };

        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device }, Buf::new(bytes, ..));

        assert_eq!(ctx.core_ctx.ip_counters::<I>().receive_ip_packet.get(), expected_received);
    }

    #[test]
    fn initialize_once() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: TEST_ADDRS_V4.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);
    }

    fn is_forwarding_enabled<I: Ip>(
        ctx: &mut crate::testutil::FakeCtx,
        device: &DeviceId<crate::testutil::FakeBindingsCtx>,
    ) -> bool {
        match I::VERSION {
            IpVersion::V4 => crate::device::testutil::is_forwarding_enabled::<_, Ipv4>(ctx, device),
            IpVersion::V6 => crate::device::testutil::is_forwarding_enabled::<_, Ipv6>(ctx, device),
        }
    }

    #[ip_test]
    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    fn test_set_ip_routing<I: Ip + TestIpExt + crate::IpExt>() {
        fn check_other_is_forwarding_enabled<I: Ip>(
            ctx: &mut crate::testutil::FakeCtx,
            device: &DeviceId<crate::testutil::FakeBindingsCtx>,
            expected: bool,
        ) {
            let enabled = match I::VERSION {
                IpVersion::V4 => is_forwarding_enabled::<Ipv6>(ctx, device),
                IpVersion::V6 => is_forwarding_enabled::<Ipv4>(ctx, device),
            };

            assert_eq!(enabled, expected);
        }

        fn check_icmp<I: Ip>(buf: &[u8]) {
            match I::VERSION {
                IpVersion::V4 => {
                    let _ = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                        Ipv4,
                        _,
                        IcmpDestUnreachable,
                        _,
                    >(buf, EthernetFrameLengthCheck::NoCheck, |_| {})
                    .unwrap();
                }
                IpVersion::V6 => {
                    let _ = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                        Ipv6,
                        _,
                        IcmpDestUnreachable,
                        _,
                    >(buf, EthernetFrameLengthCheck::NoCheck, |_| {})
                    .unwrap();
                }
            }
        }

        let src_ip = I::get_other_ip_address(3);
        let src_mac = UnicastAddr::new(Mac::new([10, 11, 12, 13, 14, 15])).unwrap();
        let config = I::TEST_ADDRS;
        let frame_dst = FrameDestination::Individual { local: true };
        let mut rng = new_rng(70812476915813);
        let mut body: Vec<u8> = core::iter::repeat_with(|| rng.gen()).take(100).collect();
        let buf = Buf::new(&mut body[..], ..)
            .encapsulate(I::PacketBuilder::new(
                src_ip.get(),
                config.remote_ip.get(),
                64,
                IpProto::Tcp.into(),
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .unwrap_b();

        // Test with netstack no forwarding

        let mut builder = FakeCtxBuilder::with_addrs(config.clone());
        let device_builder_id = 0;
        add_arp_or_ndp_table_entry(&mut builder, device_builder_id, src_ip, src_mac);
        let (mut ctx, device_ids) = builder.build();
        let device: DeviceId<_> = device_ids[device_builder_id].clone().into();

        // Should not be a router (default).
        assert!(!is_forwarding_enabled::<I>(&mut ctx, &device));
        check_other_is_forwarding_enabled::<I>(&mut ctx, &device, false);

        // Receiving a packet not destined for the node should only result in a
        // dest unreachable message if routing is enabled.
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf.clone());
        assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);

        // Set routing and expect packets to be forwarded.
        set_forwarding_enabled::<_, I>(&mut ctx, &device, true);
        assert!(is_forwarding_enabled::<I>(&mut ctx, &device));
        // Should not update other Ip routing status.
        check_other_is_forwarding_enabled::<I>(&mut ctx, &device, false);

        // Should route the packet since routing fully enabled (netstack &
        // device).
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf.clone());
        {
            let frames = ctx.bindings_ctx.take_ethernet_frames();
            let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
            let (packet_buf, _, _, packet_src_ip, packet_dst_ip, proto, ttl) =
                parse_ip_packet_in_ethernet_frame::<I>(
                    &frame[..],
                    EthernetFrameLengthCheck::NoCheck,
                )
                .unwrap();
            assert_eq!(src_ip.get(), packet_src_ip);
            assert_eq!(config.remote_ip.get(), packet_dst_ip);
            assert_eq!(proto, IpProto::Tcp.into());
            assert_eq!(body, packet_buf);
            assert_eq!(ttl, 63);
        }

        // Test routing a packet to an unknown address.
        let buf_unknown_dest = Buf::new(&mut body[..], ..)
            .encapsulate(I::PacketBuilder::new(
                src_ip.get(),
                // Addr must be remote, otherwise this will cause an NDP/ARP
                // request rather than ICMP unreachable.
                I::get_other_remote_ip_address(10).get(),
                64,
                IpProto::Tcp.into(),
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .unwrap_b();
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf_unknown_dest);
        let frames = ctx.bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        check_icmp::<I>(&frame);

        // Attempt to unset router
        set_forwarding_enabled::<_, I>(&mut ctx, &device, false);
        assert!(!is_forwarding_enabled::<I>(&mut ctx, &device));
        check_other_is_forwarding_enabled::<I>(&mut ctx, &device, false);

        // Should not route packets anymore
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf);
        assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);
    }

    #[ip_test]
    #[test_case(UnicastAddr::new(net_mac!("12:13:14:15:16:17")).unwrap(), true; "unicast")]
    #[test_case(MulticastAddr::new(net_mac!("13:14:15:16:17:18")).unwrap(), false; "multicast")]
    fn test_promiscuous_mode<I: Ip + TestIpExt + crate::IpExt>(
        other_mac: impl Witness<Mac>,
        is_other_host: bool,
    ) {
        // Test that frames not destined for a device will still be accepted
        // when the device is put into promiscuous mode. In all cases, frames
        // that are destined for a device must always be accepted.

        let config = I::TEST_ADDRS;
        let (mut ctx, device_ids) = FakeCtxBuilder::with_addrs(config.clone()).build();
        let eth_device = &device_ids[0];

        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(I::PacketBuilder::new(
                config.remote_ip.get(),
                config.local_ip.get(),
                64,
                IpProto::Tcp.into(),
            ))
            .encapsulate(EthernetFrameBuilder::new(
                config.remote_mac.get(),
                config.local_mac.get(),
                I::ETHER_TYPE,
                ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .unwrap_b();

        // Accept packet destined for this device if promiscuous mode is off.
        set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, false);
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf.clone());

        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet_other_host.get(), 0);

        // Accept packet destined for this device if promiscuous mode is on.
        set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, true);
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf);

        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 2);
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet_other_host.get(), 0);

        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(I::PacketBuilder::new(
                config.remote_ip.get(),
                config.local_ip.get(),
                64,
                IpProto::Tcp.into(),
            ))
            .encapsulate(EthernetFrameBuilder::new(
                config.remote_mac.get(),
                other_mac.get(),
                I::ETHER_TYPE,
                ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .unwrap_b();

        // Reject packet not destined for this device if promiscuous mode is
        // off.
        set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, false);
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf.clone());

        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 2);
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet_other_host.get(), 0);

        // Accept packet not destined for this device if promiscuous mode is on.
        set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, true);
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf);

        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 3);
        assert_eq!(
            ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet_other_host.get(),
            u64::from(is_other_host)
        );
    }

    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    #[ip_test]
    fn test_add_remove_ip_addresses<I: Ip + TestIpExt + crate::IpExt>() {
        let config = I::TEST_ADDRS;
        let mut ctx = crate::testutil::FakeCtx::default();

        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();

        crate::device::testutil::enable_device(&mut ctx, &device);

        let ip1 = I::get_other_ip_address(1);
        let ip2 = I::get_other_ip_address(2);
        let ip3 = I::get_other_ip_address(3);

        let prefix = I::Addr::BYTES * 8;
        let as1 = AddrSubnet::new(ip1.get(), prefix).unwrap();
        let as2 = AddrSubnet::new(ip2.get(), prefix).unwrap();

        assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip2));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

        // Add ip1 (ok)
        ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as1).unwrap();
        assert!(contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip2));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

        // Add ip2 (ok)
        ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as2).unwrap();
        assert!(contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(contains_addr(&ctx.core_ctx, &device, ip2));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

        // Del ip1 (ok)
        assert_eq!(
            ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip1).unwrap().into_removed(),
            as1
        );
        assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(contains_addr(&ctx.core_ctx, &device, ip2));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

        // Del ip1 again (ip1 not found)
        assert_matches!(
            ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip1),
            Err(NotFoundError)
        );
        assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(contains_addr(&ctx.core_ctx, &device, ip2));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

        // Add ip2 again (ip2 already exists)
        assert_eq!(
            ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as2),
            Err(AddIpAddrSubnetError::Exists),
        );
        assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(contains_addr(&ctx.core_ctx, &device, ip2));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

        // Add ip2 with different subnet (ip2 already exists)
        assert_eq!(
            ctx.core_api()
                .device_ip::<I>()
                .add_ip_addr_subnet(&device, AddrSubnet::new(ip2.get(), prefix - 1).unwrap()),
            Err(AddIpAddrSubnetError::Exists),
        );
        assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(contains_addr(&ctx.core_ctx, &device, ip2));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip3));
    }

    fn receive_simple_ip_packet_test<A: IpAddress>(
        ctx: &mut crate::testutil::FakeCtx,
        device: &DeviceId<crate::testutil::FakeBindingsCtx>,
        src_ip: A,
        dst_ip: A,
        expected: u64,
    ) where
        A::Version: TestIpExt + crate::IpExt,
    {
        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(<<A::Version as IpExt>::PacketBuilder as IpPacketBuilder<_>>::new(
                src_ip,
                dst_ip,
                64,
                IpProto::Tcp.into(),
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .into_inner();

        ctx.test_api().receive_ip_packet::<A::Version, _>(
            device,
            Some(FrameDestination::Individual { local: true }),
            buf,
        );
        assert_eq!(
            ctx.core_ctx.ip_counters::<A::Version>().dispatch_receive_ip_packet.get(),
            expected
        );
    }

    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    #[ip_test]
    fn test_multiple_ip_addresses<I: Ip + TestIpExt + crate::IpExt>() {
        let config = I::TEST_ADDRS;
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);

        let ip1 = I::get_other_ip_address(1);
        let ip2 = I::get_other_ip_address(2);
        let from_ip = I::get_other_ip_address(3).get();

        assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip2));

        // Should not receive packets on any IP.
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 0);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 0);

        let as1 = AddrSubnet::new(ip1.get(), I::Addr::BYTES * 8).unwrap();
        // Add ip1 to device.
        ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as1).unwrap();
        assert!(contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(!contains_addr(&ctx.core_ctx, &device, ip2));

        // Should receive packets on ip1 but not ip2
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 1);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 1);

        // Add ip2 to device.
        ctx.core_api()
            .device_ip::<I>()
            .add_ip_addr_subnet(&device, AddrSubnet::new(ip2.get(), I::Addr::BYTES * 8).unwrap())
            .unwrap();
        assert!(contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(contains_addr(&ctx.core_ctx, &device, ip2));

        // Should receive packets on both ips
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 2);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 3);

        // Remove ip1
        assert_eq!(
            ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip1).unwrap().into_removed(),
            as1
        );
        assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
        assert!(contains_addr(&ctx.core_ctx, &device, ip2));

        // Should receive packets on ip2
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 3);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 4);
    }

    /// Test that we can join and leave a multicast group, but we only truly
    /// leave it after calling `leave_ip_multicast` the same number of times as
    /// `join_ip_multicast`.
    #[ip_test]
    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    fn test_ip_join_leave_multicast_addr_ref_count<I: Ip + TestIpExt + crate::IpExt>() {
        let config = I::TEST_ADDRS;
        let mut ctx = crate::testutil::FakeCtx::default();

        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);
        let multicast_addr = I::get_multicast_addr(3);
        let mut test_api = ctx.test_api();

        // Should not be in the multicast group yet.
        assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));

        // Join the multicast group.
        test_api.join_ip_multicast(&device, multicast_addr);
        assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

        // Leave the multicast group.
        test_api.leave_ip_multicast(&device, multicast_addr);
        assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));

        // Join the multicst group.
        test_api.join_ip_multicast(&device, multicast_addr);
        assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

        // Join it again...
        test_api.join_ip_multicast(&device, multicast_addr);
        assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

        // Leave it (still in it because we joined twice).
        test_api.leave_ip_multicast(&device, multicast_addr);
        assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

        // Leave it again... (actually left now).
        test_api.leave_ip_multicast(&device, multicast_addr);
        assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));
    }

    /// Test leaving a multicast group a device has not yet joined.
    ///
    /// # Panics
    ///
    /// This method should always panic as leaving an unjoined multicast group
    /// is a panic condition.
    #[ip_test]
    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    #[should_panic(expected = "attempted to leave IP multicast group we were not a member of:")]
    fn test_ip_leave_unjoined_multicast<I: Ip + TestIpExt + crate::IpExt>() {
        let config = I::TEST_ADDRS;
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);
        let multicast_addr = I::get_multicast_addr(3);

        let mut test_api = ctx.test_api();

        // Should not be in the multicast group yet.
        assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));

        // Leave it (this should panic).
        test_api.leave_ip_multicast(&device, multicast_addr);
    }

    #[test]
    fn test_ipv6_duplicate_solicited_node_address() {
        // Test that we still receive packets destined to a solicited-node
        // multicast address of an IP address we deleted because another
        // (distinct) IP address that is still assigned uses the same
        // solicited-node multicast address.

        let config = Ipv6::TEST_ADDRS;
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);

        let ip1 = SpecifiedAddr::new(Ipv6Addr::new([0, 0, 0, 1, 0, 0, 0, 1])).unwrap();
        let ip2 = SpecifiedAddr::new(Ipv6Addr::new([0, 0, 0, 2, 0, 0, 0, 1])).unwrap();
        let from_ip = Ipv6Addr::new([0, 0, 0, 3, 0, 0, 0, 1]);

        // ip1 and ip2 are not equal but their solicited node addresses are the
        // same.
        assert_ne!(ip1, ip2);
        assert_eq!(ip1.to_solicited_node_address(), ip2.to_solicited_node_address());
        let sn_addr = ip1.to_solicited_node_address().get();

        let addr_sub1 = AddrSubnet::new(ip1.get(), 64).unwrap();
        let addr_sub2 = AddrSubnet::new(ip2.get(), 64).unwrap();

        assert_eq!(ctx.core_ctx.ip_counters::<Ipv6>().dispatch_receive_ip_packet.get(), 0);

        // Add ip1 to the device.
        //
        // Should get packets destined for the solicited node address and ip1.
        ctx.core_api().device_ip::<Ipv6>().add_ip_addr_subnet(&device, addr_sub1).unwrap();

        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 1);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 1);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, sn_addr, 2);

        // Add ip2 to the device.
        //
        // Should get packets destined for the solicited node address, ip1 and
        // ip2.
        ctx.core_api().device_ip::<Ipv6>().add_ip_addr_subnet(&device, addr_sub2).unwrap();
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 3);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 4);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, sn_addr, 5);

        // Remove ip1 from the device.
        //
        // Should get packets destined for the solicited node address and ip2.
        assert_eq!(
            ctx.core_api().device_ip::<Ipv6>().del_ip_addr(&device, ip1).unwrap().into_removed(),
            addr_sub1
        );
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 5);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 6);
        receive_simple_ip_packet_test(&mut ctx, &device, from_ip, sn_addr, 7);
    }

    #[test]
    fn test_add_ip_addr_subnet_link_local() {
        // Test that `add_ip_addr_subnet` allows link-local addresses.

        let config = Ipv6::TEST_ADDRS;
        let mut ctx = crate::testutil::FakeCtx::default();

        let eth_device =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: config.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            );
        let device = eth_device.clone().into();
        let eth_device = eth_device.device_state();

        // Enable the device and configure it to generate a link-local address.
        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device,
                Ipv6DeviceConfigurationUpdate {
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        ..Default::default()
                    }),
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();

        // Verify that there is a single assigned address.
        assert_eq!(
            eth_device
                .ip
                .ip_state::<Ipv6>()
                .addrs()
                .read()
                .iter()
                .map(|entry| entry.addr_sub().addr().get())
                .collect::<Vec<UnicastAddr<_>>>(),
            [config.local_mac.to_ipv6_link_local().addr().get()]
        );
        ctx.core_api()
            .device_ip::<Ipv6>()
            .add_ip_addr_subnet(
                &device,
                AddrSubnet::new(Ipv6::LINK_LOCAL_UNICAST_SUBNET.network(), 128).unwrap(),
            )
            .unwrap();
        // Assert that the new address got added.
        let addr_subs: Vec<_> = eth_device
            .ip
            .ip_state::<Ipv6>()
            .addrs()
            .read()
            .iter()
            .map(|entry| entry.addr_sub().addr().into())
            .collect::<Vec<Ipv6Addr>>();
        assert_eq!(
            addr_subs,
            [
                config.local_mac.to_ipv6_link_local().addr().get(),
                Ipv6::LINK_LOCAL_UNICAST_SUBNET.network()
            ]
        );
    }
}
