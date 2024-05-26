// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::vec::Vec;
use core::{
    cmp::Ordering,
    fmt::Debug,
    hash::Hash,
    num::{NonZeroU16, NonZeroU32, NonZeroU8},
    sync::atomic::{self, AtomicU16},
};

use const_unwrap::const_unwrap_option;
use derivative::Derivative;
use explicit::ResultExt as _;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr, Mtu, Subnet,
    },
    MulticastAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use netstack3_base::{
    socket::SocketIpAddrExt as _,
    sync::{Mutex, RwLock},
    AnyDevice, CoreTimerContext, Counter, CounterContext, DeviceIdContext, DeviceIdentifier as _,
    EventContext, FrameDestination, HandleableTimer, Inspectable, Inspector, InstantContext,
    NestedIntoCoreTimerCtx, StrongDeviceIdentifier, TimerContext, TimerHandler, TracingContext,
};
use netstack3_filter::{
    self as filter, ConntrackConnection, FilterBindingsContext, FilterBindingsTypes,
    FilterHandler as _, FilterIpMetadata, FilterTimerId, ForwardedPacket, IngressVerdict, IpPacket,
    NestedWithInnerIpPacket, TransportPacketSerializer,
};
use packet::{Buf, BufferMut, ParseMetadata, Serializer};
use packet_formats::{
    error::IpParseError,
    ip::{IpPacket as _, IpPacketBuilder as _, IpProtoExt},
    ipv4::{Ipv4FragmentType, Ipv4Packet},
    ipv6::Ipv6Packet,
};
use thiserror::Error;
use tracing::{debug, error, trace};

use crate::internal::{
    device::{
        self, slaac::SlaacCounters, state::IpDeviceStateIpExt, IpDeviceAddr,
        IpDeviceBindingsContext, IpDeviceIpExt, IpDeviceSendContext,
    },
    forwarding::{ForwardingTable, IpForwardingDeviceContext},
    icmp::{
        IcmpBindingsTypes, IcmpErrorHandler, IcmpHandlerIpExt, IcmpIpExt, Icmpv4Error,
        Icmpv4ErrorKind, Icmpv4State, Icmpv4StateBuilder, Icmpv6ErrorKind, Icmpv6State,
        Icmpv6StateBuilder,
    },
    ipv6,
    ipv6::Ipv6PacketAction,
    path_mtu::{PmtuBindingsTypes, PmtuCache, PmtuTimerId},
    raw::{RawIpSocketHandler, RawIpSocketMap, RawIpSocketsBindingsTypes},
    reassembly::{
        FragmentBindingsTypes, FragmentHandler, FragmentProcessingState, FragmentTimerId,
        IpPacketFragmentCache,
    },
    socket::{IpSocketBindingsContext, IpSocketContext, IpSocketHandler},
    types::{
        self, Destination, IpTypesIpExt, NextHop, ResolvedRoute, RoutableIpAddr,
        WrapBroadcastMarker,
    },
};

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

/// An [`Ip`] extension trait adding functionality specific to the IP layer.
pub trait IpExt: packet_formats::ip::IpExt + IcmpIpExt + IpTypesIpExt + IpProtoExt {
    /// The type used to specify an IP packet's source address in a call to
    /// [`IpTransportContext::receive_ip_packet`].
    ///
    /// For IPv4, this is `Ipv4Addr`. For IPv6, this is [`Ipv6SourceAddr`].
    type RecvSrcAddr: Into<Self::Addr>;
    /// The length of an IP header without any IP options.
    const IP_HEADER_LENGTH: NonZeroU32;
    /// The maximum payload size an IP payload can have.
    const IP_MAX_PAYLOAD_LENGTH: NonZeroU32;
}

impl IpExt for Ipv4 {
    type RecvSrcAddr = Ipv4Addr;
    const IP_HEADER_LENGTH: NonZeroU32 =
        const_unwrap_option(NonZeroU32::new(packet_formats::ipv4::HDR_PREFIX_LEN as u32));
    const IP_MAX_PAYLOAD_LENGTH: NonZeroU32 =
        const_unwrap_option(NonZeroU32::new(u16::MAX as u32 - Self::IP_HEADER_LENGTH.get()));
}

impl IpExt for Ipv6 {
    type RecvSrcAddr = Ipv6SourceAddr;
    const IP_HEADER_LENGTH: NonZeroU32 =
        const_unwrap_option(NonZeroU32::new(packet_formats::ipv6::IPV6_FIXED_HDR_LEN as u32));
    const IP_MAX_PAYLOAD_LENGTH: NonZeroU32 = const_unwrap_option(NonZeroU32::new(u16::MAX as u32));
}

/// Informs the transport layer of parameters for transparent local delivery.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct TransparentLocalDelivery<I: IpExt> {
    /// The local delivery address.
    pub addr: SpecifiedAddr<I::Addr>,
    /// The local delivery port.
    pub port: NonZeroU16,
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
        transport_override: Option<TransparentLocalDelivery<I>>,
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
        _transport_override: Option<TransparentLocalDelivery<I>>,
    ) -> Result<(), (B, TransportReceiveError)> {
        Err((buffer, TransportReceiveError::ProtocolUnsupported))
    }
}

/// The execution context provided by the IP layer to transport layer protocols.
pub trait TransportIpContext<I: IpExt, BC>:
    DeviceIdContext<AnyDevice> + IpSocketHandler<I, BC>
{
    /// The iterator returned by
    /// [`TransportIpContext::get_devices_with-assigned_addr`].
    type DevicesWithAddrIter<'s>: Iterator<Item = Self::DeviceId>
    where
        Self: 's;

    /// Is this one of our local addresses, and is it in the assigned state?
    ///
    /// Returns an iterator over all the local interfaces for which `addr` is an
    /// associated address, and, for IPv6, for which it is in the "assigned"
    /// state.
    fn get_devices_with_assigned_addr(
        &mut self,
        addr: SpecifiedAddr<I::Addr>,
    ) -> Self::DevicesWithAddrIter<'_>;

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

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I: IpExt,
        BC,
        CC: IpDeviceContext<I, BC>
            + IpSocketHandler<I, BC>
            + IpStateContext<I, BC>
            + UseTransportIpContextBlanket,
    > TransportIpContext<I, BC> for CC
{
    type DevicesWithAddrIter<'s> = <Vec<CC::DeviceId> as IntoIterator>::IntoIter where CC: 's;

    fn get_devices_with_assigned_addr(
        &mut self,
        addr: SpecifiedAddr<<I as Ip>::Addr>,
    ) -> Self::DevicesWithAddrIter<'_> {
        self.with_address_statuses(addr, |it| {
            it.filter_map(|(device, state)| is_unicast_assigned::<I>(&state).then_some(device))
                .collect::<Vec<_>>()
        })
        .into_iter()
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
        dst: SpecifiedAddr<<I as Ip>::Addr>,
        device: Option<&Self::DeviceId>,
    ) {
        match self
            .with_ip_routing_table(|core_ctx, routes| routes.lookup(core_ctx, device, dst.get()))
        {
            Some(Destination { next_hop, device }) => {
                let neighbor = match next_hop {
                    NextHop::RemoteAsNeighbor => dst,
                    NextHop::Gateway(gateway) => gateway,
                    NextHop::Broadcast(marker) => {
                        I::map_ip::<_, ()>(
                            WrapBroadcastMarker(marker),
                            |WrapBroadcastMarker(())| {
                                tracing::debug!(
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
                tracing::debug!("can't confirm {dst:?}@{device:?} as reachable: no route");
            }
        }
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

/// The status of an IPv6 address.
#[allow(missing_docs)]
pub enum Ipv6PresentAddressStatus {
    Multicast,
    UnicastAssigned,
    UnicastTentative,
}

/// An extension trait providing IP layer properties.
pub trait IpLayerIpExt: IpExt {
    /// IP Address status.
    type AddressStatus;
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
        + IpForwardingDeviceContext<I>
        + IpDeviceStateContext<I, BC>;

    /// Calls the function with an immutable reference to IP routing table.
    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to IP routing table.
    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

/// Provices access to an IP device's state for the IP layer.
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
    type DeviceAndAddressStatusIter<'a, 's>: Iterator<Item = (Self::DeviceId, I::AddressStatus)>
    where
        Self: IpDeviceContext<I, BC> + 's;

    /// Provides access to the status of an address.
    ///
    /// Calls the provided callback with an iterator over the devices for which
    /// the address is assigned and the status of the assignment for each
    /// device.
    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<I::Addr>,
        cb: F,
    ) -> R;

    /// Returns true iff the device has forwarding enabled.
    fn is_device_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool;

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
>: IpStateContext<I, BC> + IpDeviceContext<I, BC>
{
}

impl<
        I: IpLayerIpExt,
        BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
        CC: IpStateContext<I, BC> + IpDeviceContext<I, BC>,
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
    local_ip: SpecifiedAddr<I::Addr>,
) -> bool {
    match core_ctx.address_status_for_device(local_ip, device) {
        AddressStatus::Present(status) => is_unicast_assigned::<I>(&status),
        AddressStatus::Unassigned => false,
    }
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
) -> Result<IpDeviceAddr<I::Addr>, ResolveRouteError> {
    if let Some(local_ip) = local_ip {
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
/// If `local_ip` is specified the resolved route is limited to those that egress
/// over a device with the address assigned.
pub fn resolve_route_to_destination<
    I: Ip + IpDeviceStateIpExt + IpDeviceIpExt + IpLayerIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId> + IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC> + device::IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    device: Option<&CC::DeviceId>,
    local_ip: Option<IpDeviceAddr<I::Addr>>,
    addr: Option<RoutableIpAddr<I::Addr>>,
) -> Result<ResolvedRoute<I, CC::DeviceId>, ResolveRouteError> {
    enum LocalDelivery<A, D> {
        WeakLoopback { addr: A, device: D },
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
    let local_delivery_instructions: Option<LocalDelivery<RoutableIpAddr<I::Addr>, CC::DeviceId>> =
        addr.and_then(|addr| {
            match device {
                Some(device) => match core_ctx.address_status_for_device(addr.into(), device) {
                    AddressStatus::Present(status) => {
                        // If the destination is an address assigned to the
                        // requested egress interface, route locally via the strong
                        // host model.
                        is_unicast_assigned::<I>(&status)
                            .then_some(LocalDelivery::StrongForDevice(device.clone()))
                    }
                    AddressStatus::Unassigned => None,
                },
                None => {
                    // If the destination is an address assigned on an interface,
                    // and an egress interface wasn't specifically selected, route
                    // via the loopback device operating in a weak host model, with
                    // the exception that if either the source or destination
                    // addresses needs a zone ID, then use strong host to enforce
                    // that the source and destination addresses are assigned to
                    // the same interface.
                    //
                    // TODO(https://fxbug.dev/322539434): Linux is more permissive
                    // about allowing cross-device local delivery even when
                    // SO_BINDTODEVICE or link-local addresses are involved, and this
                    // behavior may need to be emulated.
                    core_ctx
                        .with_address_statuses(addr.into(), |mut it| {
                            it.find_map(|(device, status)| {
                                is_unicast_assigned::<I>(&status).then_some(device)
                            })
                        })
                        .map(|device| {
                            if local_ip.is_some_and(|local_ip| local_ip.as_ref().must_have_zone())
                                || addr.as_ref().must_have_zone()
                            {
                                LocalDelivery::StrongForDevice(device)
                            } else {
                                LocalDelivery::WeakLoopback { addr, device }
                            }
                        })
                }
            }
        });

    match local_delivery_instructions {
        Some(local_delivery) => {
            let loopback = match core_ctx.loopback_id() {
                None => return Err(ResolveRouteError::Unreachable),
                Some(loopback) => loopback,
            };

            let (local_ip, dest_device) = match local_delivery {
                LocalDelivery::WeakLoopback { addr, device } => match local_ip {
                    Some(local_ip) => core_ctx
                        .with_address_statuses(local_ip.into(), |mut it| {
                            it.find_map(|(device, status)| {
                                is_unicast_assigned::<I>(&status).then_some(device)
                            })
                        })
                        .map(|device| (local_ip, device))
                        .ok_or(ResolveRouteError::NoSrcAddr)?,
                    None => (addr, device),
                },
                LocalDelivery::StrongForDevice(device) => {
                    (get_local_addr(core_ctx, local_ip, &device, addr)?, device)
                }
            };
            Ok(ResolvedRoute {
                src_addr: local_ip,
                local_delivery_device: Some(dest_device),
                device: loopback,
                next_hop: NextHop::RemoteAsNeighbor,
            })
        }
        None => {
            core_ctx
                .with_ip_routing_table(|core_ctx, table| {
                    let mut matching_with_addr = table.lookup_filter_map(
                        core_ctx,
                        device,
                        addr.map_or(I::UNSPECIFIED_ADDRESS, |a| a.addr()),
                        |core_ctx, d| Some(get_local_addr(core_ctx, local_ip, d, addr)),
                    );

                    let first_error = match matching_with_addr.next() {
                        Some((Destination { device, next_hop }, Ok(addr))) => {
                            return Ok((Destination { device: device.clone(), next_hop }, addr))
                        }
                        Some((_, Err(e))) => e,
                        None => return Err(ResolveRouteError::Unreachable),
                    };

                    matching_with_addr
                        .filter_map(|(d, r)| {
                            // Select successful routes. We ignore later errors
                            // since we've already saved the first one.
                            r.ok_checked::<ResolveRouteError>().map(|a| (d, a))
                        })
                        .next()
                        .map_or(Err(first_error), |(Destination { device, next_hop }, addr)| {
                            Ok((Destination { device: device.clone(), next_hop }, addr))
                        })
                })
                .map(|(Destination { device, next_hop }, local_ip)| ResolvedRoute {
                    src_addr: local_ip,
                    device,
                    local_delivery_device: None,
                    next_hop,
                })
        }
    }
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
    ) -> Result<ResolvedRoute<I, CC::DeviceId>, ResolveRouteError> {
        resolve_route_to_destination(self, device, local_ip, Some(addr))
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
    ) -> Result<(), S>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut,
    {
        send_ip_packet_from_device(self, bindings_ctx, meta.into(), body, packet_metadata)
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
        transport_override: Option<TransparentLocalDelivery<I>>,
    ) -> Result<(), TransportReceiveError>;
}

/// A marker trait for all the contexts required for IP ingress.
pub trait IpLayerIngressContext<
    I: IpLayerIpExt + IcmpHandlerIpExt,
    BC: IpLayerBindingsContext<I, Self::DeviceId>,
>:
    IpTransportDispatchContext<I, BC, DeviceId = Self::DeviceId_>
    + IpDeviceStateContext<I, BC>
    + IpDeviceSendContext<I, BC>
    + IcmpErrorHandler<I, BC>
    + IpLayerContext<I, BC>
    + FragmentHandler<I, BC>
    + FilterHandlerProvider<I, BC>
    + RawIpSocketHandler<I, BC>
{
    // This is working around the fact that currently, where clauses are only
    // elaborated for supertraits, and not, for example, bounds on associated types
    // as we have here.
    //
    // See https://github.com/rust-lang/rust/issues/20671#issuecomment-1905186183
    // for more discussion.
    type DeviceId_: filter::InterfaceProperties<BC::DeviceClass> + Debug;
}

impl<
        I: IpLayerIpExt + IcmpHandlerIpExt,
        BC: IpLayerBindingsContext<I, CC::DeviceId>,
        CC: IpTransportDispatchContext<I, BC>
            + IpDeviceStateContext<I, BC>
            + IpDeviceSendContext<I, BC>
            + IcmpErrorHandler<I, BC>
            + IpLayerContext<I, BC>
            + FragmentHandler<I, BC>
            + FilterHandlerProvider<I, BC>
            + RawIpSocketHandler<I, BC>,
    > IpLayerIngressContext<I, BC> for CC
where
    Self::DeviceId: filter::InterfaceProperties<BC::DeviceClass>,
{
    type DeviceId_ = Self::DeviceId;
}

/// A marker trait for all the contexts required for IP egress.
pub(crate) trait IpLayerEgressContext<I, BC>:
    IpDeviceSendContext<I, BC, DeviceId = Self::DeviceId_> + FilterHandlerProvider<I, BC>
where
    I: IpLayerIpExt,
    BC: FilterBindingsContext,
{
    // This is working around the fact that currently, where clauses are only
    // elaborated for supertraits, and not, for example, bounds on associated types
    // as we have here.
    //
    // See https://github.com/rust-lang/rust/issues/20671#issuecomment-1905186183
    // for more discussion.
    type DeviceId_: filter::InterfaceProperties<BC::DeviceClass> + StrongDeviceIdentifier + Debug;
}

impl<I, BC, CC> IpLayerEgressContext<I, BC> for CC
where
    I: IpLayerIpExt,
    BC: FilterBindingsContext,
    CC: IpDeviceSendContext<I, BC> + FilterHandlerProvider<I, BC>,
    Self::DeviceId: filter::InterfaceProperties<BC::DeviceClass>,
{
    type DeviceId_ = Self::DeviceId;
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
        BC: TimerContext + IpLayerBindingsTypes,
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
        BC: TimerContext + IpLayerBindingsTypes,
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
    pub icmp: Icmpv4State<StrongDeviceId::Weak, BT>,
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
    pub icmp: Icmpv6State<StrongDeviceId::Weak, BT>,
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
    OrderedLockAccess<ForwardingTable<I, D>> for IpStateInner<I, D, BT>
{
    type Lock = RwLock<ForwardingTable<I, D>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.table)
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
    /// Version specific rx counters.
    pub version_rx: I::RxCounters,
}

/// IPv4-specific Rx counters.
#[derive(Default)]
pub struct Ipv4RxCounters {
    /// Count of incoming IPv4 packets delivered.
    pub deliver: Counter,
}

impl Inspectable for Ipv4RxCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { deliver } = self;
        inspector.record_counter("Delivered", deliver);
    }
}

/// IPv6-specific Rx counters.
#[derive(Default)]
pub struct Ipv6RxCounters {
    /// Count of incoming IPv6 multicast packets delivered.
    pub deliver_multicast: Counter,
    /// Count of incoming IPv6 unicast packets delivered.
    pub deliver_unicast: Counter,
    /// Count of incoming IPv6 packets dropped because the destination address
    /// is only tentatively assigned to the device.
    pub drop_for_tentative: Counter,
    /// Count of incoming IPv6 packets dropped due to a non-unicast source address.
    pub non_unicast_source: Counter,
    /// Count of incoming IPv6 packets discarded while processing extension
    /// headers.
    pub extension_header_discard: Counter,
}

impl Inspectable for Ipv6RxCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self {
            deliver_multicast,
            deliver_unicast,
            drop_for_tentative,
            non_unicast_source,
            extension_header_discard,
        } = self;
        inspector.record_counter("DeliveredMulticast", deliver_multicast);
        inspector.record_counter("DeliveredUnicast", deliver_unicast);
        inspector.record_counter("DroppedTentativeDst", drop_for_tentative);
        inspector.record_counter("DroppedNonUnicastSrc", non_unicast_source);
        inspector.record_counter("DroppedExtensionHeader", extension_header_discard);
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

/// The inner state for the IP layer for IP version `I`.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct IpStateInner<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpStateBindingsTypes> {
    table: RwLock<ForwardingTable<I, D>>,
    fragment_cache: Mutex<IpPacketFragmentCache<I, BT>>,
    pmtu_cache: Mutex<PmtuCache<I, BT>>,
    counters: IpCounters<I>,
    raw_sockets: RwLock<RawIpSocketMap<I, D::Weak, BT>>,
    filter: RwLock<filter::State<I, BT>>,
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BT: IpStateBindingsTypes> IpStateInner<I, D, BT> {
    /// Gets the IP counters.
    pub fn counters(&self) -> &IpCounters<I> {
        &self.counters
    }

    /// Provides direct access to the path MTU cache.
    #[cfg(any(test, feature = "testutils"))]
    pub fn pmtu_cache(&self) -> &Mutex<PmtuCache<I, BT>> {
        &self.pmtu_cache
    }

    /// Provides direct access to the forwarding table.
    #[cfg(any(test, feature = "testutils"))]
    pub fn table(&self) -> &RwLock<ForwardingTable<I, D>> {
        &self.table
    }
}

impl<I: IpLayerIpExt, D: StrongDeviceIdentifier, BC: TimerContext + IpStateBindingsTypes>
    IpStateInner<I, D, BC>
{
    /// Creates a new inner IP layer state.
    pub fn new<CC: CoreTimerContext<IpLayerTimerId, BC>>(bindings_ctx: &mut BC) -> Self {
        Self {
            table: Default::default(),
            fragment_cache: Mutex::new(
                IpPacketFragmentCache::new::<NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx),
            ),
            pmtu_cache: Mutex::new(PmtuCache::new::<NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx)),
            counters: Default::default(),
            raw_sockets: Default::default(),
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
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC) {
        match self {
            IpLayerTimerId::ReassemblyTimeoutv4(id) => core_ctx.handle_timer(bindings_ctx, id),
            IpLayerTimerId::ReassemblyTimeoutv6(id) => core_ctx.handle_timer(bindings_ctx, id),
            IpLayerTimerId::PmtuTimeoutv4(id) => core_ctx.handle_timer(bindings_ctx, id),
            IpLayerTimerId::PmtuTimeoutv6(id) => core_ctx.handle_timer(bindings_ctx, id),
            IpLayerTimerId::FilterTimerv4(id) => core_ctx.handle_timer(bindings_ctx, id),
            IpLayerTimerId::FilterTimerv6(id) => core_ctx.handle_timer(bindings_ctx, id),
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
    transport_override: Option<TransparentLocalDelivery<Ipv4>>,
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
        .dispatch_receive_ip_packet(
            bindings_ctx,
            device,
            src_ip,
            dst_ip,
            proto,
            buffer,
            transport_override,
        )
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
    transport_override: Option<TransparentLocalDelivery<Ipv6>>,
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
        .dispatch_receive_ip_packet(
            bindings_ctx,
            device,
            src_ip,
            dst_ip,
            proto,
            buffer,
            transport_override,
        )
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
    next_hop: SpecifiedAddr<I::Addr>,
    mut body: S,
    broadcast: Option<I::BroadcastMarker>,
    mut packet_metadata: IpLayerPacketMetadata<I, BC>,
) -> Result<(), S>
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
    core_ctx.send_ip_frame(bindings_ctx, device, next_hop, body, broadcast, proof)
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
macro_rules! process_fragment {
    ($core_ctx:expr, $bindings_ctx:expr, $dispatch:ident, $device:ident, $frame_dst:expr, $buffer:expr, $packet:expr, $ip:ident, $packet_metadata:expr) => {{
        match FragmentHandler::<$ip, _>::process_fragment::<&mut [u8]>(
            $core_ctx,
            $bindings_ctx,
            $packet,
        ) {
            // Handle the packet right away since reassembly is not needed.
            FragmentProcessingState::NotNeeded(packet) => {
                trace!("receive_ip_packet: not fragmented");
                // TODO(joshlf):
                // - Check for already-expired TTL?
                $dispatch(
                    $core_ctx,
                    $bindings_ctx,
                    $device,
                    $frame_dst,
                    packet,
                    $packet_metadata,
                    None,
                ).unwrap_or_else(|err| {
                    err.respond_with_icmp_error($core_ctx, $bindings_ctx, $buffer, $device)
                })
            }
            // Ready to reassemble a packet.
            FragmentProcessingState::Ready { key, packet_len } => {
                trace!("receive_ip_packet: fragmented, ready for reassembly");
                // Allocate a buffer of `packet_len` bytes.
                let mut buffer = Buf::new(alloc::vec![0; packet_len], ..);
                // The packet metadata associated with this last fragment will
                // be dropped and a new metadata struct created for the
                // reassembled packet.
                $packet_metadata.acknowledge_drop();

                // Attempt to reassemble the packet.
                match FragmentHandler::<$ip, _>::reassemble_packet(
                    $core_ctx,
                    $bindings_ctx,
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
                        $dispatch::<_, _,>(
                            $core_ctx,
                            $bindings_ctx,
                            $device,
                            $frame_dst,
                            packet,
                            packet_metadata,
                            None,
                        ).unwrap_or_else(|err| {
                            err.respond_with_icmp_error($core_ctx, $bindings_ctx, $buffer, $device)
                        })
                    }
                    // TODO(ghanan): Handle reassembly errors, remove
                    // `allow(unreachable_patterns)` when complete.
                    _ => {
                        $packet_metadata.acknowledge_drop();
                        return;
                    },
                    #[allow(unreachable_patterns)]
                    Err(e) => {
                        $core_ctx.increment(|counters: &IpCounters<$ip>| {
                            &counters.fragment_reassembly_error
                        });
                        $packet_metadata.acknowledge_drop();
                        trace!("receive_ip_packet: fragmented, failed to reassemble: {:?}", e);
                    }
                }
            }
            // Cannot proceed since we need more fragments before we
            // can reassemble a packet.
            FragmentProcessingState::NeedMoreFragments => {
                $packet_metadata.acknowledge_drop();
                $core_ctx.increment(|counters: &IpCounters<$ip>| {
                    &counters.need_more_fragments
                });
                trace!("receive_ip_packet: fragmented, need more before reassembly")
            }
            // TODO(ghanan): Handle invalid fragments.
            FragmentProcessingState::InvalidFragment => {
                $packet_metadata.acknowledge_drop();
                $core_ctx.increment(|counters: &IpCounters<$ip>| {
                    &counters.invalid_fragment
                });
                trace!("receive_ip_packet: fragmented, invalid")
            }
            FragmentProcessingState::OutOfMemory => {
                $packet_metadata.acknowledge_drop();
                $core_ctx.increment(|counters: &IpCounters<$ip>| {
                    &counters.fragment_cache_full
                });
                trace!("receive_ip_packet: fragmented, dropped because OOM")
            }
        };
    }};
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

    let dst_ip = match SpecifiedAddr::new(packet.dst_ip()) {
        Some(ip) => ip,
        None => {
            core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.unspecified_destination);
            debug!("receive_ipv4_packet: Received packet with unspecified destination IP address; dropping");
            return;
        }
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
                debug!("receive_ipv4_packet: Received packet with unspecified destination IP address; dropping");
                return;
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
                Some(TransparentLocalDelivery { addr, port }),
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

    match receive_ipv4_packet_action(core_ctx, bindings_ctx, device, dst_ip) {
        ReceivePacketAction::Deliver => {
            trace!("receive_ipv4_packet: delivering locally");

            // Process a potential IPv4 fragment if the destination is this
            // host.
            //
            // We process IPv4 packet reassembly here because, for IPv4, the
            // fragment data is in the header itself so we can handle it right
            // away.
            //
            // Note, the `process_fragment` function (which is called by the
            // `process_fragment!` macro) could panic if the packet does not
            // have fragment data. However, we are guaranteed that it will not
            // panic because the fragment data is in the fixed header so it is
            // always present (even if the fragment data has values that implies
            // that the packet is not fragmented).
            process_fragment!(
                core_ctx,
                bindings_ctx,
                dispatch_receive_ipv4_packet,
                device,
                frame_dst,
                buffer,
                packet,
                Ipv4,
                packet_metadata
            );
        }
        ReceivePacketAction::Forward { dst: Destination { device: dst_device, next_hop } } => {
            let (next_hop, broadcast) = match next_hop {
                NextHop::RemoteAsNeighbor => (dst_ip, None),
                NextHop::Gateway(gateway) => (gateway, None),
                NextHop::Broadcast(marker) => (dst_ip, Some(marker)),
            };
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
                    next_hop,
                    packet,
                    broadcast,
                    packet_metadata,
                ) {
                    Ok(()) => (),
                    Err(p) => {
                        let _: ForwardedPacket<_, B> = p;
                        core_ctx.increment(|counters: &IpCounters<Ipv4>| &counters.mtu_exceeded);
                        // TODO(https://fxbug.dev/42167236): Encode the MTU error
                        // more obviously in the type system.
                        debug!("failed to forward IPv4 packet: MTU exceeded");
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
        ReceivePacketAction::SendNoRouteToDest => {
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

    let src_ip = match packet.src_ipv6() {
        Some(ip) => ip,
        None => {
            debug!(
                "receive_ipv6_packet: received packet from non-unicast source {}; dropping",
                packet.src_ip()
            );
            core_ctx
                .increment(|counters: &IpCounters<Ipv6>| &counters.version_rx.non_unicast_source);
            return;
        }
    };
    let dst_ip = match SpecifiedAddr::new(packet.dst_ip()) {
        Some(ip) => ip,
        None => {
            core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.unspecified_destination);
            debug!("receive_ipv6_packet: Received packet with unspecified destination IP address; dropping");
            return;
        }
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
                debug!("receive_ipv6_packet: Received packet with unspecified destination IP address; dropping");
                return;
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
                Some(TransparentLocalDelivery { addr, port }),
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

    match receive_ipv6_packet_action(core_ctx, bindings_ctx, device, dst_ip) {
        ReceivePacketAction::Deliver => {
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
                        None,
                    )
                    .unwrap_or_else(|err| {
                        err.respond_with_icmp_error(core_ctx, bindings_ctx, buffer, device)
                    });
                }
                Ipv6PacketAction::ProcessFragment => {
                    trace!(
                        "receive_ipv6_packet: handled IPv6 extension headers: handling fragmented packet"
                    );

                    // Note, the `IpPacketFragmentCache::process_fragment`
                    // method (which is called by the `process_fragment!` macro)
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
                    process_fragment!(
                        core_ctx,
                        bindings_ctx,
                        dispatch_receive_ipv6_packet,
                        device,
                        frame_dst,
                        buffer,
                        packet,
                        Ipv6,
                        packet_metadata
                    );
                }
            }
        }
        ReceivePacketAction::Forward { dst: Destination { device: dst_device, next_hop } } => {
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
                    Ipv6PacketAction::ProcessFragment => unreachable!("When forwarding packets, we should only ever look at the hop by hop options extension header (if present)"),
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

                let next_hop = match next_hop {
                    NextHop::RemoteAsNeighbor => dst_ip,
                    NextHop::Gateway(gateway) => gateway,
                    NextHop::Broadcast(never) => match never {},
                };
                packet.set_ttl(ttl - 1);
                let (src, dst, proto, meta) = packet.into_metadata();
                let packet = ForwardedPacket::new(src, dst, proto, meta, buffer);

                if let Err(packet) = send_ip_frame(
                    core_ctx,
                    bindings_ctx,
                    &dst_device,
                    next_hop,
                    packet,
                    None,
                    packet_metadata,
                ) {
                    // TODO(https://fxbug.dev/42167236): Encode the MTU error more
                    // obviously in the type system.
                    core_ctx.increment(|counters: &IpCounters<Ipv6>| &counters.mtu_exceeded);
                    debug!("failed to forward IPv6 packet: MTU exceeded");
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
                            packet.into_buffer(),
                            Icmpv6ErrorKind::PacketTooBig {
                                proto,
                                header_len: meta.header_len(),
                                mtu,
                            },
                        );
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
        ReceivePacketAction::SendNoRouteToDest => {
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
pub enum ReceivePacketAction<A: IpAddress, DeviceId>
where
    A::Version: IpTypesIpExt,
{
    /// Deliver the packet locally.
    Deliver,
    /// Forward the packet to the given destination.
    #[allow(missing_docs)]
    Forward { dst: Destination<A, DeviceId> },
    /// Send a Destination Unreachable ICMP error message to the packet's sender
    /// and drop the packet.
    ///
    /// For ICMPv4, use the code "net unreachable". For ICMPv6, use the code "no
    /// route to destination".
    SendNoRouteToDest,
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
    /// Packet should be forwarded but packet's inbound interface has forwarding
    /// disabled.
    ForwardingDisabledInboundIface,
}

/// Computes the action to take in order to process a received IPv4 packet.
pub fn receive_ipv4_packet_action<
    BC: IpLayerBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpLayerContext<Ipv4, BC> + CounterContext<IpCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
) -> ReceivePacketAction<Ipv4Addr, CC::DeviceId> {
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
            Ipv4PresentAddressStatus::LimitedBroadcast
            | Ipv4PresentAddressStatus::SubnetBroadcast
            | Ipv4PresentAddressStatus::Multicast
            | Ipv4PresentAddressStatus::Unicast
            | Ipv4PresentAddressStatus::LoopbackSubnet,
        ) => {
            core_ctx.increment(|counters| &counters.version_rx.deliver);
            ReceivePacketAction::Deliver
        }
        None => {
            receive_ip_packet_action_common::<Ipv4, _, _>(core_ctx, bindings_ctx, dst_ip, device)
        }
    }
}

/// Computes the action to take in order to process a received IPv6 packet.
pub fn receive_ipv6_packet_action<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpLayerContext<Ipv6, BC> + CounterContext<IpCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
) -> ReceivePacketAction<Ipv6Addr, CC::DeviceId> {
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
        Some(Ipv6PresentAddressStatus::Multicast) => {
            core_ctx.increment(|counters| &counters.version_rx.deliver_multicast);
            ReceivePacketAction::Deliver
        }
        Some(Ipv6PresentAddressStatus::UnicastAssigned) => {
            core_ctx.increment(|counters| &counters.version_rx.deliver_unicast);
            ReceivePacketAction::Deliver
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
        None => {
            receive_ip_packet_action_common::<Ipv6, _, _>(core_ctx, bindings_ctx, dst_ip, device)
        }
    }
}

/// Computes the remaining protocol-agnostic actions on behalf of
/// [`receive_ipv4_packet_action`] and [`receive_ipv6_packet_action`].
fn receive_ip_packet_action_common<
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC> + CounterContext<IpCounters<I>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    dst_ip: SpecifiedAddr<I::Addr>,
    device_id: &CC::DeviceId,
) -> ReceivePacketAction<I::Addr, CC::DeviceId> {
    // The packet is not destined locally, so we attempt to forward it.
    if !core_ctx.is_device_forwarding_enabled(device_id) {
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
        match lookup_route_table(core_ctx, bindings_ctx, None, *dst_ip) {
            Some(dst) => {
                core_ctx.increment(|counters| &counters.forward);
                ReceivePacketAction::Forward { dst }
            }
            None => {
                core_ctx.increment(|counters| &counters.no_route_to_host);
                ReceivePacketAction::SendNoRouteToDest
            }
        }
    }
}

// Look up the route to a host.
fn lookup_route_table<
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC>,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    device: Option<&CC::DeviceId>,
    dst_ip: I::Addr,
) -> Option<Destination<I::Addr, CC::DeviceId>> {
    core_ctx.with_ip_routing_table(|core_ctx, table| table.lookup(core_ctx, device, dst_ip))
}

/// The metadata associated with an outgoing IP packet.
#[derive(Debug)]
pub struct SendIpPacketMeta<I: packet_formats::ip::IpExt + IpTypesIpExt, D, Src> {
    /// The outgoing device.
    pub device: D,

    /// The source address of the packet.
    pub src_ip: Src,

    /// The destination address of the packet.
    pub dst_ip: SpecifiedAddr<I::Addr>,

    /// The next-hop node that the packet should be sent to.
    pub next_hop: SpecifiedAddr<I::Addr>,

    /// Whether the destination is a broadcast address.
    pub broadcast: Option<I::BroadcastMarker>,

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
}

impl<I: packet_formats::ip::IpExt + IpTypesIpExt, D>
    From<SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>>
    for SendIpPacketMeta<I, D, Option<SpecifiedAddr<I::Addr>>>
{
    fn from(
        SendIpPacketMeta { device, src_ip, dst_ip, broadcast, next_hop, proto, ttl, mtu }: SendIpPacketMeta<
            I,
            D,
            SpecifiedAddr<I::Addr>,
        >,
    ) -> SendIpPacketMeta<I, D, Option<SpecifiedAddr<I::Addr>>> {
        SendIpPacketMeta {
            device,
            src_ip: Some(src_ip),
            dst_ip,
            broadcast,
            next_hop,
            proto,
            ttl,
            mtu,
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
    ) -> Result<(), S>
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
        next_hop: SpecifiedAddr<I::Addr>,
        body: S,
        broadcast: Option<I::BroadcastMarker>,
    ) -> Result<(), S>
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
    ) -> Result<(), S>
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
        next_hop: SpecifiedAddr<I::Addr>,
        body: S,
        broadcast: Option<I::BroadcastMarker>,
    ) -> Result<(), S>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut,
    {
        send_ip_frame(
            self,
            bindings_ctx,
            device,
            next_hop,
            body,
            broadcast,
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
) -> Result<(), S>
where
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
    CC: IpLayerEgressContext<I, BC> + IpDeviceStateContext<I, BC>,
    S: TransportPacketSerializer<I>,
    S::Buffer: BufferMut,
{
    let SendIpPacketMeta { device, src_ip, dst_ip, broadcast, next_hop, proto, ttl, mtu } = meta;
    let next_packet_id = gen_ip_packet_id(core_ctx);
    let ttl = ttl.unwrap_or_else(|| core_ctx.get_hop_limit(device)).get();
    let src_ip = src_ip.map_or(I::UNSPECIFIED_ADDRESS, |a| a.get());
    assert!(
        (!I::LOOPBACK_SUBNET.contains(&src_ip) && !I::LOOPBACK_SUBNET.contains(&dst_ip))
            || device.is_loopback()
    );
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

    let body = body.encapsulate(builder);

    if let Some(mtu) = mtu {
        let body = NestedWithInnerIpPacket::new(body.with_size_limit(mtu as usize));
        send_ip_frame(core_ctx, bindings_ctx, device, next_hop, body, broadcast, packet_metadata)
            .map_err(|ser| ser.into_inner().into_inner())
    } else {
        send_ip_frame(core_ctx, bindings_ctx, device, next_hop, body, broadcast, packet_metadata)
            .map_err(|ser| ser.into_inner())
    }
}

/// Abstracts access to a [`filter::FilterHandler`] for core contexts.
pub trait FilterHandlerProvider<I: packet_formats::ip::IpExt, BC: FilterBindingsContext> {
    /// The filter handler.
    type Handler<'a>: filter::FilterHandler<I, BC>
    where
        Self: 'a;

    /// Gets the filter handler for this context.
    fn filter_handler(&mut self) -> Self::Handler<'_>;
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    use core::marker::PhantomData;

    use derivative::Derivative;
    use net_types::ip::IpInvariant;
    use netstack3_base::{
        testutil::{FakeCoreCtx, FakeInstant, FakeStrongDeviceId, FakeWeakDeviceId},
        RngContext, SendFrameContext, SendableFrameMeta,
    };

    /// A [`SendIpPacketMeta`] for dual stack contextx.
    #[derive(Debug, GenericOverIp)]
    #[generic_over_ip()]
    #[allow(missing_docs)]
    pub enum DualStackSendIpPacketMeta<D> {
        V4(SendIpPacketMeta<Ipv4, D, SpecifiedAddr<Ipv4Addr>>),
        V6(SendIpPacketMeta<Ipv6, D, SpecifiedAddr<Ipv6Addr>>),
    }

    impl<I: packet_formats::ip::IpExt + IpTypesIpExt, D>
        From<SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>> for DualStackSendIpPacketMeta<D>
    {
        fn from(value: SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>) -> Self {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<I: packet_formats::ip::IpExt + IpTypesIpExt, D>(
                SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>,
            );
            use DualStackSendIpPacketMeta::*;
            let IpInvariant(dual_stack) = I::map_ip(
                Wrap(value),
                |Wrap(value)| IpInvariant(V4(value)),
                |Wrap(value)| IpInvariant(V6(value)),
            );
            dual_stack
        }
    }

    impl<I: packet_formats::ip::IpExt + IpTypesIpExt, S, DeviceId, BC>
        SendableFrameMeta<FakeCoreCtx<S, DualStackSendIpPacketMeta<DeviceId>, DeviceId>, BC>
        for SendIpPacketMeta<I, DeviceId, SpecifiedAddr<I::Addr>>
    {
        fn send_meta<SS>(
            self,
            core_ctx: &mut FakeCoreCtx<S, DualStackSendIpPacketMeta<DeviceId>, DeviceId>,
            bindings_ctx: &mut BC,
            frame: SS,
        ) -> Result<(), SS>
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
        pub fn try_as<I: packet_formats::ip::IpExt + IpTypesIpExt>(
            &self,
        ) -> Result<&SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>, WrongIpVersion> {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: packet_formats::ip::IpExt + IpTypesIpExt, D>(
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

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    pub(crate) struct FakeIpDeviceIdCtx<D>(PhantomData<D>);

    impl<DeviceId: FakeStrongDeviceId> DeviceIdContext<AnyDevice> for FakeIpDeviceIdCtx<DeviceId> {
        type DeviceId = DeviceId;
        type WeakDeviceId = FakeWeakDeviceId<DeviceId>;
    }

    impl<
            I: Ip,
            BC: RngContext + InstantContext<Instant = FakeInstant>,
            D: FakeStrongDeviceId,
            State: MulticastMembershipHandler<I, BC, DeviceId = D>,
            Meta,
        > MulticastMembershipHandler<I, BC> for FakeCoreCtx<State, Meta, D>
    where
        Self: DeviceIdContext<AnyDevice, DeviceId = D>,
    {
        fn join_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            self.state.join_multicast_group(bindings_ctx, device, addr)
        }

        fn leave_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            self.state.leave_multicast_group(bindings_ctx, device, addr)
        }

        fn select_device_for_multicast_group(
            &mut self,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) -> Result<Self::DeviceId, ResolveRouteError> {
            self.state.select_device_for_multicast_group(addr)
        }
    }

    impl<I: packet_formats::ip::IpExt, BC: FilterBindingsContext, S, Meta, DeviceId>
        FilterHandlerProvider<I, BC> for FakeCoreCtx<S, Meta, DeviceId>
    {
        type Handler<'a> = filter::testutil::NoopImpl where Self: 'a;

        fn filter_handler(&mut self) -> Self::Handler<'_> {
            filter::testutil::NoopImpl
        }
    }
}
