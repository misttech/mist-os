// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::collections::HashMap;
use core::cmp::Ordering;
use core::fmt::Debug;
use core::hash::Hash;
use core::marker::PhantomData;
use core::num::{NonZeroU16, NonZeroU8};
use core::sync::atomic::{self, AtomicU16};

use const_unwrap::const_unwrap_option;
use derivative::Derivative;
use explicit::ResultExt as _;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use log::{debug, error, trace};
use net_types::ip::{
    GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr, Mtu, Subnet,
};
use net_types::{
    MulticastAddr, MulticastAddress, SpecifiedAddr, SpecifiedAddress as _, UnicastAddr, Witness,
};
use netstack3_base::socket::SocketIpAddrExt as _;
use netstack3_base::sync::{Mutex, PrimaryRc, RwLock, StrongRc, WeakRc};
use netstack3_base::{
    AnyDevice, BroadcastIpExt, CoreTimerContext, Counter, CounterContext, DeviceIdContext,
    DeviceIdentifier as _, ErrorAndSerializer, EventContext, FrameDestination, HandleableTimer,
    Inspectable, Inspector, InstantContext, IpExt, NestedIntoCoreTimerCtx, NotFoundError,
    RngContext, SendFrameErrorReason, StrongDeviceIdentifier, TimerBindingsTypes, TimerContext,
    TimerHandler, TracingContext, WrapBroadcastMarker,
};
use netstack3_filter::{
    self as filter, ConntrackConnection, FilterBindingsContext, FilterBindingsTypes,
    FilterHandler as _, FilterIpContext, FilterIpMetadata, FilterTimerId, ForwardedPacket,
    IngressVerdict, IpPacket, NatType, NestedWithInnerIpPacket, TransportPacketSerializer, Tuple,
};
use packet::{Buf, BufferMut, ParseMetadata, Serializer};
use packet_formats::error::IpParseError;
use packet_formats::ip::{DscpAndEcn, IpPacket as _, IpPacketBuilder as _};
use packet_formats::ipv4::{Ipv4FragmentType, Ipv4Packet};
use packet_formats::ipv6::Ipv6Packet;
use thiserror::Error;
use zerocopy::ByteSlice;

use crate::internal::device::slaac::SlaacCounters;
use crate::internal::device::state::{
    IpDeviceStateBindingsTypes, IpDeviceStateIpExt, Ipv6AddressFlags, Ipv6AddressState,
};
use crate::internal::device::{
    self, IpAddressId as _, IpDeviceAddr, IpDeviceBindingsContext, IpDeviceIpExt,
    IpDeviceSendContext,
};
use crate::internal::gmp::GmpQueryHandler;
use crate::internal::icmp::{
    IcmpBindingsTypes, IcmpErrorHandler, IcmpHandlerIpExt, Icmpv4Error, Icmpv4ErrorKind,
    Icmpv4State, Icmpv4StateBuilder, Icmpv6ErrorKind, Icmpv6State, Icmpv6StateBuilder,
};
use crate::internal::ipv6::Ipv6PacketAction;
use crate::internal::multicast_forwarding::route::{MulticastRouteIpExt, MulticastRouteTargets};
use crate::internal::multicast_forwarding::state::{
    MulticastForwardingState, MulticastForwardingStateContext,
};
use crate::internal::multicast_forwarding::MulticastForwardingDeviceContext;
use crate::internal::path_mtu::{PmtuBindingsTypes, PmtuCache, PmtuTimerId};
use crate::internal::raw::counters::RawIpSocketCounters;
use crate::internal::raw::{RawIpSocketHandler, RawIpSocketMap, RawIpSocketsBindingsTypes};
use crate::internal::reassembly::{
    FragmentBindingsTypes, FragmentHandler, FragmentProcessingState, FragmentTimerId,
    FragmentablePacket, IpPacketFragmentCache,
};
use crate::internal::routing::{IpRoutingDeviceContext, RoutingTable};
use crate::internal::socket::{IpSocketBindingsContext, IpSocketContext, IpSocketHandler};
use crate::internal::types::{self, Destination, NextHop, ResolvedRoute, RoutableIpAddr};
use crate::internal::{ipv6, multicast_forwarding};

#[cfg(test)]
mod tests;

/// Default IPv4 TTL.
pub const DEFAULT_TTL: NonZeroU8 = const_unwrap_option(NonZeroU8::new(64));

/// Hop limits for packets sent to multicast and unicast destinations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct HopLimits {
    pub unicast: NonZeroU8,
    pub multicast: NonZeroU8,
}

/// Default hop limits for sockets.
pub const DEFAULT_HOP_LIMITS: HopLimits =
    HopLimits { unicast: DEFAULT_TTL, multicast: const_unwrap_option(NonZeroU8::new(1)) };

/// The IPv6 subnet that contains all addresses; `::/0`.
// Safe because 0 is less than the number of IPv6 address bits.
pub const IPV6_DEFAULT_SUBNET: Subnet<Ipv6Addr> =
    unsafe { Subnet::new_unchecked(Ipv6::UNSPECIFIED_ADDRESS, 0) };

/// An error encountered when receiving a transport-layer packet.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum TransportReceiveError {
    ProtocolUnsupported,
    PortUnreachable,
}

/// Sidecar metadata passed along with the packet.
///
/// NOTE: This metadata may be reset after a packet goes through reassembly, and
/// consumers must be able to handle this case.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct IpLayerPacketMetadata<I: packet_formats::ip::IpExt, BT: FilterBindingsTypes> {
    conntrack_connection: Option<ConntrackConnection<I, BT>>,
    #[cfg(debug_assertions)]
    drop_check: IpLayerPacketMetadataDropCheck,
}

/// A type that asserts, on drop, that it was intentionally being dropped.
///
/// NOTE: Unfortunately, debugging this requires backtraces, since track_caller
/// won't do what we want (https://github.com/rust-lang/rust/issues/116942).
/// Since this is only enabled in debug, the assumption is that stacktraces are
/// enabled.
#[cfg(debug_assertions)]
#[derive(Default)]
struct IpLayerPacketMetadataDropCheck {
    okay_to_drop: bool,
}

impl<I: packet_formats::ip::IpExt, BT: FilterBindingsTypes> IpLayerPacketMetadata<I, BT> {
    #[cfg(debug_assertions)]
    pub(crate) fn acknowledge_drop(&mut self) {
        self.drop_check.okay_to_drop = true;
    }

    #[cfg(not(debug_assertions))]
    pub(crate) fn acknowledge_drop(&mut self) {}
}

#[cfg(debug_assertions)]
impl Drop for IpLayerPacketMetadataDropCheck {
    fn drop(&mut self) {
        if !self.okay_to_drop {
            panic!(
                "IpLayerPacketMetadata dropped without acknowledgement.  https://fxbug.dev/334127474"
            );
        }
    }
}

impl<I: packet_formats::ip::IpExt, BT: FilterBindingsTypes> FilterIpMetadata<I, BT>
    for IpLayerPacketMetadata<I, BT>
{
    fn take_conntrack_connection(&mut self) -> Option<ConntrackConnection<I, BT>> {
        self.conntrack_connection.take()
    }

    fn replace_conntrack_connection(
        &mut self,
        conn: ConntrackConnection<I, BT>,
    ) -> Option<ConntrackConnection<I, BT>> {
        self.conntrack_connection.replace(conn)
    }
}

/// Send errors observed at or above the IP layer that carry a serializer.
pub type IpSendFrameError<S> = ErrorAndSerializer<IpSendFrameErrorReason, S>;

/// Send error cause for [`IpSendFrameError`].
#[derive(Debug, PartialEq)]
pub enum IpSendFrameErrorReason {
    /// Error comes from the device layer.
    Device(SendFrameErrorReason),
    /// The frame's source or destination address is in the loopback subnet, but
    /// the target device is not the loopback device.
    IllegalLoopbackAddress,
}

impl From<SendFrameErrorReason> for IpSendFrameErrorReason {
    fn from(value: SendFrameErrorReason) -> Self {
        Self::Device(value)
    }
}

/// Informs the transport layer of parameters for transparent local delivery.
#[derive(Debug, GenericOverIp, Clone)]
#[generic_over_ip(I, Ip)]
pub struct TransparentLocalDelivery<I: IpExt> {
    /// The local delivery address.
    pub addr: SpecifiedAddr<I::Addr>,
    /// The local delivery port.
    pub port: NonZeroU16,
}

/// Meta information for an incoming packet.
#[derive(Debug, Derivative, GenericOverIp, Clone)]
#[derivative(Default(bound = ""))]
#[generic_over_ip(I, Ip)]
pub struct ReceiveIpPacketMeta<I: IpExt> {
    /// Indicates that the packet was sent to a broadcast address.
    pub broadcast: Option<I::BroadcastMarker>,

    /// Destination overrides for the transparent proxy.
    pub transport_override: Option<TransparentLocalDelivery<I>>,
}

/// The execution context provided by a transport layer protocol to the IP
/// layer.
///
/// An implementation for `()` is provided which indicates that a particular
/// transport layer protocol is unsupported.
pub trait IpTransportContext<I: IpExt, BC, CC: DeviceIdContext<AnyDevice> + ?Sized> {
    /// Receive an ICMP error message.
    ///
    /// All arguments beginning with `original_` are fields from the IP packet
    /// that triggered the error. The `original_body` is provided here so that
    /// the error can be associated with a transport-layer socket. `device`
    /// identifies the device that received the ICMP error message packet.
    ///
    /// While ICMPv4 error messages are supposed to contain the first 8 bytes of
    /// the body of the offending packet, and ICMPv6 error messages are supposed
    /// to contain as much of the offending packet as possible without violating
    /// the IPv6 minimum MTU, the caller does NOT guarantee that either of these
    /// hold. It is `receive_icmp_error`'s responsibility to handle any length
    /// of `original_body`, and to perform any necessary validation.
    fn receive_icmp_error(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        original_body: &[u8],
        err: I::ErrorCode,
    );

    /// Receive a transport layer packet in an IP packet.
    ///
    /// In the event of an unreachable port, `receive_ip_packet` returns the
    /// buffer in its original state (with the transport packet un-parsed) in
    /// the `Err` variant.
    fn receive_ip_packet<B: BufferMut>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        src_ip: I::RecvSrcAddr,
        dst_ip: SpecifiedAddr<I::Addr>,
        buffer: B,
        meta: ReceiveIpPacketMeta<I>,
    ) -> Result<(), (B, TransportReceiveError)>;
}

impl<I: IpExt, BC, CC: DeviceIdContext<AnyDevice> + ?Sized> IpTransportContext<I, BC, CC> for () {
    fn receive_icmp_error(
        _core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _device: &CC::DeviceId,
        _original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        _original_dst_ip: SpecifiedAddr<I::Addr>,
        _original_body: &[u8],
        err: I::ErrorCode,
    ) {
        trace!("IpTransportContext::receive_icmp_error: Received ICMP error message ({:?}) for unsupported IP protocol", err);
    }

    fn receive_ip_packet<B: BufferMut>(
        _core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _device: &CC::DeviceId,
        _src_ip: I::RecvSrcAddr,
        _dst_ip: SpecifiedAddr<I::Addr>,
        buffer: B,
        _meta: ReceiveIpPacketMeta<I>,
    ) -> Result<(), (B, TransportReceiveError)> {
        Err((buffer, TransportReceiveError::ProtocolUnsupported))
    }
}

/// The base execution context provided by the IP layer to transport layer
/// protocols.
pub trait BaseTransportIpContext<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// The iterator given to
    /// [`BaseTransportIpContext::with_devices_with_assigned_addr`].
    type DevicesWithAddrIter<'s>: Iterator<Item = Self::DeviceId>;

    /// Is this one of our local addresses, and is it in the assigned state?
    ///
    /// Calls `cb` with an iterator over all the local interfaces for which
    /// `addr` is an associated address, and, for IPv6, for which it is in the
    /// "assigned" state.
    fn with_devices_with_assigned_addr<O, F: FnOnce(Self::DevicesWithAddrIter<'_>) -> O>(
        &mut self,
        addr: SpecifiedAddr<I::Addr>,
        cb: F,
    ) -> O;

    /// Get default hop limits.
    ///
    /// If `device` is not `None` and exists, its hop limits will be returned.
    /// Otherwise the system defaults are returned.
    fn get_default_hop_limits(&mut self, device: Option<&Self::DeviceId>) -> HopLimits;

    /// Confirms the provided destination is reachable.
    ///
    /// Implementations must retrieve the next hop given the provided
    /// destination and confirm neighbor reachability for the resolved target
    /// device.
    fn confirm_reachable_with_destination(
        &mut self,
        bindings_ctx: &mut BC,
        dst: SpecifiedAddr<I::Addr>,
        device: Option<&Self::DeviceId>,
    );

    /// Gets the original destination for the tracked connection indexed by
    /// `tuple`, which includes the source and destination addresses and
    /// transport-layer ports as well as the transport protocol number.
    fn get_original_destination(&mut self, tuple: &Tuple<I>) -> Option<(I::Addr, u16)>;
}

/// A marker trait for the traits required by the transport layer from the IP
/// layer.
pub trait TransportIpContext<I: IpExt, BC>:
    BaseTransportIpContext<I, BC> + IpSocketHandler<I, BC>
{
}

impl<I, CC, BC> TransportIpContext<I, BC> for CC
where
    I: IpExt,
    CC: BaseTransportIpContext<I, BC> + IpSocketHandler<I, BC>,
{
}

/// Abstraction over the ability to join and leave multicast groups.
pub trait MulticastMembershipHandler<I: Ip, BC>: DeviceIdContext<AnyDevice> {
    /// Requests that the specified device join the given multicast group.
    ///
    /// If this method is called multiple times with the same device and
    /// address, the device will remain joined to the multicast group until
    /// [`MulticastTransportIpContext::leave_multicast_group`] has been called
    /// the same number of times.
    fn join_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    );

    /// Requests that the specified device leave the given multicast group.
    ///
    /// Each call to this method must correspond to an earlier call to
    /// [`MulticastTransportIpContext::join_multicast_group`]. The device
    /// remains a member of the multicast group so long as some call to
    /// `join_multicast_group` has been made without a corresponding call to
    /// `leave_multicast_group`.
    fn leave_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    );

    /// Selects a default device with which to join the given multicast group.
    ///
    /// The selection is made by consulting the routing table; If there is no
    /// route available to the given address, an error is returned.
    fn select_device_for_multicast_group(
        &mut self,
        addr: MulticastAddr<I::Addr>,
    ) -> Result<Self::DeviceId, ResolveRouteError>;
}

// TODO(joshlf): With all 256 protocol numbers (minus reserved ones) given their
// own associated type in both traits, running `cargo check` on a 2018 MacBook
// Pro takes over a minute. Eventually - and before we formally publish this as
// a library - we should identify the bottleneck in the compiler and optimize
// it. For the time being, however, we only support protocol numbers that we
// actually use (TCP and UDP).

/// Enables a blanket implementation of [`TransportIpContext`].
///
/// Implementing this marker trait for a type enables a blanket implementation
/// of `TransportIpContext` given the other requirements are met.
pub trait UseTransportIpContextBlanket {}

/// An iterator supporting the blanket implementation of
/// [`BaseTransportIpContext::with_devices_with_assigned_addr`].
pub struct AssignedAddressDeviceIterator<Iter, I, D>(Iter, PhantomData<(I, D)>);

impl<Iter, I, D> Iterator for AssignedAddressDeviceIterator<Iter, I, D>
where
    Iter: Iterator<Item = (D, I::AddressStatus)>,
    I: IpLayerIpExt,
{
    type Item = D;
    fn next(&mut self) -> Option<D> {
        let Self(iter, PhantomData) = self;
        iter.by_ref().find_map(|(device, state)| is_unicast_assigned::<I>(&state).then_some(device))
    }
}

impl<
        I: IpLayerIpExt,
        BC: FilterBindingsContext,
        CC: IpDeviceContext<I, BC>
            + IpSocketHandler<I, BC>
            + IpStateContext<I, BC>
            + FilterIpContext<I, BC>
            + UseTransportIpContextBlanket,
    > BaseTransportIpContext<I, BC> for CC
{
    type DevicesWithAddrIter<'s> =
        AssignedAddressDeviceIterator<CC::DeviceAndAddressStatusIter<'s>, I, CC::DeviceId>;

    fn with_devices_with_assigned_addr<O, F: FnOnce(Self::DevicesWithAddrIter<'_>) -> O>(
        &mut self,
        addr: SpecifiedAddr<I::Addr>,
        cb: F,
    ) -> O {
        self.with_address_statuses(addr, |it| cb(AssignedAddressDeviceIterator(it, PhantomData)))
    }

    fn get_default_hop_limits(&mut self, device: Option<&Self::DeviceId>) -> HopLimits {
        match device {
            Some(device) => HopLimits {
                unicast: IpDeviceStateContext::<I, _>::get_hop_limit(self, device),
                ..DEFAULT_HOP_LIMITS
            },
            None => DEFAULT_HOP_LIMITS,
        }
    }

    fn confirm_reachable_with_destination(
        &mut self,
        bindings_ctx: &mut BC,
        dst: SpecifiedAddr<I::Addr>,
        device: Option<&Self::DeviceId>,
    ) {
        match lookup_route_table(self, device, dst.get()) {
            Some(Destination { next_hop, device }) => {
                let neighbor = match next_hop {
                    NextHop::RemoteAsNeighbor => dst,
                    NextHop::Gateway(gateway) => gateway,
                    NextHop::Broadcast(marker) => {
                        I::map_ip::<_, ()>(
                            WrapBroadcastMarker(marker),
                            |WrapBroadcastMarker(())| {
                                debug!(
                                    "can't confirm {dst:?}@{device:?} as reachable: \
                                                 dst is a broadcast address"
                                );
                            },
                            |WrapBroadcastMarker(never)| match never {},
                        );
                        return;
                    }
                };
                self.confirm_reachable(bindings_ctx, &device, neighbor);
            }
            None => {
                debug!("can't confirm {dst:?}@{device:?} as reachable: no route");
            }
        }
    }

    fn get_original_destination(&mut self, tuple: &Tuple<I>) -> Option<(I::Addr, u16)> {
        self.with_filter_state(|state| {
            let conn = state.conntrack.get_connection(&tuple)?;

            // If NAT has not been configured for the connection, return None.
            let _: NatType = conn.external_data().nat_type()?;

            // The tuple marking the original direction of the connection is
            // never modified by NAT. This means it can be used to recover the
            // destination before NAT was performed.
            let original = conn.original_tuple();
            Some((original.dst_addr, original.dst_port_or_id))
        })
    }
}

/// The status of an IP address on an interface.
#[derive(Debug, PartialEq)]
#[allow(missing_docs)]
pub enum AddressStatus<S> {
    Present(S),
    Unassigned,
}

impl<S> AddressStatus<S> {
    fn into_present(self) -> Option<S> {
        match self {
            Self::Present(s) => Some(s),
            Self::Unassigned => None,
        }
    }
}

impl AddressStatus<Ipv4PresentAddressStatus> {
    /// Creates an IPv4 `AddressStatus` for `addr` on `device`.
    pub fn from_context_addr_v4<
        BC: IpDeviceStateBindingsTypes,
        CC: device::IpDeviceStateContext<Ipv4, BC> + GmpQueryHandler<Ipv4, BC>,
    >(
        core_ctx: &mut CC,
        device: &CC::DeviceId,
        addr: SpecifiedAddr<Ipv4Addr>,
    ) -> AddressStatus<Ipv4PresentAddressStatus> {
        if addr.is_limited_broadcast() {
            return AddressStatus::Present(Ipv4PresentAddressStatus::LimitedBroadcast);
        }

        if MulticastAddr::new(addr.get())
            .is_some_and(|addr| GmpQueryHandler::gmp_is_in_group(core_ctx, device, addr))
        {
            return AddressStatus::Present(Ipv4PresentAddressStatus::Multicast);
        }

        core_ctx.with_address_ids(device, |mut addrs, _core_ctx| {
            addrs
                .find_map(|addr_id| {
                    let dev_addr = addr_id.addr_sub();
                    let (dev_addr, subnet) = dev_addr.addr_subnet();

                    if dev_addr == addr {
                        Some(AddressStatus::Present(Ipv4PresentAddressStatus::Unicast))
                    } else if addr.get() == subnet.broadcast() {
                        Some(AddressStatus::Present(Ipv4PresentAddressStatus::SubnetBroadcast))
                    } else if device.is_loopback() && subnet.contains(addr.as_ref()) {
                        Some(AddressStatus::Present(Ipv4PresentAddressStatus::LoopbackSubnet))
                    } else {
                        None
                    }
                })
                .unwrap_or(AddressStatus::Unassigned)
        })
    }
}

impl AddressStatus<Ipv6PresentAddressStatus> {
    /// /// Creates an IPv6 `AddressStatus` for `addr` on `device`.
    pub fn from_context_addr_v6<
        BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
        CC: device::Ipv6DeviceContext<BC> + GmpQueryHandler<Ipv6, BC>,
    >(
        core_ctx: &mut CC,
        device: &CC::DeviceId,
        addr: SpecifiedAddr<Ipv6Addr>,
    ) -> AddressStatus<Ipv6PresentAddressStatus> {
        if MulticastAddr::new(addr.get())
            .is_some_and(|addr| GmpQueryHandler::gmp_is_in_group(core_ctx, device, addr))
        {
            return AddressStatus::Present(Ipv6PresentAddressStatus::Multicast);
        }

        let addr_id = match core_ctx.get_address_id(device, addr) {
            Ok(o) => o,
            Err(NotFoundError) => return AddressStatus::Unassigned,
        };

        let assigned = core_ctx.with_ip_address_state(
            device,
            &addr_id,
            |Ipv6AddressState {
                 flags: Ipv6AddressFlags { deprecated: _, assigned },
                 config: _,
             }| { *assigned },
        );

        if assigned {
            AddressStatus::Present(Ipv6PresentAddressStatus::UnicastAssigned)
        } else {
            AddressStatus::Present(Ipv6PresentAddressStatus::UnicastTentative)
        }
    }
}

impl<S: GenericOverIp<I>, I: Ip> GenericOverIp<I> for AddressStatus<S> {
    type Type = AddressStatus<S::Type>;
}

/// The status of an IPv4 address.
#[derive(Debug, PartialEq)]
#[allow(missing_docs)]
pub enum Ipv4PresentAddressStatus {
    LimitedBroadcast,
    SubnetBroadcast,
    Multicast,
    Unicast,
    /// This status indicates that the queried device was Loopback. The address
    /// belongs to a subnet that is assigned to the interface. This status
    /// takes lower precedence than `Unicast` and `SubnetBroadcast``, E.g. if
    /// the loopback device is assigned `127.0.0.1/8`:
    ///   * address `127.0.0.1` -> `Unicast`
    ///   * address `127.0.0.2` -> `LoopbackSubnet`
    ///   * address `127.255.255.255` -> `SubnetBroadcast`
    /// This exists for Linux conformance, which on the Loopback device,
    /// considers an IPv4 address assigned if it belongs to one of the device's
    /// assigned subnets.
    LoopbackSubnet,
}

impl Ipv4PresentAddressStatus {
    fn to_broadcast_marker(&self) -> Option<<Ipv4 as BroadcastIpExt>::BroadcastMarker> {
        match self {
            Self::LimitedBroadcast | Self::SubnetBroadcast => Some(()),
            Self::Multicast | Self::Unicast | Self::LoopbackSubnet => None,
        }
    }
}

/// The status of an IPv6 address.
#[derive(Debug, PartialEq)]
#[allow(missing_docs)]
pub enum Ipv6PresentAddressStatus {
    Multicast,
    UnicastAssigned,
    UnicastTentative,
}

/// An extension trait providing IP layer properties.
pub trait IpLayerIpExt: IpExt + MulticastRouteIpExt {
    /// IP Address status.
    type AddressStatus: Debug;
    /// IP Address state.
    type State<StrongDeviceId: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>: AsRef<
        IpStateInner<Self, StrongDeviceId, BT>,
    >;
    /// State kept for packet identifiers.
    type PacketIdState;
    /// The type of a single packet identifier.
    type PacketId;
    /// Receive counters.
    type RxCounters: Default + Inspectable;
    /// Produces the next packet ID from the state.
    fn next_packet_id_from_state(state: &Self::PacketIdState) -> Self::PacketId;
}

impl IpLayerIpExt for Ipv4 {
    type AddressStatus = Ipv4PresentAddressStatus;
    type State<StrongDeviceId: StrongDeviceIdentifier, BT: IpLayerBindingsTypes> =
        Ipv4State<StrongDeviceId, BT>;
    type PacketIdState = AtomicU16;
    type PacketId = u16;
    type RxCounters = Ipv4RxCounters;
    fn next_packet_id_from_state(next_packet_id: &Self::PacketIdState) -> Self::PacketId {
        // Relaxed ordering as we only need atomicity without synchronization. See
        // https://en.cppreference.com/w/cpp/atomic/memory_order#Relaxed_ordering
        // for more details.
        next_packet_id.fetch_add(1, atomic::Ordering::Relaxed)
    }
}

impl IpLayerIpExt for Ipv6 {
    type AddressStatus = Ipv6PresentAddressStatus;
    type State<StrongDeviceId: StrongDeviceIdentifier, BT: IpLayerBindingsTypes> =
        Ipv6State<StrongDeviceId, BT>;
    type PacketIdState = ();
    type PacketId = ();
    type RxCounters = Ipv6RxCounters;
    fn next_packet_id_from_state((): &Self::PacketIdState) -> Self::PacketId {
        ()
    }
}

/// The state context provided to the IP layer.
pub trait IpStateContext<I: IpLayerIpExt, BC>: DeviceIdContext<AnyDevice> {
    /// The inner device id context.
    type IpDeviceIdCtx<'a>: DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + IpRoutingDeviceContext<I>
        + IpDeviceStateContext<I, BC>;

    /// Gets the main table ID.
    fn main_table_id(&self) -> RoutingTableId<I, Self::DeviceId>;

    /// Gets mutable access to all the routing tables that currently exist.
    fn with_ip_routing_tables_mut<
        O,
        F: FnOnce(
            &mut HashMap<
                RoutingTableId<I, Self::DeviceId>,
                PrimaryRc<RwLock<RoutingTable<I, Self::DeviceId>>>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    // TODO(https://fxbug.dev/354724171): Remove this function when we no longer
    // make routing decisions starting from the main table.
    /// Calls the function with an immutable reference to IP routing table.
    fn with_main_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let main_table_id = self.main_table_id();
        self.with_ip_routing_table(&main_table_id, cb)
    }

    // TODO(https://fxbug.dev/341194323): Remove this function when we no longer
    // only update the main routing table by default.
    /// Calls the function with a mutable reference to IP routing table.
    fn with_main_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let main_table_id = self.main_table_id();
        self.with_ip_routing_table_mut(&main_table_id, cb)
    }

    /// Calls the function with an immutable reference to IP routing table.
    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        table_id: &RoutingTableId<I, Self::DeviceId>,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to IP routing table.
    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut RoutingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        table_id: &RoutingTableId<I, Self::DeviceId>,
        cb: F,
    ) -> O;
}

/// Provides access to an IP device's state for the IP layer.
pub trait IpDeviceStateContext<I: IpLayerIpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Calls the callback with the next packet ID.
    fn with_next_packet_id<O, F: FnOnce(&I::PacketIdState) -> O>(&self, cb: F) -> O;

    /// Returns the best local address for communicating with the remote.
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: Option<SpecifiedAddr<I::Addr>>,
    ) -> Option<IpDeviceAddr<I::Addr>>;

    /// Returns the hop limit.
    fn get_hop_limit(&mut self, device_id: &Self::DeviceId) -> NonZeroU8;

    /// Gets the status of an address.
    ///
    /// Only the specified device will be checked for the address. Returns
    /// [`AddressStatus::Unassigned`] if the address is not assigned to the
    /// device.
    fn address_status_for_device(
        &mut self,
        addr: SpecifiedAddr<I::Addr>,
        device_id: &Self::DeviceId,
    ) -> AddressStatus<I::AddressStatus>;
}

/// The IP device context provided to the IP layer.
pub trait IpDeviceContext<I: IpLayerIpExt, BC>: IpDeviceStateContext<I, BC> {
    /// Is the device enabled?
    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool;

    /// The iterator provided to [`IpDeviceContext::with_address_statuses`].
    type DeviceAndAddressStatusIter<'a>: Iterator<Item = (Self::DeviceId, I::AddressStatus)>;

    /// Provides access to the status of an address.
    ///
    /// Calls the provided callback with an iterator over the devices for which
    /// the address is assigned and the status of the assignment for each
    /// device.
    fn with_address_statuses<F: FnOnce(Self::DeviceAndAddressStatusIter<'_>) -> R, R>(
        &mut self,
        addr: SpecifiedAddr<I::Addr>,
        cb: F,
    ) -> R;

    /// Returns true iff the device has unicast forwarding enabled.
    fn is_device_unicast_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool;

    /// Returns the MTU of the device.
    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu;

    /// Confirm transport-layer forward reachability to the specified neighbor
    /// through the specified device.
    fn confirm_reachable(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    );
}

/// Events observed at the IP layer.
#[derive(Debug, Eq, Hash, PartialEq, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub enum IpLayerEvent<DeviceId, I: Ip> {
    /// A route needs to be added.
    AddRoute(types::AddableEntry<I::Addr, DeviceId>),
    /// Routes matching these specifiers need to be removed.
    RemoveRoutes {
        /// Destination subnet
        subnet: Subnet<I::Addr>,
        /// Outgoing interface
        device: DeviceId,
        /// Gateway/next-hop
        gateway: Option<SpecifiedAddr<I::Addr>>,
    },
}

impl<DeviceId, I: Ip> IpLayerEvent<DeviceId, I> {
    /// Changes the device id type with `map`.
    pub fn map_device<N, F: FnOnce(DeviceId) -> N>(self, map: F) -> IpLayerEvent<N, I> {
        match self {
            IpLayerEvent::AddRoute(types::AddableEntry { subnet, device, gateway, metric }) => {
                IpLayerEvent::AddRoute(types::AddableEntry {
                    subnet,
                    device: map(device),
                    gateway,
                    metric,
                })
            }
            IpLayerEvent::RemoveRoutes { subnet, device, gateway } => {
                IpLayerEvent::RemoveRoutes { subnet, device: map(device), gateway }
            }
        }
    }
}

/// The bindings execution context for the IP layer.
pub trait IpLayerBindingsContext<I: Ip, DeviceId>:
    InstantContext + EventContext<IpLayerEvent<DeviceId, I>> + TracingContext + FilterBindingsContext
{
}
impl<
        I: Ip,
        DeviceId,
        BC: InstantContext
            + EventContext<IpLayerEvent<DeviceId, I>>
            + TracingContext
            + FilterBindingsContext,
    > IpLayerBindingsContext<I, DeviceId> for BC
{
}

/// A marker trait for bindings types at the IP layer.
pub trait IpLayerBindingsTypes: IcmpBindingsTypes + IpStateBindingsTypes {}
impl<BT: IcmpBindingsTypes + IpStateBindingsTypes> IpLayerBindingsTypes for BT {}

/// The execution context for the IP layer.
pub trait IpLayerContext<
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, <Self as DeviceIdContext<AnyDevice>>::DeviceId>,
>:
    IpStateContext<I, BC>
    + IpDeviceContext<I, BC>
    + MulticastForwardingStateContext<I>
    + MulticastForwardingDeviceContext<I>
{
}

impl<
        I: IpLayerIpExt,
        BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
        CC: IpStateContext<I, BC>
            + IpDeviceContext<I, BC>
            + MulticastForwardingStateContext<I>
            + MulticastForwardingDeviceContext<I>,
    > IpLayerContext<I, BC> for CC
{
}

fn is_unicast_assigned<I: IpLayerIpExt>(status: &I::AddressStatus) -> bool {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct WrapAddressStatus<'a, I: IpLayerIpExt>(&'a I::AddressStatus);

    I::map_ip(
        WrapAddressStatus(status),
        |WrapAddressStatus(status)| match status {
            Ipv4PresentAddressStatus::Unicast | Ipv4PresentAddressStatus::LoopbackSubnet => true,
            Ipv4PresentAddressStatus::LimitedBroadcast
            | Ipv4PresentAddressStatus::SubnetBroadcast
            | Ipv4PresentAddressStatus::Multicast => false,
        },
        |WrapAddressStatus(status)| match status {
            Ipv6PresentAddressStatus::UnicastAssigned => true,
            Ipv6PresentAddressStatus::Multicast | Ipv6PresentAddressStatus::UnicastTentative => {
                false
            }
        },
    )
}

fn is_local_assigned_address<
    I: Ip + IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC>,
>(
    core_ctx: &mut CC,
    device: &CC::DeviceId,
    addr: SpecifiedAddr<I::Addr>,
) -> bool {
    match core_ctx.address_status_for_device(addr, device) {
        AddressStatus::Present(status) => is_unicast_assigned::<I>(&status),
        AddressStatus::Unassigned => false,
    }
}

fn get_device_with_assigned_address<A, BC, CC>(
    core_ctx: &mut CC,
    addr: SpecifiedAddr<A>,
) -> Option<CC::DeviceId>
where
    A: IpAddress,
    A::Version: IpLayerIpExt,
    BC: IpLayerBindingsContext<A::Version, CC::DeviceId>,
    CC: IpDeviceStateContext<A::Version, BC> + IpDeviceContext<A::Version, BC>,
{
    core_ctx.with_address_statuses(addr, |mut it| {
        it.find_map(|(device, status)| is_unicast_assigned::<A::Version>(&status).then_some(device))
    })
}

// Returns the local IP address to use for sending packets from the
// given device to `addr`, restricting to `local_ip` if it is not
// `None`.
fn get_local_addr<
    I: Ip + IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC>,
>(
    core_ctx: &mut CC,
    local_ip: Option<IpDeviceAddr<I::Addr>>,
    device: &CC::DeviceId,
    remote_addr: Option<RoutableIpAddr<I::Addr>>,
    allow_non_local_src: bool,
) -> Result<IpDeviceAddr<I::Addr>, ResolveRouteError> {
    if let Some(local_ip) = local_ip {
        if allow_non_local_src {
            return Ok(local_ip);
        }
        is_local_assigned_address(core_ctx, device, local_ip.into())
            .then_some(local_ip)
            .ok_or(ResolveRouteError::NoSrcAddr)
    } else {
        core_ctx
            .get_local_addr_for_remote(device, remote_addr.map(Into::into))
            .ok_or(ResolveRouteError::NoSrcAddr)
    }
}

/// An error occurred while resolving the route to a destination
#[derive(Error, Copy, Clone, Debug, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub enum ResolveRouteError {
    /// A source address could not be selected.
    #[error("a source address could not be selected")]
    NoSrcAddr,
    /// The destination in unreachable.
    #[error("no route exists to the destination IP address")]
    Unreachable,
}

/// Returns the forwarding instructions for reaching the given destination.
///
/// If a `device` is specified, the resolved route is limited to those that
/// egress over the device.
///
/// If `src_ip` is specified the resolved route is limited to those that egress
/// over a device with the address assigned.
pub fn resolve_route_to_destination<
    I: Ip + IpDeviceStateIpExt + IpDeviceIpExt + IpLayerIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId> + IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC> + device::IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    device: Option<&CC::DeviceId>,
    src_ip: Option<IpDeviceAddr<I::Addr>>,
    dst_ip: Option<RoutableIpAddr<I::Addr>>,
    allow_non_local_src: bool,
) -> Result<ResolvedRoute<I, CC::DeviceId>, ResolveRouteError> {
    enum LocalDelivery<A, D> {
        WeakLoopback { dst_ip: A, device: D },
        StrongForDevice(D),
    }

    // Check if locally destined. If the destination is an address assigned
    // on an interface, and an egress interface wasn't specifically
    // selected, route via the loopback device. This lets us operate as a
    // strong host when an outgoing interface is explicitly requested while
    // still enabling local delivery via the loopback interface, which is
    // acting as a weak host. Note that if the loopback interface is
    // requested as an outgoing interface, route selection is still
    // performed as a strong host! This makes the loopback interface behave
    // more like the other interfaces on the system.
    //
    // TODO(https://fxbug.dev/42175703): Encode the delivery of locally-
    // destined packets to loopback in the route table.
    //
    // TODO(https://fxbug.dev/322539434): Linux is more permissive about
    // allowing cross-device local delivery even when SO_BINDTODEVICE or
    // link-local addresses are involved, and this behavior may need to be
    // emulated.
    let local_delivery_instructions: Option<LocalDelivery<RoutableIpAddr<I::Addr>, CC::DeviceId>> =
        match (device, dst_ip) {
            (Some(device), Some(dst_ip)) => {
                is_local_assigned_address(core_ctx, device, dst_ip.into())
                    .then_some(LocalDelivery::StrongForDevice(device.clone()))
            }
            (None, Some(dst_ip)) => {
                get_device_with_assigned_address(core_ctx, dst_ip.into()).map(|dst_device| {
                    // If either the source or destination addresses needs
                    // a zone ID, then use strong host to enforce that the
                    // source and destination addresses are assigned to the
                    // same interface.
                    if src_ip.is_some_and(|ip| ip.as_ref().must_have_zone())
                        || dst_ip.as_ref().must_have_zone()
                    {
                        LocalDelivery::StrongForDevice(dst_device)
                    } else {
                        LocalDelivery::WeakLoopback { dst_ip, device: dst_device }
                    }
                })
            }
            (_, None) => None,
        };

    if let Some(local_delivery) = local_delivery_instructions {
        let loopback = core_ctx.loopback_id().ok_or(ResolveRouteError::Unreachable)?;

        let (src_addr, dest_device) = match local_delivery {
            LocalDelivery::WeakLoopback { dst_ip, device } => {
                let src_ip = match src_ip {
                    Some(src_ip) => {
                        if !allow_non_local_src {
                            let _device = get_device_with_assigned_address(core_ctx, src_ip.into())
                                .ok_or(ResolveRouteError::NoSrcAddr)?;
                        }
                        src_ip
                    }
                    None => dst_ip,
                };
                (src_ip, device)
            }
            LocalDelivery::StrongForDevice(device) => {
                (get_local_addr(core_ctx, src_ip, &device, dst_ip, allow_non_local_src)?, device)
            }
        };
        return Ok(ResolvedRoute {
            src_addr,
            local_delivery_device: Some(dest_device),
            device: loopback,
            next_hop: NextHop::RemoteAsNeighbor,
        });
    }

    core_ctx
        .with_main_ip_routing_table(|core_ctx, table| {
            let mut matching_with_addr = table.lookup_filter_map(
                core_ctx,
                device,
                dst_ip.map_or(I::UNSPECIFIED_ADDRESS, |a| a.addr()),
                |core_ctx, d| {
                    Some(get_local_addr(core_ctx, src_ip, d, dst_ip, allow_non_local_src))
                },
            );

            let first_error = match matching_with_addr.next() {
                Some((Destination { device, next_hop }, Ok(local_addr))) => {
                    return Ok((Destination { device: device.clone(), next_hop }, local_addr))
                }
                Some((_, Err(e))) => e,
                None => return Err(ResolveRouteError::Unreachable),
            };

            matching_with_addr
                .filter_map(|(destination, local_addr)| {
                    // Select successful routes. We ignore later errors
                    // since we've already saved the first one.
                    local_addr
                        .ok_checked::<ResolveRouteError>()
                        .map(|local_addr| (destination, local_addr))
                })
                .next()
                .map_or(Err(first_error), |(Destination { device, next_hop }, local_addr)| {
                    Ok((Destination { device: device.clone(), next_hop }, local_addr))
                })
        })
        .map(|(Destination { device, next_hop }, local_addr)| ResolvedRoute {
            src_addr: local_addr,
            device,
            local_delivery_device: None,
            next_hop,
        })
}

/// Enables a blanket implementation of [`IpSocketContext`].
///
/// Implementing this marker trait for a type enables a blanket implementation
/// of `IpSocketContext` given the other requirements are met.
pub trait UseIpSocketContextBlanket {}

impl<
        I: Ip + IpDeviceStateIpExt + IpDeviceIpExt + IpLayerIpExt,
        BC: IpDeviceBindingsContext<I, CC::DeviceId>
            + IpLayerBindingsContext<I, CC::DeviceId>
            + IpSocketBindingsContext,
        CC: IpLayerContext<I, BC>
            + IpLayerEgressContext<I, BC>
            + device::IpDeviceConfigurationContext<I, BC>
            + UseIpSocketContextBlanket,
    > IpSocketContext<I, BC> for CC
{
    fn lookup_route(
        &mut self,
        _bindings_ctx: &mut BC,
        device: Option<&CC::DeviceId>,
        local_ip: Option<IpDeviceAddr<I::Addr>>,
        addr: RoutableIpAddr<I::Addr>,
        transparent: bool,
    ) -> Result<ResolvedRoute<I, CC::DeviceId>, ResolveRouteError> {
        resolve_route_to_destination(self, device, local_ip, Some(addr), transparent)
    }

    fn send_ip_packet<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<
            I,
            &<CC as DeviceIdContext<AnyDevice>>::DeviceId,
            SpecifiedAddr<I::Addr>,
        >,
        body: S,
        packet_metadata: IpLayerPacketMetadata<I, BC>,
    ) -> Result<(), IpSendFrameError<S>>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut,
    {
        send_ip_packet_from_device(self, bindings_ctx, meta.into(), body, packet_metadata)
    }

    fn get_loopback_device(&mut self) -> Option<Self::DeviceId> {
        device::IpDeviceConfigurationContext::<I, _>::loopback_id(self)
    }
}

/// The IP context providing dispatch to the available transport protocols.
///
/// This trait acts like a demux on the transport protocol for ingress IP
/// packets.
pub trait IpTransportDispatchContext<I: IpLayerIpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Dispatches a received incoming IP packet to the appropriate protocol.
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: I::RecvSrcAddr,
        dst_ip: SpecifiedAddr<I::Addr>,
        proto: I::Proto,
        body: B,
        meta: ReceiveIpPacketMeta<I>,
    ) -> Result<(), TransportReceiveError>;
}

/// A marker trait for all the contexts required for IP ingress.
pub trait IpLayerIngressContext<
    I: IpLayerIpExt + IcmpHandlerIpExt,
    BC: IpLayerBindingsContext<I, Self::DeviceId>,
>:
    IpTransportDispatchContext<I, BC, DeviceId: filter::InterfaceProperties<BC::DeviceClass>>
    + IpDeviceStateContext<I, BC>
    + IpDeviceSendContext<I, BC>
    + IcmpErrorHandler<I, BC>
    + IpLayerContext<I, BC>
    + FragmentHandler<I, BC>
    + FilterHandlerProvider<I, BC>
    + RawIpSocketHandler<I, BC>
{
}

impl<
        I: IpLayerIpExt + IcmpHandlerIpExt,
        BC: IpLayerBindingsContext<I, CC::DeviceId>,
        CC: IpTransportDispatchContext<
                I,
                BC,
                DeviceId: filter::InterfaceProperties<BC::DeviceClass>,
            > + IpDeviceStateContext<I, BC>
            + IpDeviceSendContext<I, BC>
            + IcmpErrorHandler<I, BC>
            + IpLayerContext<I, BC>
            + FragmentHandler<I, BC>
            + FilterHandlerProvider<I, BC>
            + RawIpSocketHandler<I, BC>,
    > IpLayerIngressContext<I, BC> for CC
{
}

/// A marker trait for all the contexts required for IP egress.
pub(crate) trait IpLayerEgressContext<I, BC>:
    IpDeviceSendContext<I, BC, DeviceId: filter::InterfaceProperties<BC::DeviceClass>>
    + FilterHandlerProvider<I, BC>
    + CounterContext<IpCounters<I>>
where
    I: IpLayerIpExt,
    BC: FilterBindingsContext,
{
}

impl<I, BC, CC> IpLayerEgressContext<I, BC> for CC
where
    I: IpLayerIpExt,
    BC: FilterBindingsContext,
    CC: IpDeviceSendContext<I, BC, DeviceId: filter::InterfaceProperties<BC::DeviceClass>>
        + FilterHandlerProvider<I, BC>
        + CounterContext<IpCounters<I>>,
{
}

/// A builder for IPv4 state.
#[derive(Copy, Clone, Default)]
pub struct Ipv4StateBuilder {
    icmp: Icmpv4StateBuilder,
}

impl Ipv4StateBuilder {
    /// Get the builder for the ICMPv4 state.
    #[cfg(any(test, feature = "testutils"))]
    pub fn icmpv4_builder(&mut self) -> &mut Icmpv4StateBuilder {
        &mut self.icmp
    }

    /// Builds the [`Ipv4State`].
    pub fn build<
        CC: CoreTimerContext<IpLayerTimerId, BC>,
        StrongDeviceId: StrongDeviceIdentifier,
        BC: TimerContext + RngContext + IpLayerBindingsTypes,
    >(
        self,
        bindings_ctx: &mut BC,
    ) -> Ipv4State<StrongDeviceId, BC> {
        let Ipv4StateBuilder { icmp } = self;

        Ipv4State {
            inner: IpStateInner::new::<CC>(bindings_ctx),
            icmp: icmp.build(),
            next_packet_id: Default::default(),
        }
    }
}

/// A builder for IPv6 state.
#[derive(Copy, Clone, Default)]
pub struct Ipv6StateBuilder {
    icmp: Icmpv6StateBuilder,
}

impl Ipv6StateBuilder {
    /// Builds the [`Ipv6State`].
    pub fn build<
        CC: CoreTimerContext<IpLayerTimerId, BC>,
        StrongDeviceId: StrongDeviceIdentifier,
        BC: TimerContext + RngContext + IpLayerBindingsTypes,
    >(
        self,
        bindings_ctx: &mut BC,
    ) -> Ipv6State<StrongDeviceId, BC> {
        let Ipv6StateBuilder { icmp } = self;

        Ipv6State {
            inner: IpStateInner::new::<CC>(bindings_ctx),
            icmp: icmp.build(),
            slaac_counters: Default::default(),
        }
    }
}

/// The stack's IPv4 state.
pub struct Ipv4State<StrongDeviceId: StrongDeviceIdentifier, BT: IpLayerBindingsTypes> {
    /// The common inner IP layer state.
    pub inner: IpStateInner<Ipv4, StrongDeviceId, BT>,
    /// The ICMP state.
    pub icmp: Icmpv4State<BT>,
    /// The atomic counter providing IPv4 packet identifiers.
    pub next_packet_id: AtomicU16,
}

impl<StrongDeviceId: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    AsRef<IpStateInner<Ipv4, StrongDeviceId, BT>> for Ipv4State<StrongDeviceId, BT>
{
    fn as_ref(&self) -> &IpStateInner<Ipv4, StrongDeviceId, BT> {
        &self.inner
    }
}

/// Generates an IP packet ID.
///
/// This is only meaningful for IPv4, see [`IpLayerIpExt`].
pub fn gen_ip_packet_id<I: IpLayerIpExt, BC, CC: IpDeviceStateContext<I, BC>>(
    core_ctx: &mut CC,
) -> I::PacketId {
    core_ctx.with_next_packet_id(|state| I::next_packet_id_from_state(state))
}

/// The stack's IPv6 state.
pub struct Ipv6State<StrongDeviceId: StrongDeviceIdentifier, BT: IpLayerBindingsTypes> {
    /// The common inner IP layer state.
    pub inner: IpStateInner<Ipv6, StrongDeviceId, BT>,
    /// ICMPv6 state.
    pub icmp: Icmpv6State<BT>,
    /// Stateless address autoconfiguration counters.
    pub slaac_counters: SlaacCounters,
}

impl<StrongDeviceId: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    AsRef<IpStateInner<Ipv6, StrongDeviceId, BT>> for Ipv6State<StrongDeviceId, BT>
{
    fn as_ref(&self) -> &IpStateInner<Ipv6, StrongDeviceId, BT> {
        &self.inner
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    OrderedLockAccess<IpPacketFragmentCache<I, BT>> for IpStateInner<I, D, BT>
{
    type Lock = Mutex<IpPacketFragmentCache<I, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.fragment_cache)
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    OrderedLockAccess<PmtuCache<I, BT>> for IpStateInner<I, D, BT>
{
    type Lock = Mutex<PmtuCache<I, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.pmtu_cache)
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    OrderedLockAccess<HashMap<RoutingTableId<I, D>, PrimaryRc<RwLock<RoutingTable<I, D>>>>>
    for IpStateInner<I, D, BT>
{
    type Lock = Mutex<HashMap<RoutingTableId<I, D>, PrimaryRc<RwLock<RoutingTable<I, D>>>>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.tables)
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier> OrderedLockAccess<RoutingTable<I, D>>
    for RoutingTableId<I, D>
{
    type Lock = RwLock<RoutingTable<I, D>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        let Self(inner) = self;
        OrderedLockRef::new(&*inner)
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    OrderedLockAccess<MulticastForwardingState<I, D>> for IpStateInner<I, D, BT>
{
    type Lock = RwLock<MulticastForwardingState<I, D>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.multicast_forwarding)
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    OrderedLockAccess<RawIpSocketMap<I, D::Weak, BT>> for IpStateInner<I, D, BT>
{
    type Lock = RwLock<RawIpSocketMap<I, D::Weak, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.raw_sockets)
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpLayerBindingsTypes>
    OrderedLockAccess<filter::State<I, BT>> for IpStateInner<I, D, BT>
{
    type Lock = RwLock<filter::State<I, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.filter)
    }
}

/// Ip layer counters.
#[derive(Default, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct IpCounters<I: IpLayerIpExt> {
    /// Count of incoming IP unicast packets delivered.
    pub deliver_unicast: Counter,
    /// Count of incoming IP multicast packets delivered.
    pub deliver_multicast: Counter,
    /// Count of incoming IP packets that are dispatched to the appropriate protocol.
    pub dispatch_receive_ip_packet: Counter,
    /// Count of incoming IP packets destined to another host.
    pub dispatch_receive_ip_packet_other_host: Counter,
    /// Count of incoming IP packets received by the stack.
    pub receive_ip_packet: Counter,
    /// Count of sent outgoing IP packets.
    pub send_ip_packet: Counter,
    /// Count of packets to be forwarded which are instead dropped because
    /// forwarding is disabled.
    pub forwarding_disabled: Counter,
    /// Count of incoming packets forwarded to another host.
    pub forward: Counter,
    /// Count of incoming packets which cannot be forwarded because there is no
    /// route to the destination host.
    pub no_route_to_host: Counter,
    /// Count of incoming packets which cannot be forwarded because the MTU has
    /// been exceeded.
    pub mtu_exceeded: Counter,
    /// Count of incoming packets which cannot be forwarded because the TTL has
    /// expired.
    pub ttl_expired: Counter,
    /// Count of ICMP error messages received.
    pub receive_icmp_error: Counter,
    /// Count of IP fragment reassembly errors.
    pub fragment_reassembly_error: Counter,
    /// Count of IP fragments that could not be reassembled because more
    /// fragments were needed.
    pub need_more_fragments: Counter,
    /// Count of IP fragments that could not be reassembled because the fragment
    /// was invalid.
    pub invalid_fragment: Counter,
    /// Count of IP fragments that could not be reassembled because the stack's
    /// per-IP-protocol fragment cache was full.
    pub fragment_cache_full: Counter,
    /// Count of incoming IP packets not delivered because of a parameter problem.
    pub parameter_problem: Counter,
    /// Count of incoming IP packets with an unspecified destination address.
    pub unspecified_destination: Counter,
    /// Count of incoming IP packets with an unspecified source address.
    pub unspecified_source: Counter,
    /// Count of incoming IP packets dropped.
    pub dropped: Counter,
    /// Number of frames rejected because they'd cause illegal loopback
    /// addresses on the wire.
    pub tx_illegal_loopback_address: Counter,
    /// Version specific rx counters.
    pub version_rx: I::RxCounters,
}

/// IPv4-specific Rx counters.
#[derive(Default)]
pub struct Ipv4RxCounters {
    /// Count of incoming broadcast IPv4 packets delivered.
    pub deliver_broadcast: Counter,
}

impl Inspectable for Ipv4RxCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { deliver_broadcast } = self;
        inspector.record_counter("DeliveredBroadcast", deliver_broadcast);
    }
}

/// IPv6-specific Rx counters.
#[derive(Default)]
pub struct Ipv6RxCounters {
    /// Count of incoming IPv6 packets dropped because the destination address
    /// is only tentatively assigned to the device.
    pub drop_for_tentative: Counter,
    /// Count of incoming IPv6 packets dropped due to a non-unicast source address.
    pub non_unicast_source: Counter,
    /// Count of incoming IPv6 packets discarded while processing extension
    /// headers.
    pub extension_header_discard: Counter,
    /// Count of incoming neighbor solicitations discarded as looped-back
    /// DAD probes.
    pub drop_looped_back_dad_probe: Counter,
}

impl Inspectable for Ipv6RxCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self {
            drop_for_tentative,
            non_unicast_source,
            extension_header_discard,
            drop_looped_back_dad_probe,
        } = self;
        inspector.record_counter("DroppedTentativeDst", drop_for_tentative);
        inspector.record_counter("DroppedNonUnicastSrc", non_unicast_source);
        inspector.record_counter("DroppedExtensionHeader", extension_header_discard);
        inspector.record_counter("DroppedLoopedBackDadProbe", drop_looped_back_dad_probe);
    }
}

/// Marker trait for the bindings types required by the IP layer's inner state.
pub trait IpStateBindingsTypes:
    PmtuBindingsTypes + FragmentBindingsTypes + RawIpSocketsBindingsTypes + FilterBindingsTypes
{
}
impl<BT> IpStateBindingsTypes for BT where
    BT: PmtuBindingsTypes + FragmentBindingsTypes + RawIpSocketsBindingsTypes + FilterBindingsTypes
{
}

/// Identifier to a routing table.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RoutingTableId<I: Ip, D>(StrongRc<RwLock<RoutingTable<I, D>>>);

impl<I: Ip, D> Debug for RoutingTableId<I, D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("RoutingTabeId").field(&StrongRc::debug_id(rc)).finish()
    }
}

impl<I: Ip, D> RoutingTableId<I, D> {
    /// Creates a new table ID.
    pub(crate) fn new(rc: StrongRc<RwLock<RoutingTable<I, D>>>) -> Self {
        Self(rc)
    }

    /// Provides direct access to the forwarding table.
    #[cfg(any(test, feature = "testutils"))]
    pub fn table(&self) -> &RwLock<RoutingTable<I, D>> {
        let Self(inner) = self;
        &*inner
    }

    /// Downgrades the strong ID into a weak one.
    pub fn downgrade(&self) -> WeakRoutingTableId<I, D> {
        let Self(rc) = self;
        WeakRoutingTableId(StrongRc::downgrade(rc))
    }
}

/// Weak Identifier to a routing table.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct WeakRoutingTableId<I: Ip, D>(WeakRc<RwLock<RoutingTable<I, D>>>);

impl<I: Ip, D> Debug for WeakRoutingTableId<I, D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("WeakRoutingTabeId").field(&WeakRc::debug_id(rc)).finish()
    }
}

/// The inner state for the IP layer for IP version `I`.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct IpStateInner<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpStateBindingsTypes> {
    // TODO(https://fxbug.dev/355059838): Explore the option to let Bindings create the main table.
    main_table_id: RoutingTableId<I, D>,
    multicast_forwarding: RwLock<MulticastForwardingState<I, D>>,
    fragment_cache: Mutex<IpPacketFragmentCache<I, BT>>,
    pmtu_cache: Mutex<PmtuCache<I, BT>>,
    counters: IpCounters<I>,
    raw_sockets: RwLock<RawIpSocketMap<I, D::Weak, BT>>,
    raw_socket_counters: RawIpSocketCounters<I>,
    filter: RwLock<filter::State<I, BT>>,
    // Make sure the primary IDs are dropped last. Also note that the following hash map also stores
    // the primary ID to the main table, and if the user (Bindings) attempts to remove the main
    // table without dropping `main_table_id` first, it will panic. This serves as an assertion
    // that the main table cannot be removed and Bindings must never attempt to remove the main
    // routing table.
    tables: Mutex<HashMap<RoutingTableId<I, D>, PrimaryRc<RwLock<RoutingTable<I, D>>>>>,
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpStateBindingsTypes> IpStateInner<I, D, BT> {
    /// Gets the IP counters.
    pub fn counters(&self) -> &IpCounters<I> {
        &self.counters
    }

    /// Gets the aggregate raw IP socket counters.
    pub fn raw_ip_socket_counters(&self) -> &RawIpSocketCounters<I> {
        &self.raw_socket_counters
    }

    /// Gets the main table ID.
    pub fn main_table_id(&self) -> &RoutingTableId<I, D> {
        &self.main_table_id
    }

    /// Provides direct access to the path MTU cache.
    #[cfg(any(test, feature = "testutils"))]
    pub fn pmtu_cache(&self) -> &Mutex<PmtuCache<I, BT>> {
        &self.pmtu_cache
    }
}

impl<
        I: IpLayerIpExt,
        D: StrongDeviceIdentifier,
        BC: TimerContext + RngContext + IpStateBindingsTypes,
    > IpStateInner<I, D, BC>
{
    /// Creates a new inner IP layer state.
    fn new<CC: CoreTimerContext<IpLayerTimerId, BC>>(bindings_ctx: &mut BC) -> Self {
        let main_table: PrimaryRc<RwLock<RoutingTable<I, D>>> = PrimaryRc::new(Default::default());
        let main_table_id = RoutingTableId(PrimaryRc::clone_strong(&main_table));
        Self {
            tables: Mutex::new(HashMap::from_iter(core::iter::once((
                main_table_id.clone(),
                main_table,
            )))),
            main_table_id,
            multicast_forwarding: Default::default(),
            fragment_cache: Mutex::new(
                IpPacketFragmentCache::new::<NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx),
            ),
            pmtu_cache: Mutex::new(PmtuCache::new::<NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx)),
            counters: Default::default(),
            raw_sockets: Default::default(),
            raw_socket_counters: Default::default(),
            filter: RwLock::new(filter::State::new::<NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx)),
        }
    }
}

/// The identifier for timer events in the IP layer.
#[derive(Debug, Clone, Eq, PartialEq, Hash, GenericOverIp)]
#[generic_over_ip()]
pub enum IpLayerTimerId {
    /// A timer event for IPv4 packet reassembly timers.
    ReassemblyTimeoutv4(FragmentTimerId<Ipv4>),
    /// A timer event for IPv6 packet reassembly timers.
    ReassemblyTimeoutv6(FragmentTimerId<Ipv6>),
    /// A timer event for IPv4 path MTU discovery.
    PmtuTimeoutv4(PmtuTimerId<Ipv4>),
    /// A timer event for IPv6 path MTU discovery.
    PmtuTimeoutv6(PmtuTimerId<Ipv6>),
    /// A timer event for IPv4 filtering timers.
    FilterTimerv4(FilterTimerId<Ipv4>),
    /// A timer event for IPv6 filtering timers.
    FilterTimerv6(FilterTimerId<Ipv6>),
}

impl<I: Ip> From<FragmentTimerId<I>> for IpLayerTimerId {
    fn from(timer: FragmentTimerId<I>) -> IpLayerTimerId {
        I::map_ip(timer, IpLayerTimerId::ReassemblyTimeoutv4, IpLayerTimerId::ReassemblyTimeoutv6)
    }
}

impl<I: Ip> From<PmtuTimerId<I>> for IpLayerTimerId {
    fn from(timer: PmtuTimerId<I>) -> IpLayerTimerId {
        I::map_ip(timer, IpLayerTimerId::PmtuTimeoutv4, IpLayerTimerId::PmtuTimeoutv6)
    }
}

impl<I: Ip> From<FilterTimerId<I>> for IpLayerTimerId {
    fn from(timer: FilterTimerId<I>) -> IpLayerTimerId {
        I::map_ip(timer, IpLayerTimerId::FilterTimerv4, IpLayerTimerId::FilterTimerv6)
    }
}

impl<CC, BC> HandleableTimer<CC, BC> for IpLayerTimerId
where
    CC: TimerHandler<BC, FragmentTimerId<Ipv4>>
        + TimerHandler<BC, FragmentTimerId<Ipv6>>
        + TimerHandler<BC, PmtuTimerId<Ipv4>>
        + TimerHandler<BC, PmtuTimerId<Ipv6>>
        + TimerHandler<BC, FilterTimerId<Ipv4>>
        + TimerHandler<BC, FilterTimerId<Ipv6>>,
    BC: TimerBindingsTypes,
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC, timer: BC::UniqueTimerId) {
        match self {
            IpLayerTimerId::ReassemblyTimeoutv4(id) => {
                core_ctx.handle_timer(bindings_ctx, id, timer)
            }
            IpLayerTimerId::ReassemblyTimeoutv6(id) => {
                core_ctx.handle_timer(bindings_ctx, id, timer)
            }
            IpLayerTimerId::PmtuTimeoutv4(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
            IpLayerTimerId::PmtuTimeoutv6(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
            IpLayerTimerId::FilterTimerv4(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
            IpLayerTimerId::FilterTimerv6(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
        }
    }
}

/// A [`TransportReceiveError`], and the metadata required to handle it.
struct DispatchIpPacketError<I: IcmpHandlerIpExt> {
    /// The error that occurred while dispatching to the transport layer.
    err: TransportReceiveError,
    /// The original source IP address of the packet (before the local-ingress
    /// hook evaluation).
    src_ip: I::SourceAddress,
    /// The original destination IP address of the packet (before the
    /// local-ingress hook evaluation).
    dst_ip: SpecifiedAddr<I::Addr>,
    /// The frame destination of the packet.
    frame_dst: Option<FrameDestination>,
    /// The metadata from the packet, allowing the packet's backing buffer to be
    /// returned to it's pre-IP-parse state with [`GrowBuffer::undo_parse`].
    meta: ParseMetadata,
}

impl<I: IcmpHandlerIpExt> DispatchIpPacketError<I> {
    /// Generate an send an appropriate ICMP error in response to this error.
    ///
    /// The provided `body` must be the original buffer from which the IP
    /// packet responsible for this error was parsed. It is expected to be in a
    /// state that allows undoing the IP packet parse (e.g. unmodified after the
    /// IP packet was parsed).
    fn respond_with_icmp_error<B: BufferMut, BC, CC: IcmpErrorHandler<I, BC>>(
        self,
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        mut body: B,
        device: &CC::DeviceId,
    ) {
        fn icmp_error_from_transport_error<I: IcmpHandlerIpExt>(
            err: TransportReceiveError,
            header_len: usize,
        ) -> I::IcmpError {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct ErrorHolder<I: Ip + IcmpHandlerIpExt>(I::IcmpError);

            let ErrorHolder(err) = match err {
                TransportReceiveError::ProtocolUnsupported => I::map_ip(
                    header_len,
                    |header_len| {
                        ErrorHolder(Icmpv4Error {
                            kind: Icmpv4ErrorKind::ProtocolUnreachable,
                            header_len,
                        })
                    },
                    |header_len| ErrorHolder(Icmpv6ErrorKind::ProtocolUnreachable { header_len }),
                ),
                TransportReceiveError::PortUnreachable => I::map_ip(
                    header_len,
                    |header_len| {
                        ErrorHolder(Icmpv4Error {
                            kind: Icmpv4ErrorKind::PortUnreachable,
                            header_len,
                        })
                    },
                    |_header_len| ErrorHolder(Icmpv6ErrorKind::PortUnreachable),
                ),
            };
            err
        }

        let DispatchIpPacketError { err, src_ip, dst_ip, frame_dst, meta } = self;
        // Undo the parsing of the IP Packet, moving the buffer's cursor so that
        // it points at the start of the IP header. This way, the sent ICMP
        // error will contain the entire original IP packet.
        body.undo_parse(meta);

        core_ctx.send_icmp_error_message(
            bindings_ctx,
            device,
            frame_dst,
            src_ip,
            dst_ip,
            body,
            icmp_error_from_transport_error::<I>(err, meta.header_len()),
        );
    }
}

// TODO(joshlf): Once we support multiple extension headers in IPv6, we will
// need to verify that the callers of this function are still sound. In
// particular, they may accidentally pass a parse_metadata argument which
// corresponds to a single extension header rather than all of the IPv6 headers.

/// Dispatch a received IPv4 packet to the appropriate protocol.
///
/// `device` is the device the packet was received on. `parse_metadata` is the
/// parse metadata associated with parsing the IP headers. It is used to undo
/// that parsing. Both `device` and `parse_metadata` are required in order to
/// send ICMP messages in response to unrecognized protocols or ports. If either
/// of `device` or `parse_metadata` is `None`, the caller promises that the
/// protocol and port are recognized.
///
/// # Panics
///
/// `dispatch_receive_ipv4_packet` panics if the protocol is unrecognized and
/// `parse_metadata` is `None`. If an IGMP message is received but it is not
/// coming from a device, i.e., `device` given is `None`,
/// `dispatch_receive_ip_packet` will also panic.
fn dispatch_receive_ipv4_packet<
    BC: IpLayerBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpLayerIngressContext<Ipv4, BC> + CounterContext<IpCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: Option<FrameDestination>,
    mut packet: Ipv4Packet<&mut [u8]>,
    mut packet_metadata: IpLayerPacketMetadata<Ipv4, BC>,
    meta: ReceiveIpPacketMeta<Ipv4>,
) -> Result<(), DispatchIpPacketError<Ipv4>> {
    core_ctx.increment(|counters| &counters.dispatch_receive_ip_packet);

    match frame_dst {
        Some(FrameDestination::Individual { local: false }) => {
            core_ctx.increment(|counters| &counters.dispatch_receive_ip_packet_other_host);
        }
        Some(FrameDestination::Individual { local: true })
        | Some(FrameDestination::Multicast)
        | Some(FrameDestination::Broadcast)
        | None => (),
    }

    let proto = packet.proto();

    match core_ctx.filter_handler().local_ingress_hook(
        bindings_ctx,
        &mut packet,
        device,
        &mut packet_metadata,
    ) {
        filter::Verdict::Drop => {
            packet_metadata.acknowledge_drop();
            return Ok(());
        }
        filter::Verdict::Accept => {}
    }
    packet_metadata.acknowledge_drop();

    let src_ip = packet.src_ip();
    // `dst_ip` is validated to be specified before a packet is provided to this
    // function, but it's possible for the LOCAL_INGRESS hook to rewrite the packet,
    // so we have to re-verify this.
    let Some(dst_ip) = SpecifiedAddr::new(packet.dst_ip()) else {
        core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_destination);
        debug!(
            "dispatch_receive_ipv4_packet: Received packet with unspecified destination IP address \
            after the LOCAL_INGRESS hook; dropping"
        );
        return Ok(());
    };

    core_ctx.deliver_packet_to_raw_ip_sockets(bindings_ctx, &packet, &device);

    let buffer = Buf::new(packet.body_mut(), ..);

    core_ctx
        .dispatch_receive_ip_packet(bindings_ctx, device, src_ip, dst_ip, proto, buffer, meta)
        .or_else(|err| {
            if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
                let (_, _, _, meta) = packet.into_metadata();
                Err(DispatchIpPacketError { err, src_ip, dst_ip, frame_dst, meta })
            } else {
                Ok(())
            }
        })
}

/// Dispatch a received IPv6 packet to the appropriate protocol.
///
/// `dispatch_receive_ipv6_packet` has the same semantics as
/// `dispatch_receive_ipv4_packet`, but for IPv6.
fn dispatch_receive_ipv6_packet<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpLayerIngressContext<Ipv6, BC> + CounterContext<IpCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: Option<FrameDestination>,
    mut packet: Ipv6Packet<&mut [u8]>,
    mut packet_metadata: IpLayerPacketMetadata<Ipv6, BC>,
    meta: ReceiveIpPacketMeta<Ipv6>,
) -> Result<(), DispatchIpPacketError<Ipv6>> {
    // TODO(https://fxbug.dev/42095067): Once we support multiple extension
    // headers in IPv6, we will need to verify that the callers of this
    // function are still sound. In particular, they may accidentally pass a
    // parse_metadata argument which corresponds to a single extension
    // header rather than all of the IPv6 headers.

    core_ctx.increment(|counters| &counters.dispatch_receive_ip_packet);

    match frame_dst {
        Some(FrameDestination::Individual { local: false }) => {
            core_ctx.increment(|counters| &counters.dispatch_receive_ip_packet_other_host);
        }
        Some(FrameDestination::Individual { local: true })
        | Some(FrameDestination::Multicast)
        | Some(FrameDestination::Broadcast)
        | None => (),
    }

    let proto = packet.proto();

    match core_ctx.filter_handler().local_ingress_hook(
        bindings_ctx,
        &mut packet,
        device,
        &mut packet_metadata,
    ) {
        filter::Verdict::Drop => {
            packet_metadata.acknowledge_drop();
            return Ok(());
        }
        filter::Verdict::Accept => {}
    }

    // These invariants are validated by the caller of this function, but it's
    // possible for the LOCAL_INGRESS hook to rewrite the packet, so we have to
    // check them again.
    let Some(src_ip) = packet.src_ipv6() else {
        debug!(
            "dispatch_receive_ipv6_packet: received packet from non-unicast source {} after the \
            LOCAL_INGRESS hook; dropping",
            packet.src_ip()
        );
        core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.version_rx.non_unicast_source);
        return Ok(());
    };
    let Some(dst_ip) = SpecifiedAddr::new(packet.dst_ip()) else {
        core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.unspecified_destination);
        debug!(
            "dispatch_receive_ipv6_packet: Received packet with unspecified destination IP address \
            after the LOCAL_INGRESS hook; dropping"
        );
        return Ok(());
    };

    core_ctx.deliver_packet_to_raw_ip_sockets(bindings_ctx, &packet, &device);

    let buffer = Buf::new(packet.body_mut(), ..);

    let result = core_ctx
        .dispatch_receive_ip_packet(bindings_ctx, device, src_ip, dst_ip, proto, buffer, meta)
        .or_else(|err| {
            if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                let (_, _, _, meta) = packet.into_metadata();
                Err(DispatchIpPacketError { err, src_ip: *src_ip, dst_ip, frame_dst, meta })
            } else {
                Ok(())
            }
        });
    packet_metadata.acknowledge_drop();
    result
}

pub(crate) fn send_ip_frame<I, CC, BC, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    destination: IpPacketDestination<I, &CC::DeviceId>,
    mut body: S,
    mut packet_metadata: IpLayerPacketMetadata<I, BC>,
) -> Result<(), IpSendFrameError<S>>
where
    I: IpLayerIpExt,
    BC: FilterBindingsContext,
    CC: IpLayerEgressContext<I, BC>,
    S: Serializer + IpPacket<I>,
    S::Buffer: BufferMut,
{
    let (verdict, proof) = core_ctx.filter_handler().egress_hook(
        bindings_ctx,
        &mut body,
        device,
        &mut packet_metadata,
    );
    match verdict {
        filter::Verdict::Drop => {
            packet_metadata.acknowledge_drop();
            return Ok(());
        }
        filter::Verdict::Accept => {}
    }
    packet_metadata.acknowledge_drop();

    // The filtering layer may have changed our address. Perform a last moment
    // check to protect against sending loopback addresses on the wire for
    // non-loopback devices, which is an RFC violation.
    if !device.is_loopback()
        && (I::LOOPBACK_SUBNET.contains(&body.src_addr())
            || I::LOOPBACK_SUBNET.contains(&body.dst_addr()))
    {
        core_ctx.increment(|c: &IpCounters<I>| &c.tx_illegal_loopback_address);
        return Err(IpSendFrameError {
            serializer: body,
            error: IpSendFrameErrorReason::IllegalLoopbackAddress,
        });
    }

    core_ctx.send_ip_frame(bindings_ctx, device, destination, body, proof).map_err(|e| e.err_into())
}

/// Drop a packet and undo the effects of parsing it.
///
/// `drop_packet_and_undo_parse!` takes a `$packet` and a `$buffer` which the
/// packet was parsed from. It saves the results of the `src_ip()`, `dst_ip()`,
/// `proto()`, and `parse_metadata()` methods. It drops `$packet` and uses the
/// result of `parse_metadata()` to undo the effects of parsing the packet.
/// Finally, it returns the source IP, destination IP, protocol, and parse
/// metadata.
macro_rules! drop_packet_and_undo_parse {
    ($packet:expr, $buffer:expr) => {{
        let (src_ip, dst_ip, proto, meta) = $packet.into_metadata();
        $buffer.undo_parse(meta);
        (src_ip, dst_ip, proto, meta)
    }};
}

/// Process a fragment and reassemble if required.
///
/// Attempts to process a potential fragment packet and reassemble if we are
/// ready to do so. If the packet isn't fragmented, or a packet was reassembled,
/// attempt to dispatch the packet.
fn process_fragment<I, CC, BC, F>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    mut dispatch: F,
    device: &CC::DeviceId,
    frame_dst: Option<FrameDestination>,
    packet: I::Packet<&mut [u8]>,
    mut packet_metadata: IpLayerPacketMetadata<I, BC>,
    receive_meta: ReceiveIpPacketMeta<I>,
) -> Result<(), DispatchIpPacketError<I>>
where
    I: IpLayerIpExt + IcmpHandlerIpExt,
    for<'a> I::Packet<&'a mut [u8]>: FragmentablePacket,
    CC: IpLayerIngressContext<I, BC> + CounterContext<IpCounters<I>>,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    F: FnMut(
        &mut CC,
        &mut BC,
        &CC::DeviceId,
        Option<FrameDestination>,
        I::Packet<&mut [u8]>,
        IpLayerPacketMetadata<I, BC>,
        ReceiveIpPacketMeta<I>,
    ) -> Result<(), DispatchIpPacketError<I>>,
{
    match FragmentHandler::<I, _>::process_fragment::<&mut [u8]>(core_ctx, bindings_ctx, packet) {
        // Handle the packet right away since reassembly is not needed.
        FragmentProcessingState::NotNeeded(packet) => {
            trace!("receive_ip_packet: not fragmented");
            // TODO(joshlf):
            // - Check for already-expired TTL?
            dispatch(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                packet,
                packet_metadata,
                receive_meta,
            )
        }
        // Ready to reassemble a packet.
        FragmentProcessingState::Ready { key, packet_len } => {
            trace!("receive_ip_packet: fragmented, ready for reassembly");
            // Allocate a buffer of `packet_len` bytes.
            let mut buffer = Buf::new(alloc::vec![0; packet_len], ..);
            // The packet metadata associated with this last fragment will
            // be dropped and a new metadata struct created for the
            // reassembled packet.
            packet_metadata.acknowledge_drop();

            // Attempt to reassemble the packet.
            let reassemble_result = match FragmentHandler::<I, _>::reassemble_packet(
                core_ctx,
                bindings_ctx,
                &key,
                buffer.buffer_view_mut(),
            ) {
                // Successfully reassembled the packet, handle it.
                Ok(packet) => {
                    trace!("receive_ip_packet: fragmented, reassembled packet: {:?}", packet);
                    // TODO(joshlf):
                    // - Check for already-expired TTL?
                    // Since each fragment had its own packet metadata, it's
                    // not clear what metadata to use for the reassembled
                    // packet. Resetting the metadata is the safest bet,
                    // though it means downstream consumers must be aware of
                    // this case.
                    let packet_metadata = IpLayerPacketMetadata::default();
                    dispatch(
                        core_ctx,
                        bindings_ctx,
                        device,
                        frame_dst,
                        packet,
                        packet_metadata,
                        receive_meta,
                    )
                }
                Err(e) => {
                    core_ctx
                        .increment(|counters: &IpCounters<I>| &counters.fragment_reassembly_error);
                    packet_metadata.acknowledge_drop();
                    debug!("receive_ip_packet: fragmented, failed to reassemble: {:?}", e);
                    Ok(())
                }
            };
            reassemble_result
        }
        // Cannot proceed since we need more fragments before we
        // can reassemble a packet.
        FragmentProcessingState::NeedMoreFragments => {
            packet_metadata.acknowledge_drop();
            core_ctx.increment(|counters: &IpCounters<I>| &counters.need_more_fragments);
            trace!("receive_ip_packet: fragmented, need more before reassembly");
            Ok(())
        }
        // TODO(ghanan): Handle invalid fragments.
        FragmentProcessingState::InvalidFragment => {
            packet_metadata.acknowledge_drop();
            core_ctx.increment(|counters: &IpCounters<I>| &counters.invalid_fragment);
            trace!("receive_ip_packet: fragmented, invalid");
            Ok(())
        }
        FragmentProcessingState::OutOfMemory => {
            packet_metadata.acknowledge_drop();
            core_ctx.increment(|counters: &IpCounters<I>| &counters.fragment_cache_full);
            trace!("receive_ip_packet: fragmented, dropped because OOM");
            Ok(())
        }
    }
}

// TODO(joshlf): Can we turn `try_parse_ip_packet` into a function? So far, I've
// been unable to get the borrow checker to accept it.

/// Try to parse an IP packet from a buffer.
///
/// If parsing fails, return the buffer to its original state so that its
/// contents can be used to send an ICMP error message. When invoked, the macro
/// expands to an expression whose type is `Result<P, P::Error>`, where `P` is
/// the parsed packet type.
macro_rules! try_parse_ip_packet {
    ($buffer:expr) => {{
        let p_len = $buffer.prefix_len();
        let s_len = $buffer.suffix_len();

        let result = $buffer.parse_mut();

        if let Err(err) = result {
            // Revert `buffer` to it's original state.
            let n_p_len = $buffer.prefix_len();
            let n_s_len = $buffer.suffix_len();

            if p_len > n_p_len {
                $buffer.grow_front(p_len - n_p_len);
            }

            if s_len > n_s_len {
                $buffer.grow_back(s_len - n_s_len);
            }

            Err(err)
        } else {
            result
        }
    }};
}

/// Receive an IPv4 packet from a device.
///
/// `frame_dst` specifies how this packet was received; see [`FrameDestination`]
/// for options.
pub fn receive_ipv4_packet<
    BC: IpLayerBindingsContext<Ipv4, CC::DeviceId>,
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv4, BC> + CounterContext<IpCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: Option<FrameDestination>,
    mut buffer: B,
) {
    if !core_ctx.is_ip_device_enabled(&device) {
        return;
    }

    core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.receive_ip_packet);
    trace!("receive_ip_packet({device:?})");

    let mut packet: Ipv4Packet<_> = match try_parse_ip_packet!(buffer) {
        Ok(packet) => packet,
        // Conditionally send an ICMP response if we encountered a parameter
        // problem error when parsing an IPv4 packet. Note, we do not always
        // send back an ICMP response as it can be used as an attack vector for
        // DDoS attacks. We only send back an ICMP response if the RFC requires
        // that we MUST send one, as noted by `must_send_icmp` and `action`.
        // TODO(https://fxbug.dev/42157630): test this code path once
        // `Ipv4Packet::parse` can return an `IpParseError::ParameterProblem`
        // error.
        Err(IpParseError::ParameterProblem {
            src_ip,
            dst_ip,
            code,
            pointer,
            must_send_icmp,
            header_len,
            action,
        }) if must_send_icmp && action.should_send_icmp(&dst_ip) => {
            core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.parameter_problem);
            // `should_send_icmp_to_multicast` should never return `true` for IPv4.
            assert!(!action.should_send_icmp_to_multicast());
            let dst_ip = match SpecifiedAddr::new(dst_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx
                        .increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_destination);
                    debug!("receive_ipv4_packet: Received packet with unspecified destination IP address; dropping");
                    return;
                }
            };
            let src_ip = match SpecifiedAddr::new(src_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_source);
                    trace!("receive_ipv4_packet: Cannot send ICMP error in response to packet with unspecified source IP address");
                    return;
                }
            };
            IcmpErrorHandler::<Ipv4, _>::send_icmp_error_message(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                buffer,
                Icmpv4Error {
                    kind: Icmpv4ErrorKind::ParameterProblem {
                        code,
                        pointer,
                        // When the call to `action.should_send_icmp` returns true, it always means that
                        // the IPv4 packet that failed parsing is an initial fragment.
                        fragment_type: Ipv4FragmentType::InitialFragment,
                    },
                    header_len,
                },
            );
            return;
        }
        _ => return, // TODO(joshlf): Do something with ICMP here?
    };

    // We verify this later by actually creating the `SpecifiedAddr` witness
    // type after the INGRESS filtering hook, but we keep this check here as an
    // optimization to return early if the packet has an unspecified
    // destination.
    if !packet.dst_ip().is_specified() {
        core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_destination);
        debug!("receive_ipv4_packet: Received packet with unspecified destination IP; dropping");
        return;
    };

    // TODO(ghanan): Act upon options.

    let mut packet_metadata = IpLayerPacketMetadata::default();
    let mut filter = core_ctx.filter_handler();
    match filter.ingress_hook(bindings_ctx, &mut packet, device, &mut packet_metadata) {
        IngressVerdict::Verdict(filter::Verdict::Accept) => {}
        IngressVerdict::Verdict(filter::Verdict::Drop) => {
            packet_metadata.acknowledge_drop();
            return;
        }
        IngressVerdict::TransparentLocalDelivery { addr, port } => {
            // Drop the filter handler since it holds a mutable borrow of `core_ctx`, which
            // we need to provide to the packet dispatch function.
            drop(filter);

            let Some(addr) = SpecifiedAddr::new(addr) else {
                core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_destination);
                debug!("cannot perform transparent delivery to unspecified destination; dropping");
                return;
            };

            // It's possible that the packet was actually sent to a broadcast
            // address, but it doesn't matter here since it's being delivered
            // to a transparent proxy.
            let meta = ReceiveIpPacketMeta {
                broadcast: None,
                transport_override: Some(TransparentLocalDelivery { addr, port }),
            };

            // Short-circuit the routing process and override local demux, providing a local
            // address and port to which the packet should be transparently delivered at the
            // transport layer.
            dispatch_receive_ipv4_packet(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                packet,
                packet_metadata,
                meta,
            )
            .unwrap_or_else(|err| {
                err.respond_with_icmp_error(core_ctx, bindings_ctx, buffer, device)
            });
            return;
        }
    }
    // Drop the filter handler since it holds a mutable borrow of `core_ctx`, which
    // we need below.
    drop(filter);

    let action = receive_ipv4_packet_action(core_ctx, device, &packet);
    match action {
        // TODO(https://fxbug.dev/353329136): Actually forward the packet to
        // the targets. For now, at least deliver it locally.
        ReceivePacketAction::MulticastForward { targets: _, address_status: None } => {}
        ReceivePacketAction::MulticastForward {
            targets: _,
            address_status: Some(address_status),
        }
        | ReceivePacketAction::Deliver { address_status } => {
            trace!("receive_ipv4_packet: delivering locally");
            let meta = ReceiveIpPacketMeta {
                broadcast: address_status.to_broadcast_marker(),
                transport_override: None,
            };
            // Process a potential IPv4 fragment if the destination is this
            // host.
            //
            // We process IPv4 packet reassembly here because, for IPv4, the
            // fragment data is in the header itself so we can handle it right
            // away.
            //
            // Note, the `process_fragment` function could panic if the packet does not
            // have fragment data. However, we are guaranteed that it will not
            // panic because the fragment data is in the fixed header so it is
            // always present (even if the fragment data has values that implies
            // that the packet is not fragmented).
            process_fragment(
                core_ctx,
                bindings_ctx,
                dispatch_receive_ipv4_packet,
                device,
                frame_dst,
                packet,
                packet_metadata,
                meta,
            )
            .unwrap_or_else(|err| {
                err.respond_with_icmp_error(core_ctx, bindings_ctx, buffer, device)
            })
        }
        ReceivePacketAction::Forward {
            original_dst: dst_ip,
            dst: Destination { device: dst_device, next_hop },
        } => {
            let destination = IpPacketDestination::from_next_hop(next_hop, dst_ip);
            let ttl = packet.ttl();
            if ttl > 1 {
                trace!("receive_ipv4_packet: forwarding");

                match core_ctx.filter_handler().forwarding_hook(
                    &mut packet,
                    device,
                    &dst_device,
                    &mut packet_metadata,
                ) {
                    filter::Verdict::Drop => {
                        packet_metadata.acknowledge_drop();
                        return;
                    }
                    filter::Verdict::Accept => {}
                }

                packet.set_ttl(ttl - 1);
                let (src, dst, proto, meta) = packet.into_metadata();
                let packet = ForwardedPacket::new(src, dst, proto, meta, buffer);

                match send_ip_frame(
                    core_ctx,
                    bindings_ctx,
                    &dst_device,
                    destination,
                    packet,
                    packet_metadata,
                ) {
                    Ok(()) => (),
                    Err(IpSendFrameError { serializer: _, error }) => {
                        match error {
                            IpSendFrameErrorReason::Device(
                                SendFrameErrorReason::SizeConstraintsViolation,
                            ) => {
                                core_ctx.increment(|counters: &IpCounters<Ipv4>| {
                                    &counters.mtu_exceeded
                                });
                            }
                            IpSendFrameErrorReason::Device(SendFrameErrorReason::QueueFull)
                            | IpSendFrameErrorReason::Device(SendFrameErrorReason::Alloc)
                            | IpSendFrameErrorReason::IllegalLoopbackAddress => (),
                        }
                        debug!("failed to forward IPv4 packet: {error:?}");
                    }
                }
            } else {
                // TTL is 0 or would become 0 after decrement; see "TTL"
                // section, https://tools.ietf.org/html/rfc791#page-14
                use packet_formats::ipv4::Ipv4Header as _;
                core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.ttl_expired);
                debug!("received IPv4 packet dropped due to expired TTL");
                let fragment_type = packet.fragment_type();
                let (src_ip, _, proto, meta): (_, Ipv4Addr, _, _) =
                    drop_packet_and_undo_parse!(packet, buffer);
                packet_metadata.acknowledge_drop();
                let src_ip = match SpecifiedAddr::new(src_ip) {
                    Some(ip) => ip,
                    None => {
                        core_ctx
                            .increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_source);
                        trace!("receive_ipv4_packet: Cannot send ICMP error in response to packet with unspecified source IP address");
                        return;
                    }
                };
                IcmpErrorHandler::<Ipv4, _>::send_icmp_error_message(
                    core_ctx,
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    buffer,
                    Icmpv4Error {
                        kind: Icmpv4ErrorKind::TtlExpired { proto, fragment_type },
                        header_len: meta.header_len(),
                    },
                );
            }
        }
        ReceivePacketAction::SendNoRouteToDest { dst: dst_ip } => {
            use packet_formats::ipv4::Ipv4Header as _;
            core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.no_route_to_host);
            debug!("received IPv4 packet with no known route to destination {}", dst_ip);
            let fragment_type = packet.fragment_type();
            let (src_ip, _, proto, meta): (_, Ipv4Addr, _, _) =
                drop_packet_and_undo_parse!(packet, buffer);
            packet_metadata.acknowledge_drop();
            let src_ip = match SpecifiedAddr::new(src_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_source);
                    trace!("receive_ipv4_packet: Cannot send ICMP error in response to packet with unspecified source IP address");
                    return;
                }
            };
            IcmpErrorHandler::<Ipv4, _>::send_icmp_error_message(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                buffer,
                Icmpv4Error {
                    kind: Icmpv4ErrorKind::NetUnreachable { proto, fragment_type },
                    header_len: meta.header_len(),
                },
            );
        }
        ReceivePacketAction::Drop { reason } => {
            let src_ip = packet.src_ip();
            let dst_ip = packet.dst_ip();
            packet_metadata.acknowledge_drop();
            core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.dropped);
            debug!(
                "receive_ipv4_packet: dropping packet from {src_ip} to {dst_ip} received on \
                {device:?}: {reason:?}",
            );
        }
    }
}

/// Receive an IPv6 packet from a device.
///
/// `frame_dst` specifies how this packet was received; see [`FrameDestination`]
/// for options.
pub fn receive_ipv6_packet<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv6, BC> + CounterContext<IpCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: Option<FrameDestination>,
    mut buffer: B,
) {
    if !core_ctx.is_ip_device_enabled(&device) {
        return;
    }

    core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.receive_ip_packet);
    trace!("receive_ipv6_packet({:?})", device);

    let mut packet: Ipv6Packet<_> = match try_parse_ip_packet!(buffer) {
        Ok(packet) => packet,
        // Conditionally send an ICMP response if we encountered a parameter
        // problem error when parsing an IPv4 packet. Note, we do not always
        // send back an ICMP response as it can be used as an attack vector for
        // DDoS attacks. We only send back an ICMP response if the RFC requires
        // that we MUST send one, as noted by `must_send_icmp` and `action`.
        Err(IpParseError::ParameterProblem {
            src_ip,
            dst_ip,
            code,
            pointer,
            must_send_icmp,
            header_len: _,
            action,
        }) if must_send_icmp && action.should_send_icmp(&dst_ip) => {
            core_ctx.increment(|counters| &counters.parameter_problem);
            let dst_ip = match SpecifiedAddr::new(dst_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.increment(|counters| &counters.unspecified_destination);
                    debug!("receive_ipv6_packet: Received packet with unspecified destination IP address; dropping");
                    return;
                }
            };
            let src_ip = match UnicastAddr::new(src_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.increment(|counters| &counters.version_rx.non_unicast_source);
                    trace!("receive_ipv6_packet: Cannot send ICMP error in response to packet with non unicast source IP address");
                    return;
                }
            };
            IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                buffer,
                Icmpv6ErrorKind::ParameterProblem {
                    code,
                    pointer,
                    allow_dst_multicast: action.should_send_icmp_to_multicast(),
                },
            );
            return;
        }
        _ => return, // TODO(joshlf): Do something with ICMP here?
    };

    trace!("receive_ipv6_packet: parsed packet: {:?}", packet);

    // TODO(ghanan): Act upon extension headers.

    // We verify these properties later by actually creating the corresponding
    // witness types after the INGRESS filtering hook, but we keep these checks
    // here as an optimization to return early and save some work.
    if packet.src_ipv6().is_none() {
        debug!(
            "receive_ipv6_packet: received packet from non-unicast source {}; dropping",
            packet.src_ip()
        );
        core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.version_rx.non_unicast_source);
        return;
    };
    if !packet.dst_ip().is_specified() {
        core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.unspecified_destination);
        debug!("receive_ipv6_packet: Received packet with unspecified destination IP; dropping");
        return;
    };

    let mut packet_metadata = IpLayerPacketMetadata::default();
    let mut filter = core_ctx.filter_handler();
    match filter.ingress_hook(bindings_ctx, &mut packet, device, &mut packet_metadata) {
        IngressVerdict::Verdict(filter::Verdict::Accept) => {}
        IngressVerdict::Verdict(filter::Verdict::Drop) => {
            packet_metadata.acknowledge_drop();
            return;
        }
        IngressVerdict::TransparentLocalDelivery { addr, port } => {
            // Drop the filter handler since it holds a mutable borrow of `core_ctx`, which
            // we need to provide to the packet dispatch function.
            drop(filter);

            let Some(addr) = SpecifiedAddr::new(addr) else {
                core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.unspecified_destination);
                debug!("cannot perform transparent delivery to unspecified destination; dropping");
                return;
            };

            let meta = ReceiveIpPacketMeta {
                broadcast: None,
                transport_override: Some(TransparentLocalDelivery { addr, port }),
            };

            // Short-circuit the routing process and override local demux, providing a local
            // address and port to which the packet should be transparently delivered at the
            // transport layer.
            dispatch_receive_ipv6_packet(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                packet,
                packet_metadata,
                meta,
            )
            .unwrap_or_else(|err| {
                err.respond_with_icmp_error(core_ctx, bindings_ctx, buffer, device)
            });
            return;
        }
    }
    // Drop the filter handler since it holds a mutable borrow of `core_ctx`, which
    // we need below.
    drop(filter);

    let Some(src_ip) = packet.src_ipv6() else {
        debug!(
            "receive_ipv6_packet: received packet from non-unicast source {}; dropping",
            packet.src_ip()
        );
        core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.version_rx.non_unicast_source);
        return;
    };

    match receive_ipv6_packet_action(core_ctx, device, &packet) {
        // TODO(https://fxbug.dev/353329136): Actually forward the packet to
        // the targets. For now, at least deliver it locally.
        ReceivePacketAction::MulticastForward { targets: _, address_status: None } => {}
        ReceivePacketAction::MulticastForward { targets: _, address_status: Some(_) }
        | ReceivePacketAction::Deliver { address_status: _ } => {
            trace!("receive_ipv6_packet: delivering locally");

            // Process a potential IPv6 fragment if the destination is this
            // host.
            //
            // We need to process extension headers in the order they appear in
            // the header. With some extension headers, we do not proceed to the
            // next header, and do some action immediately. For example, say we
            // have an IPv6 packet with two extension headers (routing extension
            // header before a fragment extension header). Until we get to the
            // final destination node in the routing header, we would need to
            // reroute the packet to the next destination without reassembling.
            // Once the packet gets to the last destination in the routing
            // header, that node will process the fragment extension header and
            // handle reassembly.
            match ipv6::handle_extension_headers(core_ctx, device, frame_dst, &packet, true) {
                Ipv6PacketAction::_Discard => {
                    core_ctx.increment(|counters: &IpCounters<Ipv6>| {
                        &counters.version_rx.extension_header_discard
                    });
                    trace!(
                        "receive_ipv6_packet: handled IPv6 extension headers: discarding packet"
                    );
                    packet_metadata.acknowledge_drop();
                }
                Ipv6PacketAction::Continue => {
                    trace!(
                        "receive_ipv6_packet: handled IPv6 extension headers: dispatching packet"
                    );

                    // TODO(joshlf):
                    // - Do something with ICMP if we don't have a handler for
                    //   that protocol?
                    // - Check for already-expired TTL?
                    dispatch_receive_ipv6_packet(
                        core_ctx,
                        bindings_ctx,
                        device,
                        frame_dst,
                        packet,
                        packet_metadata,
                        ReceiveIpPacketMeta::default(),
                    )
                    .unwrap_or_else(|err| {
                        err.respond_with_icmp_error(core_ctx, bindings_ctx, buffer, device)
                    });
                }
                Ipv6PacketAction::ProcessFragment => {
                    trace!(
                        "receive_ipv6_packet: handled IPv6 extension headers: handling \
                            fragmented packet"
                    );

                    let meta = ReceiveIpPacketMeta::default();

                    // Note, `IpPacketFragmentCache::process_fragment`
                    // could panic if the packet does not have fragment data.
                    // However, we are guaranteed that it will not panic for an
                    // IPv6 packet because the fragment data is in an (optional)
                    // fragment extension header which we attempt to handle by
                    // calling `ipv6::handle_extension_headers`. We will only
                    // end up here if its return value is
                    // `Ipv6PacketAction::ProcessFragment` which is only
                    // possible when the packet has the fragment extension
                    // header (even if the fragment data has values that implies
                    // that the packet is not fragmented).
                    //
                    // TODO(ghanan): Handle extension headers again since there
                    //               could be some more in a reassembled packet
                    //               (after the fragment header).
                    process_fragment(
                        core_ctx,
                        bindings_ctx,
                        dispatch_receive_ipv6_packet,
                        device,
                        frame_dst,
                        packet,
                        packet_metadata,
                        meta,
                    )
                    .unwrap_or_else(|err| {
                        err.respond_with_icmp_error(core_ctx, bindings_ctx, buffer, device)
                    })
                }
            }
        }
        ReceivePacketAction::Forward {
            original_dst: dst_ip,
            dst: Destination { device: dst_device, next_hop },
        } => {
            let ttl = packet.ttl();
            if ttl > 1 {
                trace!("receive_ipv6_packet: forwarding");

                // Handle extension headers first.
                match ipv6::handle_extension_headers(core_ctx, device, frame_dst, &packet, false) {
                    Ipv6PacketAction::_Discard => {
                        core_ctx.increment(|counters: &IpCounters<Ipv6>| {
                            &counters.version_rx.extension_header_discard
                        });
                        trace!("receive_ipv6_packet: handled IPv6 extension headers: discarding packet");
                        return;
                    }
                    Ipv6PacketAction::Continue => {
                        trace!("receive_ipv6_packet: handled IPv6 extension headers: forwarding packet");
                    }
                    Ipv6PacketAction::ProcessFragment => {
                        unreachable!(
                            "When forwarding packets, we should only ever look at the hop by hop \
                            options extension header (if present)"
                        )
                    }
                }

                match core_ctx.filter_handler().forwarding_hook(
                    &mut packet,
                    device,
                    &dst_device,
                    &mut packet_metadata,
                ) {
                    filter::Verdict::Drop => {
                        packet_metadata.acknowledge_drop();
                        return;
                    }
                    filter::Verdict::Accept => {}
                }

                let destination = IpPacketDestination::from_next_hop(next_hop, dst_ip);
                packet.set_ttl(ttl - 1);
                let (src, dst, proto, meta) = packet.into_metadata();
                let packet = ForwardedPacket::new(src, dst, proto, meta, buffer);

                match send_ip_frame(
                    core_ctx,
                    bindings_ctx,
                    &dst_device,
                    destination,
                    packet,
                    packet_metadata,
                ) {
                    Ok(()) => (),
                    Err(IpSendFrameError {
                        serializer,
                        error:
                            IpSendFrameErrorReason::Device(
                                SendFrameErrorReason::SizeConstraintsViolation,
                            ),
                    }) => {
                        debug!("failed to forward IPv6 packet: MTU exceeded");
                        core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.mtu_exceeded);

                        if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                            trace!("receive_ipv6_packet: Sending ICMPv6 Packet Too Big");
                            // TODO(joshlf): Increment the TTL since we just
                            // decremented it. The fact that we don't do this is
                            // technically a violation of the ICMP spec (we're not
                            // encapsulating the original packet that caused the
                            // issue, but a slightly modified version of it), but
                            // it's not that big of a deal because it won't affect
                            // the sender's ability to figure out the minimum path
                            // MTU. This may break other logic, though, so we should
                            // still fix it eventually.
                            let mtu = core_ctx.get_mtu(&device);
                            IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                                core_ctx,
                                bindings_ctx,
                                device,
                                frame_dst,
                                *src_ip,
                                dst_ip,
                                serializer.into_buffer(),
                                Icmpv6ErrorKind::PacketTooBig {
                                    proto,
                                    header_len: meta.header_len(),
                                    mtu,
                                },
                            );
                        }
                    }
                    Err(IpSendFrameError { serializer: _, error }) => {
                        debug!("failed to forward IPv6 packet: {error:?}");
                    }
                }
            } else {
                // Hop Limit is 0 or would become 0 after decrement; see RFC
                // 2460 Section 3.
                core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.ttl_expired);
                debug!("received IPv6 packet dropped due to expired Hop Limit");
                packet_metadata.acknowledge_drop();

                if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                    let (_, _, proto, meta): (Ipv6Addr, Ipv6Addr, _, _) =
                        drop_packet_and_undo_parse!(packet, buffer);
                    IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                        core_ctx,
                        bindings_ctx,
                        device,
                        frame_dst,
                        *src_ip,
                        dst_ip,
                        buffer,
                        Icmpv6ErrorKind::TtlExpired { proto, header_len: meta.header_len() },
                    );
                }
            }
        }
        ReceivePacketAction::SendNoRouteToDest { dst: dst_ip } => {
            core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.no_route_to_host);
            let (_, _, proto, meta): (Ipv6Addr, Ipv6Addr, _, _) =
                drop_packet_and_undo_parse!(packet, buffer);
            debug!("received IPv6 packet with no known route to destination {}", dst_ip);
            packet_metadata.acknowledge_drop();

            if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                    core_ctx,
                    bindings_ctx,
                    device,
                    frame_dst,
                    *src_ip,
                    dst_ip,
                    buffer,
                    Icmpv6ErrorKind::NetUnreachable { proto, header_len: meta.header_len() },
                );
            }
        }
        ReceivePacketAction::Drop { reason } => {
            core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.dropped);
            let src_ip = packet.src_ip();
            let dst_ip = packet.dst_ip();
            packet_metadata.acknowledge_drop();
            debug!(
                "receive_ipv6_packet: dropping packet from {src_ip} to {dst_ip} received on \
                {device:?}: {reason:?}",
            );
        }
    }
}

/// The action to take in order to process a received IP packet.
#[derive(Debug, PartialEq)]
pub enum ReceivePacketAction<I: BroadcastIpExt + IpLayerIpExt, DeviceId: StrongDeviceIdentifier> {
    /// Deliver the packet locally.
    Deliver {
        /// Status of the receiving IP address.
        address_status: I::AddressStatus,
    },

    /// Forward the packet to the given destination.
    Forward {
        /// The original destination IP address of the packet.
        original_dst: SpecifiedAddr<I::Addr>,
        /// The destination that the packet should be forwarded to.
        dst: Destination<I::Addr, DeviceId>,
    },

    /// A multicast packet that should be forwarded (& optional local delivery).
    ///
    /// The packet should be forwarded to each of the given targets. This case
    /// is only returned when the packet is eligible for multicast forwarding;
    /// `Self::Deliver` is used for packets that are ineligible (either because
    /// multicast forwarding is disabled, or because there are no applicable
    /// multicast routes with which to forward the packet).
    MulticastForward {
        /// The multicast targets to forward the packet via.
        targets: MulticastRouteTargets<DeviceId>,
        /// Some if the host is a member of the multicast group and the packet
        /// should be delivered locally (in addition to forwarding).
        address_status: Option<I::AddressStatus>,
    },

    /// Send a Destination Unreachable ICMP error message to the packet's sender
    /// and drop the packet.
    ///
    /// For ICMPv4, use the code "net unreachable". For ICMPv6, use the code "no
    /// route to destination".
    SendNoRouteToDest {
        /// The destination IP Address to which there was no route.
        dst: SpecifiedAddr<I::Addr>,
    },

    /// Silently drop the packet.
    ///
    /// `reason` describes why the packet was dropped.
    #[allow(missing_docs)]
    Drop { reason: DropReason },
}

/// The reason a received IP packet is dropped.
#[derive(Debug, PartialEq)]
pub enum DropReason {
    /// Remote packet destined to tentative address.
    Tentative,
    /// Remote packet destined to the unspecified address.
    UnspecifiedDestination,
    /// Packet should be forwarded but packet's inbound interface has forwarding
    /// disabled.
    ForwardingDisabledInboundIface,
    /// Remote packet destined to a multicast address that could not be:
    /// * delivered locally (because we are not a member of the multicast
    ///   group), or
    /// * forwarded (either because multicast forwarding is disabled, or no
    ///   applicable multicast route has been installed).
    MulticastNoInterest,
}

/// Computes the action to take in order to process a received IPv4 packet.
pub fn receive_ipv4_packet_action<
    BC: IpLayerBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpLayerContext<Ipv4, BC> + CounterContext<IpCounters<Ipv4>>,
    B: ByteSlice,
>(
    core_ctx: &mut CC,
    device: &CC::DeviceId,
    packet: &Ipv4Packet<B>,
) -> ReceivePacketAction<Ipv4, CC::DeviceId> {
    let Some(dst_ip) = SpecifiedAddr::new(packet.dst_ip()) else {
        core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_destination);
        return ReceivePacketAction::Drop { reason: DropReason::UnspecifiedDestination };
    };

    // If the packet arrived at the loopback interface, check if any local
    // interface has the destination address assigned. This effectively lets
    // the loopback interface operate as a weak host for incoming packets.
    //
    // Note that (as of writing) the stack sends all locally destined traffic to
    // the loopback interface so we need this hack to allow the stack to accept
    // packets that arrive at the loopback interface (after being looped back)
    // but destined to an address that is assigned to another local interface.
    //
    // TODO(https://fxbug.dev/42175703): This should instead be controlled by the
    // routing table.

    // Since we treat all addresses identically, it doesn't matter whether one
    // or more than one device has the address assigned. That means we can just
    // take the first status and ignore the rest.
    let first_status = if device.is_loopback() {
        core_ctx.with_address_statuses(dst_ip, |it| it.map(|(_device, status)| status).next())
    } else {
        core_ctx.address_status_for_device(dst_ip, device).into_present()
    };
    match first_status {
        Some(
            address_status @ (Ipv4PresentAddressStatus::Unicast
            | Ipv4PresentAddressStatus::LoopbackSubnet),
        ) => {
            core_ctx.increment(|counters| &counters.deliver_unicast);
            ReceivePacketAction::Deliver { address_status }
        }
        Some(address_status @ Ipv4PresentAddressStatus::Multicast) => {
            receive_ip_multicast_packet_action(core_ctx, device, packet, Some(address_status))
        }
        Some(
            address_status @ (Ipv4PresentAddressStatus::LimitedBroadcast
            | Ipv4PresentAddressStatus::SubnetBroadcast),
        ) => {
            core_ctx.increment(|counters| &counters.version_rx.deliver_broadcast);
            ReceivePacketAction::Deliver { address_status }
        }
        None => receive_ip_packet_action_common::<Ipv4, _, _, _>(core_ctx, dst_ip, device, packet),
    }
}

/// Computes the action to take in order to process a received IPv6 packet.
pub fn receive_ipv6_packet_action<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpLayerContext<Ipv6, BC> + CounterContext<IpCounters<Ipv6>>,
    B: ByteSlice,
>(
    core_ctx: &mut CC,
    device: &CC::DeviceId,
    packet: &Ipv6Packet<B>,
) -> ReceivePacketAction<Ipv6, CC::DeviceId> {
    let Some(dst_ip) = SpecifiedAddr::new(packet.dst_ip()) else {
        core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.unspecified_destination);
        return ReceivePacketAction::Drop { reason: DropReason::UnspecifiedDestination };
    };

    // If the packet arrived at the loopback interface, check if any local
    // interface has the destination address assigned. This effectively lets
    // the loopback interface operate as a weak host for incoming packets.
    //
    // Note that (as of writing) the stack sends all locally destined traffic to
    // the loopback interface so we need this hack to allow the stack to accept
    // packets that arrive at the loopback interface (after being looped back)
    // but destined to an address that is assigned to another local interface.
    //
    // TODO(https://fxbug.dev/42175703): This should instead be controlled by the
    // routing table.

    // It's possible that there is more than one device with the address
    // assigned. Since IPv6 addresses are either multicast or unicast, we
    // don't expect to see one device with `UnicastAssigned` or
    // `UnicastTentative` and another with `Multicast`. We might see one
    // assigned and one tentative status, though, in which case we should
    // prefer the former.
    fn choose_highest_priority(
        address_statuses: impl Iterator<Item = Ipv6PresentAddressStatus>,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
    ) -> Option<Ipv6PresentAddressStatus> {
        address_statuses.max_by(|lhs, rhs| {
            use Ipv6PresentAddressStatus::*;
            match (lhs, rhs) {
                (UnicastAssigned | UnicastTentative, Multicast)
                | (Multicast, UnicastAssigned | UnicastTentative) => {
                    unreachable!("the IPv6 address {:?} is not both unicast and multicast", dst_ip)
                }
                (UnicastAssigned, UnicastTentative) => Ordering::Greater,
                (UnicastTentative, UnicastAssigned) => Ordering::Less,
                (UnicastTentative, UnicastTentative)
                | (UnicastAssigned, UnicastAssigned)
                | (Multicast, Multicast) => Ordering::Equal,
            }
        })
    }

    let highest_priority = if device.is_loopback() {
        core_ctx.with_address_statuses(dst_ip, |it| {
            let it = it.map(|(_device, status)| status);
            choose_highest_priority(it, dst_ip)
        })
    } else {
        core_ctx.address_status_for_device(dst_ip, device).into_present()
    };
    match highest_priority {
        Some(address_status @ Ipv6PresentAddressStatus::Multicast) => {
            receive_ip_multicast_packet_action(core_ctx, device, packet, Some(address_status))
        }
        Some(address_status @ Ipv6PresentAddressStatus::UnicastAssigned) => {
            core_ctx.increment(|counters| &counters.deliver_unicast);
            ReceivePacketAction::Deliver { address_status }
        }
        Some(Ipv6PresentAddressStatus::UnicastTentative) => {
            // If the destination address is tentative (which implies that
            // we are still performing NDP's Duplicate Address Detection on
            // it), then we don't consider the address "assigned to an
            // interface", and so we drop packets instead of delivering them
            // locally.
            //
            // As per RFC 4862 section 5.4:
            //
            //   An address on which the Duplicate Address Detection
            //   procedure is applied is said to be tentative until the
            //   procedure has completed successfully. A tentative address
            //   is not considered "assigned to an interface" in the
            //   traditional sense.  That is, the interface must accept
            //   Neighbor Solicitation and Advertisement messages containing
            //   the tentative address in the Target Address field, but
            //   processes such packets differently from those whose Target
            //   Address matches an address assigned to the interface. Other
            //   packets addressed to the tentative address should be
            //   silently discarded. Note that the "other packets" include
            //   Neighbor Solicitation and Advertisement messages that have
            //   the tentative (i.e., unicast) address as the IP destination
            //   address and contain the tentative address in the Target
            //   Address field.  Such a case should not happen in normal
            //   operation, though, since these messages are multicasted in
            //   the Duplicate Address Detection procedure.
            //
            // That is, we accept no packets destined to a tentative
            // address. NS and NA packets should be addressed to a multicast
            // address that we would have joined during DAD so that we can
            // receive those packets.
            core_ctx.increment(|counters| &counters.version_rx.drop_for_tentative);
            ReceivePacketAction::Drop { reason: DropReason::Tentative }
        }
        None => receive_ip_packet_action_common::<Ipv6, _, _, _>(core_ctx, dst_ip, device, packet),
    }
}

/// Computes the action to take for multicast packets on behalf of
/// [`receive_ipv4_packet_action`] and [`receive_ipv6_packet_action`].
fn receive_ip_multicast_packet_action<
    I: IpLayerIpExt,
    B: ByteSlice,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC> + CounterContext<IpCounters<I>>,
>(
    core_ctx: &mut CC,
    device: &CC::DeviceId,
    packet: &I::Packet<B>,
    address_status: Option<I::AddressStatus>,
) -> ReceivePacketAction<I, CC::DeviceId> {
    let targets = multicast_forwarding::lookup_multicast_route_for_packet(core_ctx, packet, device);
    match (targets, address_status) {
        (Some(targets), address_status) => {
            if address_status.is_some() {
                core_ctx.increment(|counters| &counters.deliver_multicast);
            }
            // TODO(https://fxbug.dev/352570820): Increment Counters.
            ReceivePacketAction::MulticastForward { targets, address_status }
        }
        (None, Some(address_status)) => {
            // If the address was present on the device (e.g. the host is a
            // member of the multicast group), fallback to local delivery.
            core_ctx.increment(|counters| &counters.deliver_multicast);
            ReceivePacketAction::Deliver { address_status }
        }
        (None, None) => {
            // As per RFC 1122 Section 3.2.2
            //   An ICMP error message MUST NOT be sent as the result of
            //   receiving:
            //   ...
            //   * a datagram destined to an IP broadcast or IP multicast
            //     address
            //
            // As such, drop the packet
            // TODO(https://fxbug.dev/352570820): Increment Counters.
            ReceivePacketAction::Drop { reason: DropReason::MulticastNoInterest }
        }
    }
}

/// Computes the remaining protocol-agnostic actions on behalf of
/// [`receive_ipv4_packet_action`] and [`receive_ipv6_packet_action`].
fn receive_ip_packet_action_common<
    I: IpLayerIpExt,
    B: ByteSlice,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC> + CounterContext<IpCounters<I>>,
>(
    core_ctx: &mut CC,
    dst_ip: SpecifiedAddr<I::Addr>,
    device_id: &CC::DeviceId,
    packet: &I::Packet<B>,
) -> ReceivePacketAction<I, CC::DeviceId> {
    if dst_ip.is_multicast() {
        return receive_ip_multicast_packet_action(core_ctx, device_id, packet, None);
    }

    // The packet is not destined locally, so we attempt to forward it.
    if !core_ctx.is_device_unicast_forwarding_enabled(device_id) {
        // Forwarding is disabled; we are operating only as a host.
        //
        // For IPv4, per RFC 1122 Section 3.2.1.3, "A host MUST silently discard
        // an incoming datagram that is not destined for the host."
        //
        // For IPv6, per RFC 4443 Section 3.1, the only instance in which a host
        // sends an ICMPv6 Destination Unreachable message is when a packet is
        // destined to that host but on an unreachable port (Code 4 - "Port
        // unreachable"). Since the only sensible error message to send in this
        // case is a Destination Unreachable message, we interpret the RFC text
        // to mean that, consistent with IPv4's behavior, we should silently
        // discard the packet in this case.
        core_ctx.increment(|counters| &counters.forwarding_disabled);
        ReceivePacketAction::Drop { reason: DropReason::ForwardingDisabledInboundIface }
    } else {
        match lookup_route_table(core_ctx, None, *dst_ip) {
            Some(dst) => {
                core_ctx.increment(|counters| &counters.forward);
                ReceivePacketAction::Forward { original_dst: dst_ip, dst }
            }
            None => {
                core_ctx.increment(|counters| &counters.no_route_to_host);
                ReceivePacketAction::SendNoRouteToDest { dst: dst_ip }
            }
        }
    }
}

// Look up the route to a host.
fn lookup_route_table<I: IpLayerIpExt, BC, CC: IpStateContext<I, BC>>(
    core_ctx: &mut CC,
    device: Option<&CC::DeviceId>,
    dst_ip: I::Addr,
) -> Option<Destination<I::Addr, CC::DeviceId>> {
    core_ctx.with_main_ip_routing_table(|core_ctx, table| table.lookup(core_ctx, device, dst_ip))
}

/// Packed destination passed to [`IpDeviceSendContext::send_ip_frame`].
#[derive(Debug, Derivative)]
#[derivative(Eq(bound = "D: Eq"), PartialEq(bound = "D: PartialEq"))]
pub enum IpPacketDestination<I: BroadcastIpExt, D> {
    /// Broadcast packet.
    Broadcast(I::BroadcastMarker),

    /// Multicast packet to the specified IP.
    Multicast(MulticastAddr<I::Addr>),

    /// Send packet to the neighbor with the specified IP (the receiving
    /// node is either a router or the final recipient of the packet).
    Neighbor(SpecifiedAddr<I::Addr>),

    /// Loopback the packet to the specified device. Can be used only when
    /// sending to the loopback device.
    Loopback(D),
}

impl<I: BroadcastIpExt, D> IpPacketDestination<I, D> {
    /// Creates `IpPacketDestination` for IP address.
    pub fn from_addr(addr: SpecifiedAddr<I::Addr>) -> Self {
        match MulticastAddr::new(addr.into_addr()) {
            Some(mc_addr) => Self::Multicast(mc_addr),
            None => Self::Neighbor(addr),
        }
    }

    /// Create `IpPacketDestination` from `NextHop`.
    pub fn from_next_hop(next_hop: NextHop<I::Addr>, dst_ip: SpecifiedAddr<I::Addr>) -> Self {
        match next_hop {
            NextHop::RemoteAsNeighbor => Self::from_addr(dst_ip),
            NextHop::Gateway(gateway) => Self::Neighbor(gateway),
            NextHop::Broadcast(marker) => Self::Broadcast(marker),
        }
    }
}

/// The metadata associated with an outgoing IP packet.
#[derive(Debug)]
pub struct SendIpPacketMeta<I: IpExt, D, Src> {
    /// The outgoing device.
    pub device: D,

    /// The source address of the packet.
    pub src_ip: Src,

    /// The destination address of the packet.
    pub dst_ip: SpecifiedAddr<I::Addr>,

    /// The destination for the send operation.
    pub destination: IpPacketDestination<I, D>,

    /// The upper-layer protocol held in the packet's payload.
    pub proto: I::Proto,

    /// The time-to-live (IPv4) or hop limit (IPv6) for the packet.
    ///
    /// If not set, a default TTL may be used.
    pub ttl: Option<NonZeroU8>,

    /// An MTU to artificially impose on the whole IP packet.
    ///
    /// Note that the device's MTU will still be imposed on the packet.
    pub mtu: Option<u32>,

    /// Traffic Class (IPv6) or Type of Service (IPv4) field for the packet.
    pub dscp_and_ecn: DscpAndEcn,
}

impl<I: IpExt, D> From<SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>>
    for SendIpPacketMeta<I, D, Option<SpecifiedAddr<I::Addr>>>
{
    fn from(
        SendIpPacketMeta { device, src_ip, dst_ip, destination, proto, ttl, mtu, dscp_and_ecn }: SendIpPacketMeta<
            I,
            D,
            SpecifiedAddr<I::Addr>,
        >,
    ) -> SendIpPacketMeta<I, D, Option<SpecifiedAddr<I::Addr>>> {
        SendIpPacketMeta {
            device,
            src_ip: Some(src_ip),
            dst_ip,
            destination,
            proto,
            ttl,
            mtu,
            dscp_and_ecn,
        }
    }
}

/// Trait for abstracting the IP layer for locally-generated traffic.  That is,
/// traffic generated by the netstack itself (e.g. ICMP, IGMP, or MLD).
///
/// NOTE: Due to filtering rules, it is possible that the device provided in
/// `meta` will not be the device that final IP packet is actually sent from.
pub trait IpLayerHandler<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Encapsulate and send the provided transport packet and from the device
    /// provided in `meta`.
    fn send_ip_packet_from_device<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<I, &Self::DeviceId, Option<SpecifiedAddr<I::Addr>>>,
        body: S,
    ) -> Result<(), IpSendFrameError<S>>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut;

    /// Send an IP packet that doesn't require the encapsulation and other
    /// processing of [`send_ip_packet_from_device`] from the device specified
    /// in `meta`.
    // TODO(https://fxbug.dev/333908066): The packets going through this
    // function only hit the EGRESS filter hook, bypassing LOCAL_EGRESS.
    // Refactor callers and other functions to prevent this.
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        destination: IpPacketDestination<I, &Self::DeviceId>,
        body: S,
    ) -> Result<(), IpSendFrameError<S>>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut;
}

impl<
        I: IpLayerIpExt,
        BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
        CC: IpLayerEgressContext<I, BC> + IpDeviceStateContext<I, BC>,
    > IpLayerHandler<I, BC> for CC
{
    fn send_ip_packet_from_device<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<I, &CC::DeviceId, Option<SpecifiedAddr<I::Addr>>>,
        body: S,
    ) -> Result<(), IpSendFrameError<S>>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut,
    {
        send_ip_packet_from_device(self, bindings_ctx, meta, body, IpLayerPacketMetadata::default())
    }

    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        destination: IpPacketDestination<I, &Self::DeviceId>,
        body: S,
    ) -> Result<(), IpSendFrameError<S>>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut,
    {
        send_ip_frame(
            self,
            bindings_ctx,
            device,
            destination,
            body,
            IpLayerPacketMetadata::default(),
        )
    }
}

/// Sends an Ip packet with the specified metadata.
///
/// # Panics
///
/// Panics if either the source or destination address is the loopback address
/// and the device is a non-loopback device.
pub(crate) fn send_ip_packet_from_device<I, BC, CC, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    meta: SendIpPacketMeta<
        I,
        &<CC as DeviceIdContext<AnyDevice>>::DeviceId,
        Option<SpecifiedAddr<I::Addr>>,
    >,
    body: S,
    packet_metadata: IpLayerPacketMetadata<I, BC>,
) -> Result<(), IpSendFrameError<S>>
where
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
    CC: IpLayerEgressContext<I, BC> + IpDeviceStateContext<I, BC>,
    S: TransportPacketSerializer<I>,
    S::Buffer: BufferMut,
{
    let SendIpPacketMeta { device, src_ip, dst_ip, destination, proto, ttl, mtu, dscp_and_ecn } =
        meta;
    let next_packet_id = gen_ip_packet_id(core_ctx);
    let ttl = ttl.unwrap_or_else(|| core_ctx.get_hop_limit(device)).get();
    let src_ip = src_ip.map_or(I::UNSPECIFIED_ADDRESS, |a| a.get());
    let mut builder = I::PacketBuilder::new(src_ip, dst_ip.get(), ttl, proto);

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Wrap<'a, I: IpLayerIpExt> {
        builder: &'a mut I::PacketBuilder,
        next_packet_id: I::PacketId,
    }

    I::map_ip::<_, ()>(
        Wrap { builder: &mut builder, next_packet_id },
        |Wrap { builder, next_packet_id }| {
            builder.id(next_packet_id);
        },
        |Wrap { builder: _, next_packet_id: () }| {
            // IPv6 doesn't have packet IDs.
        },
    );

    builder.set_dscp_and_ecn(dscp_and_ecn);

    let body = body.encapsulate(builder);

    if let Some(mtu) = mtu {
        let body = NestedWithInnerIpPacket::new(body.with_size_limit(mtu as usize));
        send_ip_frame(core_ctx, bindings_ctx, device, destination, body, packet_metadata)
            .map_err(|ser| ser.map_serializer(|s| s.into_inner()).into_inner())
    } else {
        send_ip_frame(core_ctx, bindings_ctx, device, destination, body, packet_metadata)
            .map_err(|ser| ser.into_inner())
    }
}

/// Abstracts access to a [`filter::FilterHandler`] for core contexts.
pub trait FilterHandlerProvider<I: packet_formats::ip::IpExt, BT: FilterBindingsTypes>:
    DeviceIdContext<AnyDevice, DeviceId: filter::InterfaceProperties<BT::DeviceClass>>
{
    /// The filter handler.
    type Handler<'a>: filter::FilterHandler<I, BT, DeviceId = Self::DeviceId>
    where
        Self: 'a;

    /// Gets the filter handler for this context.
    fn filter_handler(&mut self) -> Self::Handler<'_>;
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    use netstack3_base::testutil::{FakeCoreCtx, FakeStrongDeviceId};
    use netstack3_base::{SendFrameContext, SendFrameError, SendableFrameMeta};

    /// A [`SendIpPacketMeta`] for dual stack contextx.
    #[derive(Debug, GenericOverIp)]
    #[generic_over_ip()]
    #[allow(missing_docs)]
    pub enum DualStackSendIpPacketMeta<D> {
        V4(SendIpPacketMeta<Ipv4, D, SpecifiedAddr<Ipv4Addr>>),
        V6(SendIpPacketMeta<Ipv6, D, SpecifiedAddr<Ipv6Addr>>),
    }

    impl<I: IpExt, D> From<SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>>
        for DualStackSendIpPacketMeta<D>
    {
        fn from(value: SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>) -> Self {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<I: IpExt, D>(SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>);
            use DualStackSendIpPacketMeta::*;
            I::map_ip_in(Wrap(value), |Wrap(value)| V4(value), |Wrap(value)| V6(value))
        }
    }

    impl<I: IpExt, S, DeviceId, BC>
        SendableFrameMeta<FakeCoreCtx<S, DualStackSendIpPacketMeta<DeviceId>, DeviceId>, BC>
        for SendIpPacketMeta<I, DeviceId, SpecifiedAddr<I::Addr>>
    {
        fn send_meta<SS>(
            self,
            core_ctx: &mut FakeCoreCtx<S, DualStackSendIpPacketMeta<DeviceId>, DeviceId>,
            bindings_ctx: &mut BC,
            frame: SS,
        ) -> Result<(), SendFrameError<SS>>
        where
            SS: Serializer,
            SS::Buffer: BufferMut,
        {
            SendFrameContext::send_frame(
                &mut core_ctx.frames,
                bindings_ctx,
                DualStackSendIpPacketMeta::from(self),
                frame,
            )
        }
    }

    /// Error returned when the IP version doesn't match.
    #[derive(Debug)]
    pub struct WrongIpVersion;

    impl<D> DualStackSendIpPacketMeta<D> {
        /// Returns the internal [`SendIpPacketMeta`] if this is carrying the
        /// version matching `I`.
        pub fn try_as<I: IpExt>(
            &self,
        ) -> Result<&SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>, WrongIpVersion> {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpExt, D>(
                Option<&'a SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>>,
            );
            use DualStackSendIpPacketMeta::*;
            let Wrap(dual_stack) = I::map_ip(
                self,
                |value| {
                    Wrap(match value {
                        V4(meta) => Some(meta),
                        V6(_) => None,
                    })
                },
                |value| {
                    Wrap(match value {
                        V4(_) => None,
                        V6(meta) => Some(meta),
                    })
                },
            );
            dual_stack.ok_or(WrongIpVersion)
        }
    }

    impl<I, BC, S, Meta, DeviceId> FilterHandlerProvider<I, BC> for FakeCoreCtx<S, Meta, DeviceId>
    where
        I: packet_formats::ip::IpExt,
        BC: FilterBindingsContext,
        DeviceId: FakeStrongDeviceId + filter::InterfaceProperties<BC::DeviceClass>,
    {
        type Handler<'a> = filter::testutil::NoopImpl<DeviceId> where Self: 'a;

        fn filter_handler(&mut self) -> Self::Handler<'_> {
            filter::testutil::NoopImpl::default()
        }
    }
}
