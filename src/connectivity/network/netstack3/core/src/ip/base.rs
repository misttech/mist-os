// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{borrow::Cow, vec::Vec};
use core::{
    borrow::Borrow,
    cmp::Ordering,
    fmt::Debug,
    hash::Hash,
    num::{NonZeroU16, NonZeroU32, NonZeroU8},
    sync::atomic::{self, AtomicU16},
};

use const_unwrap::const_unwrap_option;
use derivative::Derivative;
use explicit::ResultExt as _;
use lock_order::lock::UnlockedAccess;
use lock_order::{
    lock::{LockFor, RwLockFor},
    relation::LockBefore,
    wrap::prelude::*,
};
#[cfg(test)]
use net_types::ip::IpVersion;
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr, Mtu, Subnet,
    },
    MulticastAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{Buf, BufferMut, ParseMetadata, Serializer};
use packet_formats::{
    error::IpParseError,
    ip::{IpPacket as _, IpPacketBuilder as _, IpProto, Ipv4Proto, Ipv6Proto},
    ipv4::{Ipv4FragmentType, Ipv4Packet},
    ipv6::Ipv6Packet,
};
use thiserror::Error;
use tracing::{debug, error, trace};

use crate::{
    context::{
        CoreTimerContext, CounterContext, EventContext, HandleableTimer, InstantContext,
        NestedIntoCoreTimerCtx, TimerContext, TimerHandler, TracingContext,
    },
    counters::Counter,
    data_structures::token_bucket::TokenBucket,
    device::{AnyDevice, DeviceId, DeviceIdContext, FrameDestination, Id, StrongId, WeakDeviceId},
    filter::{
        ConntrackConnection, FilterBindingsContext, FilterBindingsTypes, FilterHandler as _,
        FilterHandlerProvider, FilterIpMetadata, FilterTimerId, ForwardedPacket, IngressVerdict,
        IpPacket, MaybeTransportPacket, NestedWithInnerIpPacket,
    },
    inspect::{Inspectable, Inspector},
    ip::{
        device::{
            self, slaac::SlaacCounters, state::IpDeviceStateIpExt, IpDeviceAddr,
            IpDeviceBindingsContext, IpDeviceIpExt, IpDeviceSendContext,
        },
        forwarding::{ForwardingTable, IpForwardingDeviceContext},
        icmp::{
            self,
            socket::{IcmpEchoBindingsTypes, IcmpSocketId, IcmpSocketSet, IcmpSocketState},
            IcmpBindingsTypes, IcmpErrorHandler, IcmpHandlerIpExt, IcmpIpExt,
            IcmpIpTransportContext, Icmpv4Error, Icmpv4ErrorCode, Icmpv4ErrorKind, Icmpv4State,
            Icmpv4StateBuilder, Icmpv6ErrorCode, Icmpv6ErrorKind, Icmpv6State, Icmpv6StateBuilder,
            InnerIcmpContext,
        },
        ipv6,
        ipv6::Ipv6PacketAction,
        path_mtu::{PmtuBindingsTypes, PmtuCache, PmtuTimerId},
        reassembly::{
            FragmentBindingsTypes, FragmentHandler, FragmentProcessingState, FragmentTimerId,
            IpPacketFragmentCache,
        },
        socket::{IpSocketBindingsContext, IpSocketContext, IpSocketHandler},
        types::{
            self, Destination, IpTypesIpExt, NextHop, ResolvedRoute, RoutableIpAddr,
            WrapBroadcastMarker,
        },
    },
    socket::datagram,
    sync::{LockGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
    transport::{tcp::socket::TcpIpTransportContext, udp::UdpIpTransportContext},
    uninstantiable::UninstantiableWrapper,
    BindingsContext, BindingsTypes, CoreCtx, StackState,
};

/// Default IPv4 TTL.
pub(crate) const DEFAULT_TTL: NonZeroU8 = const_unwrap_option(NonZeroU8::new(64));

/// Hop limits for packets sent to multicast and unicast destinations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HopLimits {
    pub(crate) unicast: NonZeroU8,
    pub(crate) multicast: NonZeroU8,
}

/// Default hop limits for sockets.
pub(crate) const DEFAULT_HOP_LIMITS: HopLimits =
    HopLimits { unicast: DEFAULT_TTL, multicast: const_unwrap_option(NonZeroU8::new(1)) };

/// The IPv6 subnet that contains all addresses; `::/0`.
// Safe because 0 is less than the number of IPv6 address bits.
pub(crate) const IPV6_DEFAULT_SUBNET: Subnet<Ipv6Addr> =
    unsafe { Subnet::new_unchecked(Ipv6::UNSPECIFIED_ADDRESS, 0) };

/// An error encountered when receiving a transport-layer packet.
#[derive(Debug)]
pub(crate) struct TransportReceiveError {
    inner: TransportReceiveErrorInner,
}

impl TransportReceiveError {
    // NOTE: We don't expose a constructor for the "protocol unsupported" case.
    // This ensures that the only way that we send a "protocol unsupported"
    // error is if the implementation of `IpTransportContext` provided for a
    // given protocol number is `()`. That's because `()` is the only type whose
    // `receive_ip_packet` function is implemented in this module, and thus it's
    // the only type that is able to construct a "protocol unsupported"
    // `TransportReceiveError`.

    /// Constructs a new `TransportReceiveError` to indicate an unreachable
    /// port.
    pub(crate) fn new_port_unreachable() -> TransportReceiveError {
        TransportReceiveError { inner: TransportReceiveErrorInner::PortUnreachable }
    }
}

#[derive(Debug)]
enum TransportReceiveErrorInner {
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
pub trait IpExt: packet_formats::ip::IpExt + IcmpIpExt + IpTypesIpExt {
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

#[derive(Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub(crate) struct TransparentLocalDelivery<I: IpExt> {
    pub addr: SpecifiedAddr<I::Addr>,
    pub port: NonZeroU16,
}

/// The execution context provided by a transport layer protocol to the IP
/// layer.
///
/// An implementation for `()` is provided which indicates that a particular
/// transport layer protocol is unsupported.
pub(crate) trait IpTransportContext<I: IpExt, BC, CC: DeviceIdContext<AnyDevice> + ?Sized> {
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
        Err((
            buffer,
            TransportReceiveError { inner: TransportReceiveErrorInner::ProtocolUnsupported },
        ))
    }
}

/// The execution context provided by the IP layer to transport layer protocols.
pub trait TransportIpContext<I: IpExt, BC>:
    DeviceIdContext<AnyDevice> + IpSocketHandler<I, BC>
{
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
pub(crate) trait MulticastMembershipHandler<I: Ip, BC>: DeviceIdContext<AnyDevice> {
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

#[derive(Copy, Clone)]
pub enum EitherDeviceId<S, W> {
    Strong(S),
    Weak(W),
}

impl<S: PartialEq, W: PartialEq + PartialEq<S>> PartialEq for EitherDeviceId<S, W> {
    fn eq(&self, other: &EitherDeviceId<S, W>) -> bool {
        match (self, other) {
            (EitherDeviceId::Strong(this), EitherDeviceId::Strong(other)) => this == other,
            (EitherDeviceId::Strong(this), EitherDeviceId::Weak(other)) => other == this,
            (EitherDeviceId::Weak(this), EitherDeviceId::Strong(other)) => this == other,
            (EitherDeviceId::Weak(this), EitherDeviceId::Weak(other)) => this == other,
        }
    }
}

impl<S: Id, W: crate::device::WeakId<Strong = S>> EitherDeviceId<&'_ S, &'_ W> {
    pub(crate) fn as_strong_ref<'a>(&'a self) -> Option<Cow<'a, S>> {
        match self {
            EitherDeviceId::Strong(s) => Some(Cow::Borrowed(s)),
            EitherDeviceId::Weak(w) => w.upgrade().map(Cow::Owned),
        }
    }
}

impl<S, W> EitherDeviceId<S, W> {
    pub(crate) fn as_ref<'a, S2, W2>(&'a self) -> EitherDeviceId<&'a S2, &'a W2>
    where
        S: Borrow<S2>,
        W: Borrow<W2>,
    {
        match self {
            EitherDeviceId::Strong(s) => EitherDeviceId::Strong(s.borrow()),
            EitherDeviceId::Weak(w) => EitherDeviceId::Weak(w.borrow()),
        }
    }
}

impl<S: crate::device::StrongId<Weak = W>, W: crate::device::WeakId<Strong = S>>
    EitherDeviceId<S, W>
{
    pub(crate) fn as_strong<'a>(&'a self) -> Option<Cow<'a, S>> {
        match self {
            EitherDeviceId::Strong(s) => Some(Cow::Borrowed(s)),
            EitherDeviceId::Weak(w) => w.upgrade().map(Cow::Owned),
        }
    }

    pub(crate) fn as_weak<'a>(&'a self) -> Cow<'a, W> {
        match self {
            EitherDeviceId::Strong(s) => Cow::Owned(s.downgrade()),
            EitherDeviceId::Weak(w) => Cow::Borrowed(w),
        }
    }
}

/// The status of an IP address on an interface.
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
pub enum Ipv4PresentAddressStatus {
    LimitedBroadcast,
    SubnetBroadcast,
    Multicast,
    Unicast,
}

/// The status of an IPv6 address.
pub enum Ipv6PresentAddressStatus {
    Multicast,
    UnicastAssigned,
    UnicastTentative,
}

/// An extension trait providing IP layer properties.
pub trait IpLayerIpExt: IpExt {
    type AddressStatus;
    type State<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes>: AsRef<
        IpStateInner<Self, StrongDeviceId, BT>,
    >;
    type PacketIdState;
    type PacketId;
    type RxCounters: Default + Inspectable;
    fn next_packet_id_from_state(state: &Self::PacketIdState) -> Self::PacketId;
}

impl IpLayerIpExt for Ipv4 {
    type AddressStatus = Ipv4PresentAddressStatus;
    type State<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes> = Ipv4State<StrongDeviceId, BT>;
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
    type State<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes> = Ipv6State<StrongDeviceId, BT>;
    type PacketIdState = ();
    type PacketId = ();
    type RxCounters = Ipv6RxCounters;
    fn next_packet_id_from_state((): &Self::PacketIdState) -> Self::PacketId {
        ()
    }
}

/// The state context provided to the IP layer.
pub trait IpStateContext<I: IpLayerIpExt, BC>: DeviceIdContext<AnyDevice> {
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
pub trait IpLayerBindingsTypes:
    FilterBindingsTypes + IcmpBindingsTypes + IpStateBindingsTypes
{
}
impl<BT: FilterBindingsTypes + IcmpBindingsTypes + IpStateBindingsTypes> IpLayerBindingsTypes
    for BT
{
}

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
            Ipv4PresentAddressStatus::Unicast => true,
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

// Returns the forwarding instructions for reaching the given destination.
//
// If a `device` is specified, the resolved route is limited to those that
// egress over the device.
//
// If `local_ip` is specified the resolved route is limited to those that egress
// over a device with the address assigned.
pub(crate) fn resolve_route_to_destination<
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
                            if local_ip.is_some_and(|local_ip| {
                                crate::socket::must_have_zone(local_ip.as_ref())
                            }) || crate::socket::must_have_zone(addr.as_ref())
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
        S: Serializer + MaybeTransportPacket,
        S::Buffer: BufferMut,
    {
        send_ip_packet_from_device(self, bindings_ctx, meta.into(), body, packet_metadata)
    }
}

impl<BC: BindingsContext, I: IpLayerIpExt, L> CounterContext<IpCounters<I>> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&IpCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::IpStateCounters<I>>())
    }
}

impl<I, BC, L> IpStateContext<I, BC> for CoreCtx<'_, BC, L>
where
    I: IpLayerIpExt,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::IpStateRoutingTable<I>>,

    // These bounds ensure that we can fulfill all the traits for the associated
    // type `IpDeviceIdCtx` below and keep the compiler happy where we don't
    // have implementations that are generic on Ip.
    for<'a> CoreCtx<'a, BC, crate::lock_ordering::IpStateRoutingTable<I>>:
        DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
            + IpForwardingDeviceContext<I>
            + IpDeviceStateContext<I, BC>,
{
    type IpDeviceIdCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IpStateRoutingTable<I>>;

    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (cache, mut locked) =
            self.read_lock_and::<crate::lock_ordering::IpStateRoutingTable<I>>();
        cb(&mut locked, &cache)
    }

    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut cache, mut locked) =
            self.write_lock_and::<crate::lock_ordering::IpStateRoutingTable<I>>();
        cb(&mut locked, &mut cache)
    }
}

/// The IP context providing dispatch to the available transport protocols.
///
/// This trait acts like a demux on the transport protocol for ingress IP
/// packets.
pub(crate) trait IpTransportDispatchContext<I: IpLayerIpExt, BC>:
    DeviceIdContext<AnyDevice>
{
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
    ) -> Result<(), (B, TransportReceiveError)>;
}

/// A marker trait for all the contexts required for IP ingress.
pub(crate) trait IpLayerIngressContext<
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
{
    // This is working around the fact that currently, where clauses are only
    // elaborated for supertraits, and not, for example, bounds on associated types
    // as we have here.
    //
    // See https://github.com/rust-lang/rust/issues/20671#issuecomment-1905186183
    // for more discussion.
    type DeviceId_: crate::filter::InterfaceProperties<BC::DeviceClass> + Debug;
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
            + FilterHandlerProvider<I, BC>,
    > IpLayerIngressContext<I, BC> for CC
where
    Self::DeviceId: crate::filter::InterfaceProperties<BC::DeviceClass>,
{
    type DeviceId_ = Self::DeviceId;
}

/// A marker trait for all the contexts required for IP egress.
pub(crate) trait IpLayerEgressContext<I, BC>:
    IpDeviceSendContext<I, BC, DeviceId = Self::DeviceId_> + FilterHandlerProvider<I, BC>
where
    I: IpLayerIpExt,
    BC: FilterBindingsTypes,
{
    // This is working around the fact that currently, where clauses are only
    // elaborated for supertraits, and not, for example, bounds on associated types
    // as we have here.
    //
    // See https://github.com/rust-lang/rust/issues/20671#issuecomment-1905186183
    // for more discussion.
    type DeviceId_: crate::filter::InterfaceProperties<BC::DeviceClass> + StrongId + Debug;
}

impl<I, BC, CC> IpLayerEgressContext<I, BC> for CC
where
    I: IpLayerIpExt,
    BC: FilterBindingsTypes,
    CC: IpDeviceSendContext<I, BC> + FilterHandlerProvider<I, BC>,
    Self::DeviceId: crate::filter::InterfaceProperties<BC::DeviceClass>,
{
    type DeviceId_ = Self::DeviceId;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<Ipv4>>>
    IpTransportDispatchContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv4Addr,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        proto: Ipv4Proto,
        body: B,
        transport_override: Option<TransparentLocalDelivery<Ipv4>>,
    ) -> Result<(), (B, TransportReceiveError)> {
        // TODO(https://fxbug.dev/42175797): Deliver the packet to interested raw
        // sockets.

        match proto {
            Ipv4Proto::Icmp => {
                <IcmpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    transport_override,
                )
            }
            Ipv4Proto::Igmp => {
                device::receive_igmp_packet(self, bindings_ctx, device, src_ip, dst_ip, body);
                Ok(())
            }
            Ipv4Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    transport_override,
                )
            }
            Ipv4Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    transport_override,
                )
            }
            // TODO(joshlf): Once all IP protocol numbers are covered, remove
            // this default case.
            _ => Err((
                body,
                TransportReceiveError { inner: TransportReceiveErrorInner::ProtocolUnsupported },
            )),
        }
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<Ipv6>>>
    IpTransportDispatchContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        proto: Ipv6Proto,
        body: B,
        transport_override: Option<TransparentLocalDelivery<Ipv6>>,
    ) -> Result<(), (B, TransportReceiveError)> {
        // TODO(https://fxbug.dev/42175797): Deliver the packet to interested raw
        // sockets.

        match proto {
            Ipv6Proto::Icmpv6 => {
                <IcmpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    transport_override,
                )
            }
            // A value of `Ipv6Proto::NoNextHeader` tells us that there is no
            // header whatsoever following the last lower-level header so we stop
            // processing here.
            Ipv6Proto::NoNextHeader => Ok(()),
            Ipv6Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    transport_override,
                )
            }
            Ipv6Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                    transport_override,
                )
            }
            // TODO(joshlf): Once all IP Next Header numbers are covered, remove
            // this default case.
            _ => Err((
                body,
                TransportReceiveError { inner: TransportReceiveErrorInner::ProtocolUnsupported },
            )),
        }
    }
}

/// A builder for IPv4 state.
#[derive(Copy, Clone, Default)]
pub(crate) struct Ipv4StateBuilder {
    icmp: Icmpv4StateBuilder,
}

impl Ipv4StateBuilder {
    /// Get the builder for the ICMPv4 state.
    #[cfg(test)]
    pub(crate) fn icmpv4_builder(&mut self) -> &mut Icmpv4StateBuilder {
        &mut self.icmp
    }

    pub(crate) fn build<
        CC: CoreTimerContext<IpLayerTimerId, BC>,
        StrongDeviceId: StrongId,
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
            filter: RwLock::new(crate::filter::State::new::<NestedIntoCoreTimerCtx<CC, _>>(
                bindings_ctx,
            )),
        }
    }
}

/// A builder for IPv6 state.
#[derive(Copy, Clone, Default)]
pub(crate) struct Ipv6StateBuilder {
    icmp: Icmpv6StateBuilder,
}

impl Ipv6StateBuilder {
    pub(crate) fn build<
        CC: CoreTimerContext<IpLayerTimerId, BC>,
        StrongDeviceId: StrongId,
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
            filter: RwLock::new(crate::filter::State::new::<NestedIntoCoreTimerCtx<CC, _>>(
                bindings_ctx,
            )),
        }
    }
}

pub struct Ipv4State<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes> {
    pub(super) inner: IpStateInner<Ipv4, StrongDeviceId, BT>,
    pub(super) icmp: Icmpv4State<StrongDeviceId::Weak, BT>,
    pub(super) next_packet_id: AtomicU16,
    pub(super) filter: RwLock<crate::filter::State<Ipv4, BT>>,
}

impl<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes> Ipv4State<StrongDeviceId, BT> {
    pub fn filter(&self) -> &RwLock<crate::filter::State<Ipv4, BT>> {
        &self.filter
    }

    pub(crate) fn inner(&self) -> &IpStateInner<Ipv4, StrongDeviceId, BT> {
        &self.inner
    }

    pub(crate) fn icmp(&self) -> &Icmpv4State<StrongDeviceId::Weak, BT> {
        &self.icmp
    }
}

impl<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes>
    AsRef<IpStateInner<Ipv4, StrongDeviceId, BT>> for Ipv4State<StrongDeviceId, BT>
{
    fn as_ref(&self) -> &IpStateInner<Ipv4, StrongDeviceId, BT> {
        &self.inner
    }
}

pub(super) fn gen_ip_packet_id<I: IpLayerIpExt, BC, CC: IpDeviceStateContext<I, BC>>(
    core_ctx: &mut CC,
) -> I::PacketId {
    core_ctx.with_next_packet_id(|state| I::next_packet_id_from_state(state))
}

pub struct Ipv6State<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes> {
    pub(super) inner: IpStateInner<Ipv6, StrongDeviceId, BT>,
    pub(super) icmp: Icmpv6State<StrongDeviceId::Weak, BT>,
    pub(super) slaac_counters: SlaacCounters,
    pub(super) filter: RwLock<crate::filter::State<Ipv6, BT>>,
}

impl<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes> Ipv6State<StrongDeviceId, BT> {
    pub(crate) fn slaac_counters(&self) -> &SlaacCounters {
        &self.slaac_counters
    }

    pub fn filter(&self) -> &RwLock<crate::filter::State<Ipv6, BT>> {
        &self.filter
    }

    pub(crate) fn inner(&self) -> &IpStateInner<Ipv6, StrongDeviceId, BT> {
        &self.inner
    }

    pub(crate) fn icmp(&self) -> &Icmpv6State<StrongDeviceId::Weak, BT> {
        &self.icmp
    }
}

impl<StrongDeviceId: StrongId, BT: IpLayerBindingsTypes>
    AsRef<IpStateInner<Ipv6, StrongDeviceId, BT>> for Ipv6State<StrongDeviceId, BT>
{
    fn as_ref(&self) -> &IpStateInner<Ipv6, StrongDeviceId, BT> {
        &self.inner
    }
}

impl<I, BT> LockFor<crate::lock_ordering::IpStateFragmentCache<I>> for StackState<BT>
where
    I: IpLayerIpExt,
    BT: BindingsTypes,
{
    type Data = IpPacketFragmentCache<I, BT>;
    type Guard<'l> = LockGuard<'l, IpPacketFragmentCache<I, BT>> where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        self.inner_ip_state().fragment_cache.lock()
    }
}

impl<I, BT> LockFor<crate::lock_ordering::IpStatePmtuCache<I>> for StackState<BT>
where
    I: IpLayerIpExt,
    BT: BindingsTypes,
{
    type Data = PmtuCache<I, BT>;
    type Guard<'l> = LockGuard<'l, PmtuCache<I, BT>> where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        self.inner_ip_state().pmtu_cache.lock()
    }
}

impl<I: IpLayerIpExt, BT: BindingsTypes> RwLockFor<crate::lock_ordering::IpStateRoutingTable<I>>
    for StackState<BT>
{
    type Data = ForwardingTable<I, DeviceId<BT>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, ForwardingTable<I, DeviceId<BT>>>
        where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, ForwardingTable<I, DeviceId<BT>>>
        where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.inner_ip_state().table.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.inner_ip_state().table.write()
    }
}

impl<I, BT> RwLockFor<crate::lock_ordering::IcmpBoundMap<I>> for StackState<BT>
where
    I: datagram::DualStackIpExt,
    BT: BindingsTypes,
{
    type Data = icmp::socket::BoundSockets<I, WeakDeviceId<BT>, BT>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.inner_icmp_state().sockets.bound_and_id_allocator.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.inner_icmp_state().sockets.bound_and_id_allocator.write()
    }
}

impl<I: datagram::DualStackIpExt, D: crate::device::WeakId, BT: IcmpEchoBindingsTypes>
    RwLockFor<crate::lock_ordering::IcmpSocketState<I>> for IcmpSocketId<I, D, BT>
{
    type Data = IcmpSocketState<I, D, BT>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data>
    where
        Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data>
    where
        Self: 'l ;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state_for_locking().read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state_for_locking().write()
    }
}

impl<I, BT> RwLockFor<crate::lock_ordering::IcmpAllSocketsSet<I>> for StackState<BT>
where
    I: datagram::DualStackIpExt,
    BT: BindingsTypes,
{
    type Data = IcmpSocketSet<I, WeakDeviceId<BT>, BT>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.inner_icmp_state().sockets.all_sockets.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.inner_icmp_state().sockets.all_sockets.write()
    }
}

impl<I, BT> LockFor<crate::lock_ordering::IcmpTokenBucket<I>> for StackState<BT>
where
    I: datagram::DualStackIpExt,
    BT: BindingsTypes,
{
    type Data = TokenBucket<BT::Instant>;
    type Guard<'l> = LockGuard<'l, TokenBucket<BT::Instant>>
        where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        self.inner_icmp_state::<I>().error_send_bucket.lock()
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::Ipv4StateNextPacketId>
    for StackState<BC>
{
    type Data = AtomicU16;
    type Guard<'l> = &'l AtomicU16 where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv4.next_packet_id
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

impl<BC: BindingsContext, I: IpLayerIpExt> UnlockedAccess<crate::lock_ordering::IpStateCounters<I>>
    for StackState<BC>
{
    type Data = IpCounters<I>;
    type Guard<'l> = &'l IpCounters<I> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ip_counters()
    }
}

/// Marker trait for the bindings types required by the IP layer's inner state.
pub trait IpStateBindingsTypes: PmtuBindingsTypes + FragmentBindingsTypes {}
impl<BT> IpStateBindingsTypes for BT where BT: PmtuBindingsTypes + FragmentBindingsTypes {}

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct IpStateInner<I: IpLayerIpExt, DeviceId, BT: IpStateBindingsTypes> {
    table: RwLock<ForwardingTable<I, DeviceId>>,
    fragment_cache: Mutex<IpPacketFragmentCache<I, BT>>,
    pmtu_cache: Mutex<PmtuCache<I, BT>>,
    counters: IpCounters<I>,
}

impl<I: IpLayerIpExt, DeviceId, BT: IpStateBindingsTypes> IpStateInner<I, DeviceId, BT> {
    pub(crate) fn counters(&self) -> &IpCounters<I> {
        &self.counters
    }
}

impl<I: IpLayerIpExt, DeviceId, BC: TimerContext + IpStateBindingsTypes>
    IpStateInner<I, DeviceId, BC>
{
    pub fn new<CC: CoreTimerContext<IpLayerTimerId, BC>>(bindings_ctx: &mut BC) -> Self {
        Self {
            table: Default::default(),
            fragment_cache: Mutex::new(
                IpPacketFragmentCache::new::<NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx),
            ),
            pmtu_cache: Mutex::new(PmtuCache::new::<NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx)),
            counters: Default::default(),
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
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv4, BC> + CounterContext<IpCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: Option<FrameDestination>,
    src_ip: Ipv4Addr,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    proto: Ipv4Proto,
    body: B,
    parse_metadata: Option<ParseMetadata>,
    mut packet_metadata: IpLayerPacketMetadata<Ipv4, BC>,
    transport_override: Option<TransparentLocalDelivery<Ipv4>>,
) {
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

    match core_ctx.filter_handler().local_ingress_hook(
        &mut crate::filter::RxPacket::new(src_ip, dst_ip.get(), proto, &body),
        device,
        &mut packet_metadata,
    ) {
        crate::filter::Verdict::Drop => {
            packet_metadata.acknowledge_drop();
            return;
        }
        crate::filter::Verdict::Accept => {}
    }
    packet_metadata.acknowledge_drop();

    let (mut body, err) = match core_ctx.dispatch_receive_ip_packet(
        bindings_ctx,
        device,
        src_ip,
        dst_ip,
        proto,
        body,
        transport_override,
    ) {
        Ok(()) => return,
        Err(e) => e,
    };
    // All branches promise to return the buffer in the same state it was in
    // when they were executed. Thus, all we have to do is undo the parsing
    // of the IP packet header, and the buffer will be back to containing
    // the entire original IP packet.
    let meta = parse_metadata.unwrap();
    body.undo_parse(meta);

    if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
        match err.inner {
            TransportReceiveErrorInner::ProtocolUnsupported => {
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    body,
                    Icmpv4Error {
                        kind: Icmpv4ErrorKind::ProtocolUnreachable,
                        header_len: meta.header_len(),
                    },
                );
            }
            TransportReceiveErrorInner::PortUnreachable => {
                // TODO(joshlf): What if we're called from a loopback
                // handler, and device and parse_metadata are None? In other
                // words, what happens if we attempt to send to a loopback
                // port which is unreachable? We will eventually need to
                // restructure the control flow here to handle that case.
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    body,
                    Icmpv4Error {
                        kind: Icmpv4ErrorKind::PortUnreachable,
                        header_len: meta.header_len(),
                    },
                );
            }
        }
    } else {
        trace!("dispatch_receive_ipv4_packet: Cannot send ICMP error message in response to a packet from the unspecified address");
    }
}

/// Dispatch a received IPv6 packet to the appropriate protocol.
///
/// `dispatch_receive_ipv6_packet` has the same semantics as
/// `dispatch_receive_ipv4_packet`, but for IPv6.
fn dispatch_receive_ipv6_packet<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv6, BC> + CounterContext<IpCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: Option<FrameDestination>,
    src_ip: Ipv6SourceAddr,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    body: B,
    parse_metadata: Option<ParseMetadata>,
    mut packet_metadata: IpLayerPacketMetadata<Ipv6, BC>,
    transport_override: Option<TransparentLocalDelivery<Ipv6>>,
) {
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

    match core_ctx.filter_handler().local_ingress_hook(
        &mut crate::filter::RxPacket::new(src_ip.get(), dst_ip.get(), proto, &body),
        device,
        &mut packet_metadata,
    ) {
        crate::filter::Verdict::Drop => {
            packet_metadata.acknowledge_drop();
            return;
        }
        crate::filter::Verdict::Accept => {}
    }

    let (mut body, err) = match core_ctx.dispatch_receive_ip_packet(
        bindings_ctx,
        device,
        src_ip,
        dst_ip,
        proto,
        body,
        transport_override,
    ) {
        Ok(()) => {
            packet_metadata.acknowledge_drop();
            return;
        }
        Err(e) => e,
    };

    // All branches promise to return the buffer in the same state it was in
    // when they were executed. Thus, all we have to do is undo the parsing
    // of the IP packet header, and the buffer will be back to containing
    // the entire original IP packet.
    let meta = parse_metadata.unwrap();
    body.undo_parse(meta);

    match err.inner {
        TransportReceiveErrorInner::ProtocolUnsupported => {
            if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    *src_ip,
                    dst_ip,
                    body,
                    Icmpv6ErrorKind::ProtocolUnreachable { header_len: meta.header_len() },
                );
            }
        }
        TransportReceiveErrorInner::PortUnreachable => {
            // TODO(joshlf): What if we're called from a loopback handler,
            // and device and parse_metadata are None? In other words, what
            // happens if we attempt to send to a loopback port which is
            // unreachable? We will eventually need to restructure the
            // control flow here to handle that case.
            if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    *src_ip,
                    dst_ip,
                    body,
                    Icmpv6ErrorKind::PortUnreachable,
                );
            }
        }
    }

    packet_metadata.acknowledge_drop();
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
    BC: FilterBindingsTypes,
    CC: IpLayerEgressContext<I, BC>,
    S: Serializer + IpPacket<I>,
    S::Buffer: BufferMut,
{
    let (verdict, proof) =
        core_ctx.filter_handler().egress_hook(&mut body, device, &mut packet_metadata);
    match verdict {
        crate::filter::Verdict::Drop => {
            packet_metadata.acknowledge_drop();
            return Ok(());
        }
        crate::filter::Verdict::Accept => {}
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
    ($core_ctx:expr, $bindings_ctx:expr, $dispatch:ident, $device:ident, $frame_dst:expr, $buffer:expr, $packet:expr, $src_ip:expr, $dst_ip:expr, $ip:ident, $packet_metadata:expr) => {{
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
                let (_, _, proto, meta) = packet.into_metadata();
                $dispatch(
                    $core_ctx,
                    $bindings_ctx,
                    $device,
                    $frame_dst,
                    $src_ip,
                    $dst_ip,
                    proto,
                    $buffer,
                    Some(meta),
                    $packet_metadata,
                    None,
                );
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
                        let (_, _, proto, meta) = packet.into_metadata();
                        // Since each fragment had its own packet metadata, it's
                        // not clear what metadata to use for the reassembled
                        // packet. Resetting the metadata is the safest bet,
                        // though it means downstream consumers must be aware of
                        // this case.
                        let packet_metadata = IpLayerPacketMetadata::default();
                        $dispatch::<_, Buf<Vec<u8>>, _,>(
                            $core_ctx,
                            $bindings_ctx,
                            $device,
                            $frame_dst,
                            $src_ip,
                            $dst_ip,
                            proto,
                            buffer,
                            Some(meta),
                            packet_metadata,
                            None,
                        );
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
pub(crate) fn receive_ipv4_packet<
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
    match filter.ingress_hook(&mut packet, device, &mut packet_metadata) {
        IngressVerdict::Verdict(crate::filter::Verdict::Accept) => {}
        IngressVerdict::Verdict(crate::filter::Verdict::Drop) => {
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
            let src_ip = packet.src_ip();
            let (_, _, proto, meta) = packet.into_metadata();
            dispatch_receive_ipv4_packet(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                buffer,
                Some(meta),
                packet_metadata,
                Some(TransparentLocalDelivery { addr, port }),
            );
            return;
        }
    }
    // Drop the filter handler since it holds a mutable borrow of `core_ctx`, which
    // we need below.
    drop(filter);

    match receive_ipv4_packet_action(core_ctx, bindings_ctx, device, dst_ip) {
        ReceivePacketAction::Deliver => {
            trace!("receive_ipv4_packet: delivering locally");
            let src_ip = packet.src_ip();

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
                src_ip,
                dst_ip,
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
                    crate::filter::Verdict::Drop => {
                        packet_metadata.acknowledge_drop();
                        return;
                    }
                    crate::filter::Verdict::Accept => {}
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
pub(crate) fn receive_ipv6_packet<
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
    match filter.ingress_hook(&mut packet, device, &mut packet_metadata) {
        IngressVerdict::Verdict(crate::filter::Verdict::Accept) => {}
        IngressVerdict::Verdict(crate::filter::Verdict::Drop) => {
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
            let (_, _, proto, meta) = packet.into_metadata();
            dispatch_receive_ipv6_packet(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                buffer,
                Some(meta),
                packet_metadata,
                Some(TransparentLocalDelivery { addr, port }),
            );
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
                    let (_, _, proto, meta): (Ipv6Addr, Ipv6Addr, _, _) = packet.into_metadata();
                    dispatch_receive_ipv6_packet(
                        core_ctx,
                        bindings_ctx,
                        device,
                        frame_dst,
                        src_ip,
                        dst_ip,
                        proto,
                        buffer,
                        Some(meta),
                        packet_metadata,
                        None,
                    );
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
                        src_ip,
                        dst_ip,
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
                    crate::filter::Verdict::Drop => {
                        packet_metadata.acknowledge_drop();
                        return;
                    }
                    crate::filter::Verdict::Accept => {}
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
#[cfg_attr(test, derive(Debug, PartialEq))]
enum ReceivePacketAction<A: IpAddress, DeviceId>
where
    A::Version: IpTypesIpExt,
{
    /// Deliver the packet locally.
    Deliver,
    /// Forward the packet to the given destination.
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
    Drop { reason: DropReason },
}

#[cfg_attr(test, derive(PartialEq))]
#[derive(Debug)]
enum DropReason {
    /// Remote packet destined to tentative address.
    Tentative,
    /// Packet should be forwarded but packet's inbound interface has forwarding
    /// disabled.
    ForwardingDisabledInboundIface,
}

/// Computes the action to take in order to process a received IPv4 packet.
fn receive_ipv4_packet_action<
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
            | Ipv4PresentAddressStatus::Unicast,
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
fn receive_ipv6_packet_action<
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
#[cfg_attr(test, derive(Debug))]
pub struct SendIpPacketMeta<I: packet_formats::ip::IpExt + IpTypesIpExt, D, Src> {
    /// The outgoing device.
    pub(crate) device: D,

    /// The source address of the packet.
    pub(crate) src_ip: Src,

    /// The destination address of the packet.
    pub(crate) dst_ip: SpecifiedAddr<I::Addr>,

    /// The next-hop node that the packet should be sent to.
    pub(crate) next_hop: SpecifiedAddr<I::Addr>,

    /// Whether the destination is a broadcast address.
    pub(crate) broadcast: Option<I::BroadcastMarker>,

    /// The upper-layer protocol held in the packet's payload.
    pub(crate) proto: I::Proto,

    /// The time-to-live (IPv4) or hop limit (IPv6) for the packet.
    ///
    /// If not set, a default TTL may be used.
    pub(crate) ttl: Option<NonZeroU8>,

    /// An MTU to artificially impose on the whole IP packet.
    ///
    /// Note that the device's MTU will still be imposed on the packet.
    pub(crate) mtu: Option<u32>,
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
pub(crate) trait IpLayerHandler<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Encapsulate and send the provided transport packet and from the device
    /// provided in `meta`.
    fn send_ip_packet_from_device<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<I, &Self::DeviceId, Option<SpecifiedAddr<I::Addr>>>,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer + MaybeTransportPacket,
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
        S: Serializer + MaybeTransportPacket,
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
    S: Serializer + MaybeTransportPacket,
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

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv4>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>
            + LockBefore<crate::lock_ordering::UdpAllSocketsSet<Ipv4>>,
    > InnerIcmpContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpBoundMap<Ipv4>>;
    type DualStackContext = UninstantiableWrapper<Self>;

    fn receive_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        original_src_ip: Option<SpecifiedAddr<Ipv4Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv4Addr>,
        original_proto: Ipv4Proto,
        original_body: &[u8],
        err: Icmpv4ErrorCode,
    ) {
        self.increment(|counters: &IpCounters<Ipv4>| &counters.receive_icmp_error);
        trace!("InnerIcmpContext<Ipv4>::receive_icmp_error({:?})", err);

        match original_proto {
            Ipv4Proto::Icmp => {
                <IcmpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv4Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv4Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            // TODO(joshlf): Once all IP protocol numbers are covered,
            // remove this default case.
            _ => <() as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                self,
                bindings_ctx,
                device,
                original_src_ip,
                original_dst_ip,
                original_body,
                err,
            ),
        }
    }

    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut icmp::socket::BoundSockets<Ipv4, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpBoundMap<Ipv4>>();
        cb(&mut core_ctx, &mut sockets)
    }

    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.lock::<crate::lock_ordering::IcmpTokenBucket<Ipv4>>())
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv6>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv6>>
            + LockBefore<crate::lock_ordering::UdpAllSocketsSet<Ipv6>>,
    > InnerIcmpContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpBoundMap<Ipv6>>;
    type DualStackContext = UninstantiableWrapper<Self>;
    fn receive_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        original_src_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv6Addr>,
        original_next_header: Ipv6Proto,
        original_body: &[u8],
        err: Icmpv6ErrorCode,
    ) {
        self.increment(|counters: &IpCounters<Ipv6>| &counters.receive_icmp_error);
        trace!("InnerIcmpContext<Ipv6>::receive_icmp_error({:?})", err);

        match original_next_header {
            Ipv6Proto::Icmpv6 => {
                <IcmpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv6Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv6Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            // TODO(joshlf): Once all IP protocol numbers are covered,
            // remove this default case.
            _ => <() as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                self,
                bindings_ctx,
                device,
                original_src_ip,
                original_dst_ip,
                original_body,
                err,
            ),
        }
    }

    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut icmp::socket::BoundSockets<Ipv6, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpBoundMap<Ipv6>>();
        cb(&mut core_ctx, &mut sockets)
    }

    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.lock::<crate::lock_ordering::IcmpTokenBucket<Ipv6>>())
    }
}

impl<L, BC: BindingsContext> icmp::IcmpStateContext for CoreCtx<'_, BC, L> {}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I,
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<I>>
            + LockBefore<crate::lock_ordering::TcpDemux<I>>
            + LockBefore<crate::lock_ordering::UdpBoundMap<I>>,
    > icmp::socket::StateContext<I, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpSocketState<I>>;

    fn with_all_sockets_mut<O, F: FnOnce(&mut IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.write_lock::<crate::lock_ordering::IcmpAllSocketsSet<I>>())
    }

    fn with_all_sockets<O, F: FnOnce(&IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&self.read_lock::<crate::lock_ordering::IcmpAllSocketsSet<I>>())
    }

    fn with_socket_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &IcmpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id);
        let (socket_state, mut restricted) =
            locked.read_lock_with_and::<crate::lock_ordering::IcmpSocketState<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &socket_state)
    }

    fn with_socket_state_mut<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut IcmpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(id);
        let (mut socket_state, mut restricted) = locked
            .write_lock_with_and::<crate::lock_ordering::IcmpSocketState<I>, _>(|c| c.right());
        let mut restricted = restricted.cast_core_ctx();
        cb(&mut restricted, &mut socket_state)
    }

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked())
    }

    fn for_each_socket<
        F: FnMut(
            &mut Self::SocketStateCtx<'_>,
            &IcmpSocketId<I, Self::WeakDeviceId, BC>,
            &IcmpSocketState<I, Self::WeakDeviceId, BC>,
        ),
    >(
        &mut self,
        mut cb: F,
    ) {
        let (all_sockets, mut locked) =
            self.read_lock_and::<crate::lock_ordering::IcmpAllSocketsSet<I>>();
        all_sockets.keys().for_each(|id| {
            let id = IcmpSocketId::from(id.clone());
            let mut locked = locked.adopt(&id);
            let (socket_state, mut restricted) = locked
                .read_lock_with_and::<crate::lock_ordering::IcmpSocketState<I>, _>(|c| c.right());
            let mut restricted = restricted.cast_core_ctx();
            cb(&mut restricted, &id, &socket_state);
        });
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use core::marker::PhantomData;

    use derivative::Derivative;
    use net_types::ip::IpInvariant;

    use crate::{
        context::{testutil::FakeInstant, RngContext},
        device::testutil::{FakeStrongDeviceId, FakeWeakDeviceId},
    };

    impl<S, Meta, D: StrongId + 'static> DeviceIdContext<AnyDevice>
        for crate::context::testutil::FakeCoreCtx<S, Meta, D>
    where
        FakeIpDeviceIdCtx<D>: DeviceIdContext<AnyDevice, DeviceId = D, WeakDeviceId = D::Weak>,
    {
        type DeviceId = D;
        type WeakDeviceId = D::Weak;
    }

    impl<Outer, Inner: DeviceIdContext<AnyDevice>> DeviceIdContext<AnyDevice>
        for crate::context::testutil::Wrapped<Outer, Inner>
    {
        type DeviceId = Inner::DeviceId;
        type WeakDeviceId = Inner::WeakDeviceId;
    }

    #[derive(Debug, GenericOverIp)]
    #[generic_over_ip()]
    pub(crate) enum DualStackSendIpPacketMeta<D> {
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

    #[cfg(test)]
    impl<I: packet_formats::ip::IpExt + IpTypesIpExt, S, DeviceId, BC>
        crate::context::SendableFrameMeta<
            crate::context::testutil::FakeCoreCtx<S, DualStackSendIpPacketMeta<DeviceId>, DeviceId>,
            BC,
        > for SendIpPacketMeta<I, DeviceId, SpecifiedAddr<I::Addr>>
    {
        fn send_meta<SS>(
            self,
            core_ctx: &mut crate::context::testutil::FakeCoreCtx<
                S,
                DualStackSendIpPacketMeta<DeviceId>,
                DeviceId,
            >,
            bindings_ctx: &mut BC,
            frame: SS,
        ) -> Result<(), SS>
        where
            SS: Serializer,
            SS::Buffer: BufferMut,
        {
            crate::context::SendFrameContext::send_frame(
                &mut core_ctx.frames,
                bindings_ctx,
                DualStackSendIpPacketMeta::from(self),
                frame,
            )
        }
    }

    #[derive(Debug)]
    pub(crate) struct WrongIpVersion;

    impl<D> DualStackSendIpPacketMeta<D> {
        pub(crate) fn try_as<I: packet_formats::ip::IpExt + IpTypesIpExt>(
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
        > MulticastMembershipHandler<I, BC>
        for crate::context::testutil::FakeCoreCtx<State, Meta, D>
    where
        Self: DeviceIdContext<AnyDevice, DeviceId = D>,
    {
        fn join_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            self.get_mut().join_multicast_group(bindings_ctx, device, addr)
        }

        fn leave_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            self.get_mut().leave_multicast_group(bindings_ctx, device, addr)
        }

        fn select_device_for_multicast_group(
            &mut self,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) -> Result<Self::DeviceId, ResolveRouteError> {
            self.get_mut().select_device_for_multicast_group(addr)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use assert_matches::assert_matches;
    use core::time::Duration;

    use ip_test_macro::ip_test;
    use net_types::{
        ethernet::Mac,
        ip::{AddrSubnet, IpAddr},
    };
    use packet::ParseBuffer;
    use packet_formats::{
        ethernet::{
            EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        },
        icmp::{
            IcmpDestUnreachable, IcmpEchoRequest, IcmpPacketBuilder, IcmpParseArgs, IcmpUnusedCode,
            Icmpv4DestUnreachableCode, Icmpv6Packet, Icmpv6PacketTooBig,
            Icmpv6ParameterProblemCode, MessageBody,
        },
        ip::{IpPacketBuilder, Ipv6ExtHdrType},
        ipv4::Ipv4PacketBuilder,
        ipv6::{ext_hdrs::ExtensionHeaderOptionAction, Ipv6PacketBuilder},
        testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame,
    };
    use rand::Rng;
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::FakeInstant,
        device::{
            ethernet::{EthernetCreationProperties, EthernetLinkDevice, RecvEthernetFrameMeta},
            loopback::{LoopbackCreationProperties, LoopbackDevice},
            testutil::set_forwarding_enabled,
        },
        ip::{
            device::{
                config::{
                    IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
                    Ipv6DeviceConfigurationUpdate,
                },
                slaac::SlaacConfiguration,
            },
            types::{AddableEntryEither, AddableMetric, RawMetric},
        },
        testutil::{
            new_rng, set_logger_for_test, Ctx, FakeBindingsCtx, FakeCtx,
            FakeEventDispatcherBuilder, TestIpExt, DEFAULT_INTERFACE_METRIC, FAKE_CONFIG_V4,
            FAKE_CONFIG_V6, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        UnlockedCoreCtx,
    };

    // Some helper functions

    /// Verify that an ICMP Parameter Problem packet was actually sent in
    /// response to a packet with an unrecognized IPv6 extension header option.
    ///
    /// `verify_icmp_for_unrecognized_ext_hdr_option` verifies that the next
    /// frame in `net` is an ICMP packet with code set to `code`, and pointer
    /// set to `pointer`.
    fn verify_icmp_for_unrecognized_ext_hdr_option(
        code: Icmpv6ParameterProblemCode,
        pointer: u32,
        packet: &[u8],
    ) {
        // Check the ICMP that bob attempted to send to alice
        let mut buffer = Buf::new(packet, ..);
        let _frame =
            buffer.parse_with::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::Check).unwrap();
        let packet = buffer.parse::<<Ipv6 as packet_formats::ip::IpExt>::Packet<_>>().unwrap();
        let (src_ip, dst_ip, proto, _): (_, _, _, ParseMetadata) = packet.into_metadata();
        assert_eq!(dst_ip, FAKE_CONFIG_V6.remote_ip.get());
        assert_eq!(src_ip, FAKE_CONFIG_V6.local_ip.get());
        assert_eq!(proto, Ipv6Proto::Icmpv6);
        let icmp =
            buffer.parse_with::<_, Icmpv6Packet<_>>(IcmpParseArgs::new(src_ip, dst_ip)).unwrap();
        if let Icmpv6Packet::ParameterProblem(icmp) = icmp {
            assert_eq!(icmp.code(), code);
            assert_eq!(icmp.message().pointer(), pointer);
        } else {
            panic!("Expected ICMPv6 Parameter Problem: {:?}", icmp);
        }
    }

    /// Populate a buffer `bytes` with data required to test unrecognized
    /// options.
    ///
    /// The unrecognized option type will be located at index 48. `bytes` must
    /// be at least 64 bytes long. If `to_multicast` is `true`, the destination
    /// address of the packet will be a multicast address.
    fn buf_for_unrecognized_ext_hdr_option_test(
        bytes: &mut [u8],
        action: ExtensionHeaderOptionAction,
        to_multicast: bool,
    ) -> Buf<&mut [u8]> {
        assert!(bytes.len() >= 64);

        let action: u8 = action.into();

        // Unrecognized Option type.
        let oty = 63 | (action << 6);

        #[rustfmt::skip]
        bytes[40..64].copy_from_slice(&[
            // Destination Options Extension Header
            IpProto::Udp.into(),      // Next Header
            1,                        // Hdr Ext Len (In 8-octet units, not including first 8 octets)
            0,                        // Pad1
            1,   0,                   // Pad2
            1,   1, 0,                // Pad3
            oty, 6, 0, 0, 0, 0, 0, 0, // Unrecognized type w/ action = discard

            // Body
            1, 2, 3, 4, 5, 6, 7, 8
        ][..]);
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);

        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());

        bytes[6] = Ipv6ExtHdrType::DestinationOptions.into();
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());

        if to_multicast {
            bytes[24..40].copy_from_slice(
                &[255, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32][..],
            );
        } else {
            bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        }

        Buf::new(bytes, ..)
    }

    /// Create an IPv4 packet builder.
    fn get_ipv4_builder() -> Ipv4PacketBuilder {
        Ipv4PacketBuilder::new(
            FAKE_CONFIG_V4.remote_ip,
            FAKE_CONFIG_V4.local_ip,
            10,
            IpProto::Udp.into(),
        )
    }

    /// Process an IP fragment depending on the `Ip` `process_ip_fragment` is
    /// specialized with.
    fn process_ip_fragment<I: Ip>(
        ctx: &mut FakeCtx,
        device: &DeviceId<FakeBindingsCtx>,
        fragment_id: u16,
        fragment_offset: u8,
        fragment_count: u8,
    ) {
        match I::VERSION {
            IpVersion::V4 => {
                process_ipv4_fragment(ctx, device, fragment_id, fragment_offset, fragment_count)
            }
            IpVersion::V6 => {
                process_ipv6_fragment(ctx, device, fragment_id, fragment_offset, fragment_count)
            }
        }
    }

    /// Generate and 'receive' an IPv4 fragment packet.
    ///
    /// `fragment_offset` is the fragment offset. `fragment_count` is the number
    /// of fragments for a packet. The generated packet will have a body of size
    /// 8 bytes.
    fn process_ipv4_fragment(
        ctx: &mut FakeCtx,
        device: &DeviceId<FakeBindingsCtx>,
        fragment_id: u16,
        fragment_offset: u8,
        fragment_count: u8,
    ) {
        assert!(fragment_offset < fragment_count);

        let m_flag = fragment_offset < (fragment_count - 1);

        let mut builder = get_ipv4_builder();
        builder.id(fragment_id);
        builder.fragment_offset(fragment_offset as u16);
        builder.mf_flag(m_flag);
        let mut body: Vec<u8> = Vec::new();
        body.extend(fragment_offset * 8..fragment_offset * 8 + 8);
        let buffer =
            Buf::new(body, ..).encapsulate(builder).serialize_vec_outer().unwrap().into_inner();
        ctx.test_api().receive_ip_packet::<Ipv4, _>(
            device,
            Some(FrameDestination::Individual { local: true }),
            buffer,
        );
    }

    /// Generate and 'receive' an IPv6 fragment packet.
    ///
    /// `fragment_offset` is the fragment offset. `fragment_count` is the number
    /// of fragments for a packet. The generated packet will have a body of size
    /// 8 bytes.
    fn process_ipv6_fragment(
        ctx: &mut FakeCtx,
        device: &DeviceId<FakeBindingsCtx>,
        fragment_id: u16,
        fragment_offset: u8,
        fragment_count: u8,
    ) {
        assert!(fragment_offset < fragment_count);

        let m_flag = fragment_offset < (fragment_count - 1);

        let mut bytes = vec![0; 48];
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);
        bytes[6] = Ipv6ExtHdrType::Fragment.into(); // Next Header
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());
        bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        bytes[40] = IpProto::Udp.into();
        bytes[42] = fragment_offset >> 5;
        bytes[43] = ((fragment_offset & 0x1F) << 3) | if m_flag { 1 } else { 0 };
        bytes[44..48].copy_from_slice(&(u32::try_from(fragment_id).unwrap().to_be_bytes()));
        bytes.extend(fragment_offset * 8..fragment_offset * 8 + 8);
        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());
        let buffer = Buf::new(bytes, ..);
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            device,
            Some(FrameDestination::Individual { local: true }),
            buffer,
        );
    }

    #[test]
    fn test_ipv6_icmp_parameter_problem_non_must() {
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(FAKE_CONFIG_V6).build();
        let device: DeviceId<_> = device_ids[0].clone().into();

        // Test parsing an IPv6 packet with invalid next header value which
        // we SHOULD send an ICMP response for (but we don't since its not a
        // MUST).

        #[rustfmt::skip]
        let bytes: &mut [u8] = &mut [
            // FixedHeader (will be replaced later)
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,

            // Body
            1, 2, 3, 4, 5,
        ][..];
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);
        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());
        bytes[6] = 255; // Invalid Next Header
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());
        bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        let buf = Buf::new(bytes, ..);

        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device,
            Some(FrameDestination::Individual { local: true }),
            buf,
        );

        assert_eq!(ctx.core_ctx.ipv4.icmp.inner.tx_counters.parameter_problem.get(), 0);
        assert_eq!(ctx.core_ctx.ipv6.icmp.inner.tx_counters.parameter_problem.get(), 0);
        assert_eq!(ctx.core_ctx.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 0);
        assert_eq!(ctx.core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);
    }

    #[test]
    fn test_ipv6_icmp_parameter_problem_must() {
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(FAKE_CONFIG_V6).build();
        let device: DeviceId<_> = device_ids[0].clone().into();

        // Test parsing an IPv6 packet where we MUST send an ICMP parameter problem
        // response (invalid routing type for a routing extension header).

        #[rustfmt::skip]
        let bytes: &mut [u8] = &mut [
            // FixedHeader (will be replaced later)
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,

            // Routing Extension Header
            IpProto::Udp.into(),         // Next Header
            4,                                  // Hdr Ext Len (In 8-octet units, not including first 8 octets)
            255,                                // Routing Type (Invalid)
            1,                                  // Segments Left
            0, 0, 0, 0,                         // Reserved
            // Addresses for Routing Header w/ Type 0
            0,  1,  2,  3,  4,  5,  6,  7,  8,  9,  10, 11, 12, 13, 14, 15,
            16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,

            // Body
            1, 2, 3, 4, 5,
        ][..];
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);
        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());
        bytes[6] = Ipv6ExtHdrType::Routing.into();
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());
        bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        let buf = Buf::new(bytes, ..);
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device,
            Some(FrameDestination::Individual { local: true }),
            buf,
        );
        let frames = ctx.bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        verify_icmp_for_unrecognized_ext_hdr_option(
            Icmpv6ParameterProblemCode::ErroneousHeaderField,
            42,
            &frame[..],
        );
    }

    #[test]
    fn test_ipv6_unrecognized_ext_hdr_option() {
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(FAKE_CONFIG_V6).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let mut expected_icmps = 0;
        let mut bytes = [0; 64];
        let frame_dst = FrameDestination::Individual { local: true };

        // Test parsing an IPv6 packet where we MUST send an ICMP parameter
        // problem due to an unrecognized extension header option.

        // Test with unrecognized option type set with action = skip & continue.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::SkipAndContinue,
            false,
        );
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        assert_eq!(
            ctx.core_ctx.inner_icmp_state::<Ipv6>().tx_counters.parameter_problem.get(),
            expected_icmps
        );
        assert_eq!(ctx.core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);
        assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);

        // Test with unrecognized option type set with
        // action = discard.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacket,
            false,
        );
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        assert_eq!(
            ctx.core_ctx.inner_icmp_state::<Ipv6>().tx_counters.parameter_problem.get(),
            expected_icmps
        );
        assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);

        // Test with unrecognized option type set with
        // action = discard & send icmp
        // where dest addr is a unicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmp,
            false,
        );
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        expected_icmps += 1;
        assert_eq!(
            ctx.core_ctx.inner_icmp_state::<Ipv6>().tx_counters.parameter_problem.get(),
            expected_icmps
        );
        let frames = ctx.bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        verify_icmp_for_unrecognized_ext_hdr_option(
            Icmpv6ParameterProblemCode::UnrecognizedIpv6Option,
            48,
            &frame[..],
        );

        // Test with unrecognized option type set with
        // action = discard & send icmp
        // where dest addr is a multicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmp,
            true,
        );
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        expected_icmps += 1;
        assert_eq!(
            ctx.core_ctx.inner_icmp_state::<Ipv6>().tx_counters.parameter_problem.get(),
            expected_icmps
        );

        let frames = ctx.bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        verify_icmp_for_unrecognized_ext_hdr_option(
            Icmpv6ParameterProblemCode::UnrecognizedIpv6Option,
            48,
            &frame[..],
        );

        // Test with unrecognized option type set with
        // action = discard & send icmp if not multicast addr
        // where dest addr is a unicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmpNoMulticast,
            false,
        );
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        expected_icmps += 1;
        assert_eq!(
            ctx.core_ctx.inner_icmp_state::<Ipv6>().tx_counters.parameter_problem.get(),
            expected_icmps
        );

        let frames = ctx.bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        verify_icmp_for_unrecognized_ext_hdr_option(
            Icmpv6ParameterProblemCode::UnrecognizedIpv6Option,
            48,
            &frame[..],
        );

        // Test with unrecognized option type set with
        // action = discard & send icmp if not multicast addr
        // but dest addr is a multicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmpNoMulticast,
            true,
        );
        // Do not expect an ICMP response for this packet
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        assert_eq!(
            ctx.core_ctx.inner_icmp_state::<Ipv6>().tx_counters.parameter_problem.get(),
            expected_icmps
        );
        assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);

        // None of our tests should have sent an icmpv4 packet, or dispatched an
        // IP packet after the first.

        assert_eq!(ctx.core_ctx.inner_icmp_state::<Ipv4>().tx_counters.parameter_problem.get(), 0);
        assert_eq!(ctx.core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);
    }

    #[ip_test]
    fn test_ip_packet_reassembly_not_needed<I: Ip + TestIpExt>() {
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id = 5;

        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Test that a non fragmented packet gets dispatched right away.

        process_ip_fragment::<I>(&mut ctx, &device, fragment_id, 0, 1);

        // Make sure the packet got dispatched.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);
    }

    #[ip_test]
    fn test_ip_packet_reassembly<I: Ip + TestIpExt>() {
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id = 5;

        // Test that the received packet gets dispatched only after receiving
        // all the fragments.

        // Process fragment #0
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id, 0, 3);

        // Process fragment #1
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id, 1, 3);

        // Make sure no packets got dispatched yet.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Process fragment #2
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id, 2, 3);

        // Make sure the packet finally got dispatched now that the final
        // fragment has been 'received'.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);
    }

    #[ip_test]
    fn test_ip_packet_reassembly_with_packets_arriving_out_of_order<I: Ip + TestIpExt>() {
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id_0 = 5;
        let fragment_id_1 = 10;
        let fragment_id_2 = 15;

        // Test that received packets gets dispatched only after receiving all
        // the fragments with out of order arrival of fragments.

        // Process packet #0, fragment #1
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id_0, 1, 3);

        // Process packet #1, fragment #2
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id_1, 2, 3);

        // Process packet #1, fragment #0
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id_1, 0, 3);

        // Make sure no packets got dispatched yet.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Process a packet that does not require reassembly (packet #2, fragment #0).
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id_2, 0, 1);

        // Make packet #1 got dispatched since it didn't need reassembly.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        // Process packet #0, fragment #2
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id_0, 2, 3);

        // Make sure no other packets got dispatched yet.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        // Process packet #0, fragment #0
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id_0, 0, 3);

        // Make sure that packet #0 finally got dispatched now that the final
        // fragment has been 'received'.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 2);

        // Process packet #1, fragment #1
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id_1, 1, 3);

        // Make sure the packet finally got dispatched now that the final
        // fragment has been 'received'.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 3);
    }

    #[ip_test]
    fn test_ip_packet_reassembly_timer<I: Ip + TestIpExt>()
    where
        IpLayerTimerId: From<FragmentTimerId<I>>,
    {
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id = 5;

        // Test to make sure that packets must arrive within the reassembly
        // timer.

        // Process fragment #0
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id, 0, 3);

        // Make sure a timer got added.
        ctx.bindings_ctx.timer_ctx().assert_timers_installed_range([(
            IpLayerTimerId::from(FragmentTimerId::<I>::default()).into(),
            ..,
        )]);

        // Process fragment #1
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id, 1, 3);

        assert_eq!(
            ctx.trigger_next_timer().unwrap(),
            IpLayerTimerId::from(FragmentTimerId::<I>::default()).into(),
        );

        // Make sure no other timers exist.
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

        // Process fragment #2
        process_ip_fragment::<I>(&mut ctx, &device, fragment_id, 2, 3);

        // Make sure no packets got dispatched yet since even though we
        // technically received all the fragments, this fragment (#2) arrived
        // too late and the reassembly timer was triggered, causing the prior
        // fragment data to be discarded.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);
    }

    #[ip_test]
    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    fn test_ip_reassembly_only_at_destination_host<I: Ip + TestIpExt + crate::IpExt>() {
        // Create a new network with two parties (alice & bob) and enable IP
        // packet routing for alice.
        let a = "alice";
        let b = "bob";
        let fake_config = I::FAKE_CONFIG;
        let (mut alice, alice_device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.swap()).build();
        {
            set_forwarding_enabled::<_, I>(&mut alice, &alice_device_ids[0].clone().into(), true);
        }
        let (bob, bob_device_ids) = FakeEventDispatcherBuilder::from_config(fake_config).build();
        let mut net = crate::testutil::new_simple_fake_network(
            a,
            alice,
            alice_device_ids[0].downgrade(),
            b,
            bob,
            bob_device_ids[0].downgrade(),
        );
        // Make sure the (strongly referenced) device IDs are dropped before
        // `net`.
        let alice_device_id: DeviceId<_> = alice_device_ids[0].clone().into();
        core::mem::drop((alice_device_ids, bob_device_ids));

        let fragment_id = 5;

        // Test that packets only get reassembled and dispatched at the
        // destination. In this test, Alice is receiving packets from some
        // source that is actually destined for Bob. Alice should simply forward
        // the packets without attempting to process or reassemble the
        // fragments.

        // Process fragment #0
        net.with_context("alice", |ctx| {
            process_ip_fragment::<I>(ctx, &alice_device_id, fragment_id, 0, 3);
        });
        // Make sure the packet got sent from alice to bob
        assert!(!net.step().is_idle());

        // Process fragment #1
        net.with_context("alice", |ctx| {
            process_ip_fragment::<I>(ctx, &alice_device_id, fragment_id, 1, 3);
        });
        assert!(!net.step().is_idle());

        // Make sure no packets got dispatched yet.
        assert_eq!(
            net.context("alice").core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            0
        );
        assert_eq!(
            net.context("bob").core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            0
        );

        // Process fragment #2
        net.with_context("alice", |ctx| {
            process_ip_fragment::<I>(ctx, &alice_device_id, fragment_id, 2, 3);
        });
        assert!(!net.step().is_idle());

        // Make sure the packet finally got dispatched now that the final
        // fragment has been received by bob.
        assert_eq!(
            net.context("alice").core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            0
        );
        assert_eq!(
            net.context("bob").core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            1
        );

        // Make sure there are no more events.
        assert!(net.step().is_idle());
    }

    #[test]
    fn test_ipv6_packet_too_big() {
        // Test sending an IPv6 Packet Too Big Error when receiving a packet
        // that is too big to be forwarded when it isn't destined for the node
        // it arrived at.

        let fake_config = Ipv6::FAKE_CONFIG;
        let mut dispatcher_builder = FakeEventDispatcherBuilder::from_config(fake_config.clone());
        let extra_ip = UnicastAddr::new(Ipv6Addr::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 100,
        ]))
        .unwrap();
        let extra_mac = UnicastAddr::new(Mac::new([12, 13, 14, 15, 16, 17])).unwrap();
        dispatcher_builder.add_ndp_table_entry(0, extra_ip, extra_mac);
        dispatcher_builder.add_ndp_table_entry(
            0,
            extra_mac.to_ipv6_link_local().addr().get(),
            extra_mac,
        );
        let (mut ctx, device_ids) = dispatcher_builder.build();

        let device: DeviceId<_> = device_ids[0].clone().into();
        set_forwarding_enabled::<_, Ipv6>(&mut ctx, &device, true);
        let frame_dst = FrameDestination::Individual { local: true };

        // Construct an IPv6 packet that is too big for our MTU (MTU = 1280;
        // body itself is 5000). Note, the final packet will be larger because
        // of IP header data.
        let mut rng = new_rng(70812476915813);
        let body: Vec<u8> = core::iter::repeat_with(|| rng.gen()).take(5000).collect();

        // Ip packet from some node destined to a remote on this network,
        // arriving locally.
        let mut ipv6_packet_buf = Buf::new(body, ..)
            .encapsulate(Ipv6PacketBuilder::new(
                extra_ip,
                fake_config.remote_ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap();
        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device,
            Some(frame_dst),
            ipv6_packet_buf.clone(),
        );

        let Ctx { core_ctx, bindings_ctx } = &mut ctx;
        // Should not have dispatched the packet.
        assert_eq!(core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);
        assert_eq!(core_ctx.inner_icmp_state::<Ipv6>().tx_counters.packet_too_big.get(), 1);

        // Should have sent out one frame, and the received packet should be a
        // Packet Too Big ICMP error message.
        let frames = bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        // The original packet's TTL gets decremented so we decrement here
        // to validate the rest of the icmp message body.
        let ipv6_packet_buf_mut: &mut [u8] = ipv6_packet_buf.as_mut();
        ipv6_packet_buf_mut[7] -= 1;
        let (_, _, _, _, _, message, code) =
            parse_icmp_packet_in_ip_packet_in_ethernet_frame::<Ipv6, _, Icmpv6PacketTooBig, _>(
                &frame[..],
                EthernetFrameLengthCheck::NoCheck,
                move |packet| {
                    // Size of the ICMP message body should be size of the
                    // MTU without IP and ICMP headers.
                    let expected_len = 1280 - 48;
                    let actual_body: &[u8] = ipv6_packet_buf.as_ref();
                    let actual_body = &actual_body[..expected_len];
                    assert_eq!(packet.body().len(), expected_len);
                    assert_eq!(packet.body().bytes(), actual_body);
                },
            )
            .unwrap();
        assert_eq!(code, IcmpUnusedCode);
        // MTU should match the MTU for the link.
        assert_eq!(message, Icmpv6PacketTooBig::new(1280));
    }

    fn create_packet_too_big_buf<A: IpAddress>(
        src_ip: A,
        dst_ip: A,
        mtu: u16,
        body: Option<Buf<Vec<u8>>>,
    ) -> Buf<Vec<u8>> {
        let body = body.unwrap_or_else(|| Buf::new(Vec::new(), ..));

        match [src_ip, dst_ip].into() {
            IpAddr::V4([src_ip, dst_ip]) => body
                .encapsulate(IcmpPacketBuilder::<Ipv4, IcmpDestUnreachable>::new(
                    dst_ip,
                    src_ip,
                    Icmpv4DestUnreachableCode::FragmentationRequired,
                    NonZeroU16::new(mtu)
                        .map(IcmpDestUnreachable::new_for_frag_req)
                        .unwrap_or_else(Default::default),
                ))
                .encapsulate(Ipv4PacketBuilder::new(src_ip, dst_ip, 64, Ipv4Proto::Icmp))
                .serialize_vec_outer()
                .unwrap(),
            IpAddr::V6([src_ip, dst_ip]) => body
                .encapsulate(IcmpPacketBuilder::<Ipv6, Icmpv6PacketTooBig>::new(
                    dst_ip,
                    src_ip,
                    IcmpUnusedCode,
                    Icmpv6PacketTooBig::new(u32::from(mtu)),
                ))
                .encapsulate(Ipv6PacketBuilder::new(src_ip, dst_ip, 64, Ipv6Proto::Icmpv6))
                .serialize_vec_outer()
                .unwrap(),
        }
        .into_inner()
    }

    trait GetPmtuIpExt: Ip {
        fn get_pmtu<BC: BindingsContext>(
            state: &StackState<BC>,
            local_ip: Self::Addr,
            remote_ip: Self::Addr,
        ) -> Option<Mtu>;
    }

    impl GetPmtuIpExt for Ipv4 {
        fn get_pmtu<BC: BindingsContext>(
            state: &StackState<BC>,
            local_ip: Ipv4Addr,
            remote_ip: Ipv4Addr,
        ) -> Option<Mtu> {
            state.ipv4.inner.pmtu_cache.lock().get_pmtu(local_ip, remote_ip)
        }
    }

    impl GetPmtuIpExt for Ipv6 {
        fn get_pmtu<BC: BindingsContext>(
            state: &StackState<BC>,
            local_ip: Ipv6Addr,
            remote_ip: Ipv6Addr,
        ) -> Option<Mtu> {
            state.ipv6.inner.pmtu_cache.lock().get_pmtu(local_ip, remote_ip)
        }
    }

    #[ip_test]
    fn test_ip_update_pmtu<I: Ip + TestIpExt + GetPmtuIpExt>() {
        // Test receiving a Packet Too Big (IPv6) or Dest Unreachable
        // Fragmentation Required (IPv4) which should update the PMTU if it is
        // less than the current value.

        let fake_config = I::FAKE_CONFIG;
        let (mut ctx, device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.clone()).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        // Update PMTU from None.

        let new_mtu1 = Mtu::new(u32::from(I::MINIMUM_LINK_MTU) + 100);

        // Create ICMP IP buf
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(u32::from(new_mtu1)).unwrap(),
            None,
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        assert_eq!(
            I::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            new_mtu1
        );

        // Don't update PMTU when current PMTU is less than reported MTU.

        let new_mtu2 = Mtu::new(u32::from(I::MINIMUM_LINK_MTU) + 200);

        // Create IPv6 ICMPv6 packet too big packet with MTU larger than current
        // PMTU.
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(u32::from(new_mtu2)).unwrap(),
            None,
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 2);

        // The PMTU should not have updated to `new_mtu2`
        assert_eq!(
            I::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            new_mtu1
        );

        // Update PMTU when current PMTU is greater than the reported MTU.

        let new_mtu3 = Mtu::new(u32::from(I::MINIMUM_LINK_MTU) + 50);

        // Create IPv6 ICMPv6 packet too big packet with MTU smaller than
        // current PMTU.
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(u32::from(new_mtu3)).unwrap(),
            None,
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 3);

        // The PMTU should have updated to 1900.
        assert_eq!(
            I::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            new_mtu3
        );
    }

    #[ip_test]
    fn test_ip_update_pmtu_too_low<I: Ip + TestIpExt + GetPmtuIpExt>() {
        // Test receiving a Packet Too Big (IPv6) or Dest Unreachable
        // Fragmentation Required (IPv4) which should not update the PMTU if it
        // is less than the min MTU.

        let fake_config = I::FAKE_CONFIG;
        let (mut ctx, device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.clone()).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        // Update PMTU from None but with an MTU too low.

        let new_mtu1 = u32::from(I::MINIMUM_LINK_MTU) - 1;

        // Create ICMP IP buf
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(new_mtu1).unwrap(),
            None,
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        assert_eq!(
            I::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get()),
            None
        );
    }

    /// Create buffer to be used as the ICMPv4 message body
    /// where the original packet's body  length is `body_len`.
    fn create_orig_packet_buf(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, body_len: usize) -> Buf<Vec<u8>> {
        Buf::new(vec![0; body_len], ..)
            .encapsulate(Ipv4PacketBuilder::new(src_ip, dst_ip, 64, IpProto::Udp.into()))
            .serialize_vec_outer()
            .unwrap()
            .into_inner()
    }

    #[test]
    fn test_ipv4_remote_no_rfc1191() {
        // Test receiving an IPv4 Dest Unreachable Fragmentation
        // Required from a node that does not implement RFC 1191.

        let fake_config = Ipv4::FAKE_CONFIG;
        let (mut ctx, device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.clone()).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        // Update from None.

        // Create ICMP IP buf w/ orig packet body len = 500; orig packet len =
        // 520
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0, // A 0 value indicates that the source of the
            // ICMP message does not implement RFC 1191.
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 500)
                .into(),
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<Ipv4, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 1);

        // Should have decreased PMTU value to the next lower PMTU
        // plateau from `crate::ip::path_mtu::PMTU_PLATEAUS`.
        assert_eq!(
            Ipv4::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            Mtu::new(508),
        );

        // Don't Update when packet size is too small.

        // Create ICMP IP buf w/ orig packet body len = 1; orig packet len = 21
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0,
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 1)
                .into(),
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<Ipv4, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 2);

        // Should not have updated PMTU as there is no other valid
        // lower PMTU value.
        assert_eq!(
            Ipv4::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            Mtu::new(508),
        );

        // Update to lower PMTU estimate based on original packet size.

        // Create ICMP IP buf w/ orig packet body len = 60; orig packet len = 80
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0,
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 60)
                .into(),
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<Ipv4, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 3);

        // Should have decreased PMTU value to the next lower PMTU
        // plateau from `crate::ip::path_mtu::PMTU_PLATEAUS`.
        assert_eq!(
            Ipv4::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            Mtu::new(68),
        );

        // Should not update PMTU because the next low PMTU from this original
        // packet size is higher than current PMTU.

        // Create ICMP IP buf w/ orig packet body len = 290; orig packet len =
        // 310
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0, // A 0 value indicates that the source of the
            // ICMP message does not implement RFC 1191.
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 290)
                .into(),
        );

        // Receive the IP packet.
        ctx.test_api().receive_ip_packet::<Ipv4, _>(&device, Some(frame_dst), packet_buf);

        // Should have dispatched the packet.
        assert_eq!(ctx.core_ctx.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 4);

        // Should not have updated the PMTU as the current PMTU is lower.
        assert_eq!(
            Ipv4::get_pmtu(&ctx.core_ctx, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            Mtu::new(68),
        );
    }

    #[test]
    fn test_invalid_icmpv4_in_ipv6() {
        let ip_config = Ipv6::FAKE_CONFIG;
        let (mut ctx, device_ids) =
            FakeEventDispatcherBuilder::from_config(ip_config.clone()).build();
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        let ic_config = Ipv4::FAKE_CONFIG;
        let icmp_builder = IcmpPacketBuilder::<Ipv4, _>::new(
            ic_config.remote_ip,
            ic_config.local_ip,
            IcmpUnusedCode,
            IcmpEchoRequest::new(0, 0),
        );

        let ip_builder = Ipv6PacketBuilder::new(
            ip_config.remote_ip,
            ip_config.local_ip,
            64,
            Ipv6Proto::Other(Ipv4Proto::Icmp.into()),
        );

        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(icmp_builder)
            .encapsulate(ip_builder)
            .serialize_vec_outer()
            .unwrap();

        crate::device::testutil::enable_device(&mut ctx, &device);
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);

        let Ctx { core_ctx, bindings_ctx } = &mut ctx;
        // Should not have dispatched the packet.
        assert_eq!(core_ctx.ipv6.inner.counters.receive_ip_packet.get(), 1);
        assert_eq!(core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);

        // In IPv6, the next header value (ICMP(v4)) would have been considered
        // unrecognized so an ICMP parameter problem response SHOULD be sent,
        // but the netstack chooses to just drop the packet since we are not
        // required to send the ICMP response.
        assert_matches!(bindings_ctx.take_ethernet_frames()[..], []);
    }

    #[test]
    fn test_invalid_icmpv6_in_ipv4() {
        let ip_config = Ipv4::FAKE_CONFIG;
        let (mut ctx, device_ids) =
            FakeEventDispatcherBuilder::from_config(ip_config.clone()).build();
        // First possible device id.
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        let ic_config = Ipv6::FAKE_CONFIG;
        let icmp_builder = IcmpPacketBuilder::<Ipv6, _>::new(
            ic_config.remote_ip,
            ic_config.local_ip,
            IcmpUnusedCode,
            IcmpEchoRequest::new(0, 0),
        );

        let ip_builder = Ipv4PacketBuilder::new(
            ip_config.remote_ip,
            ip_config.local_ip,
            64,
            Ipv4Proto::Other(Ipv6Proto::Icmpv6.into()),
        );

        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(icmp_builder)
            .encapsulate(ip_builder)
            .serialize_vec_outer()
            .unwrap();

        ctx.test_api().receive_ip_packet::<Ipv4, _>(&device, Some(frame_dst), buf);

        // Should have dispatched the packet but resulted in an ICMP error.
        assert_eq!(ctx.core_ctx.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 1);
        assert_eq!(ctx.core_ctx.inner_icmp_state::<Ipv4>().tx_counters.dest_unreachable.get(), 1);
        let frames = ctx.bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        let (_, _, _, _, _, _, code) =
            parse_icmp_packet_in_ip_packet_in_ethernet_frame::<Ipv4, _, IcmpDestUnreachable, _>(
                &frame[..],
                EthernetFrameLengthCheck::NoCheck,
                |_| {},
            )
            .unwrap();
        assert_eq!(code, Icmpv4DestUnreachableCode::DestProtocolUnreachable);
    }

    #[ip_test]
    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    fn test_joining_leaving_ip_multicast_group<I: Ip + TestIpExt + crate::IpExt>() {
        // Test receiving a packet destined to a multicast IP (and corresponding
        // multicast MAC).

        let config = I::FAKE_CONFIG;
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(config.clone()).build();
        let eth_device = &device_ids[0];
        let device: DeviceId<_> = eth_device.clone().into();
        let multi_addr = I::get_multicast_addr(3).get();
        let dst_mac = Mac::from(&MulticastAddr::new(multi_addr).unwrap());
        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(I::PacketBuilder::new(
                config.remote_ip.get(),
                multi_addr,
                64,
                IpProto::Udp.into(),
            ))
            .encapsulate(EthernetFrameBuilder::new(
                config.remote_mac.get(),
                dst_mac,
                I::ETHER_TYPE,
                ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .into_inner();

        let multi_addr = MulticastAddr::new(multi_addr).unwrap();
        // Should not have dispatched the packet since we are not in the
        // multicast group `multi_addr`.

        assert!(!ctx.test_api().is_in_ip_multicast(&device, multi_addr));
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf.clone());

        let Ctx { core_ctx, bindings_ctx } = &mut ctx;
        assert_eq!(core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Join the multicast group and receive the packet, we should dispatch
        // it.
        match multi_addr.into() {
            IpAddr::V4(multicast_addr) => crate::ip::device::join_ip_multicast::<Ipv4, _, _>(
                &mut core_ctx.context(),
                bindings_ctx,
                &device,
                multicast_addr,
            ),
            IpAddr::V6(multicast_addr) => crate::ip::device::join_ip_multicast::<Ipv6, _, _>(
                &mut core_ctx.context(),
                bindings_ctx,
                &device,
                multicast_addr,
            ),
        }
        assert!(ctx.test_api().is_in_ip_multicast(&device, multi_addr));
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf.clone());

        let Ctx { core_ctx, bindings_ctx } = &mut ctx;
        assert_eq!(core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        // Leave the multicast group and receive the packet, we should not
        // dispatch it.
        match multi_addr.into() {
            IpAddr::V4(multicast_addr) => crate::ip::device::leave_ip_multicast::<Ipv4, _, _>(
                &mut core_ctx.context(),
                bindings_ctx,
                &device,
                multicast_addr,
            ),
            IpAddr::V6(multicast_addr) => crate::ip::device::leave_ip_multicast::<Ipv6, _, _>(
                &mut core_ctx.context(),
                bindings_ctx,
                &device,
                multicast_addr,
            ),
        }
        assert!(!ctx.test_api().is_in_ip_multicast(&device, multi_addr));
        ctx.core_api()
            .device::<EthernetLinkDevice>()
            .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf);
        assert_eq!(ctx.core_ctx.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);
    }

    #[test]
    fn test_no_dispatch_non_ndp_packets_during_ndp_dad() {
        // Here we make sure we are not dispatching packets destined to a
        // tentative address (that is performing NDP's Duplicate Address
        // Detection (DAD)) -- IPv6 only.

        let config = Ipv6::FAKE_CONFIG;
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

        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device,
                Ipv6DeviceConfigurationUpdate {
                    // Doesn't matter as long as DAD is enabled.
                    dad_transmits: Some(NonZeroU16::new(1)),
                    // Auto-generate a link-local address.
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

        let frame_dst = FrameDestination::Individual { local: true };

        let ip: Ipv6Addr = config.local_mac.to_ipv6_link_local().addr().get();

        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(Ipv6PacketBuilder::new(config.remote_ip, ip, 64, IpProto::Udp.into()))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();

        // Received packet should not have been dispatched.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf.clone());

        assert_eq!(ctx.core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);

        // Wait until DAD is complete. Arbitrarily choose a year in the future
        // as a time after which we're confident DAD will be complete. We can't
        // run until there are no timers because some timers will always exist
        // for background tasks.
        //
        // TODO(https://fxbug.dev/42125450): Once this test is contextified, use a
        // more precise condition to ensure that DAD is complete.
        let now = ctx.bindings_ctx.now();
        let _: Vec<_> =
            ctx.trigger_timers_until_instant(now + Duration::from_secs(60 * 60 * 24 * 365));

        // Received packet should have been dispatched.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        assert_eq!(ctx.core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);

        // Set the new IP (this should trigger DAD).
        let ip = config.local_ip.get();
        ctx.core_api()
            .device_ip::<Ipv6>()
            .add_ip_addr_subnet(&device, AddrSubnet::new(ip, 128).unwrap())
            .unwrap();

        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(Ipv6PacketBuilder::new(config.remote_ip, ip, 64, IpProto::Udp.into()))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();

        // Received packet should not have been dispatched.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf.clone());
        assert_eq!(ctx.core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);

        // Make sure all timers are done (DAD to complete on the interface due
        // to new IP).
        //
        // TODO(https://fxbug.dev/42125450): Once this test is contextified, use a
        // more precise condition to ensure that DAD is complete.
        let _: Vec<_> = ctx.trigger_timers_until_instant(FakeInstant::LATEST);

        // Received packet should have been dispatched.
        ctx.test_api().receive_ip_packet::<Ipv6, _>(&device, Some(frame_dst), buf);
        assert_eq!(ctx.core_ctx.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 2);
    }

    #[test]
    fn test_drop_non_unicast_ipv6_source() {
        // Test that an inbound IPv6 packet with a non-unicast source address is
        // dropped.
        let cfg = FAKE_CONFIG_V6;
        let (mut ctx, _device_ids) = FakeEventDispatcherBuilder::from_config(cfg.clone()).build();
        let device = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: cfg.local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &device);

        let ip: Ipv6Addr = cfg.local_mac.to_ipv6_link_local().addr().get();
        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(Ipv6PacketBuilder::new(
                Ipv6::MULTICAST_SUBNET.network(),
                ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();

        ctx.test_api().receive_ip_packet::<Ipv6, _>(
            &device,
            Some(FrameDestination::Individual { local: true }),
            buf,
        );
        assert_eq!(ctx.core_ctx.ipv6.inner.counters.version_rx.non_unicast_source.get(), 1);
    }

    #[test]
    fn test_receive_ip_packet_action() {
        let v4_config = Ipv4::FAKE_CONFIG;
        let v6_config = Ipv6::FAKE_CONFIG;

        let mut builder = FakeEventDispatcherBuilder::default();
        // Both devices have the same MAC address, which is a bit weird, but not
        // a problem for this test.
        let v4_subnet = AddrSubnet::from_witness(v4_config.local_ip, 16).unwrap().subnet();
        let dev_idx0 =
            builder.add_device_with_ip(v4_config.local_mac, v4_config.local_ip.get(), v4_subnet);
        let dev_idx1 = builder.add_device_with_ip_and_config(
            v6_config.local_mac,
            v6_config.local_ip.get(),
            AddrSubnet::from_witness(v6_config.local_ip, 64).unwrap().subnet(),
            Ipv4DeviceConfigurationUpdate::default(),
            Ipv6DeviceConfigurationUpdate {
                // Auto-generate a link-local address.
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let (mut ctx, device_ids) = builder.clone().build();
        let v4_dev: DeviceId<_> = device_ids[dev_idx0].clone().into();
        let v6_dev: DeviceId<_> = device_ids[dev_idx1].clone().into();

        let Ctx { core_ctx, bindings_ctx } = &mut ctx;

        // Receive packet addressed to us.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v4_dev,
                v4_config.local_ip
            ),
            ReceivePacketAction::Deliver
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v6_dev,
                v6_config.local_ip
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to the IPv4 subnet broadcast address.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v4_dev,
                SpecifiedAddr::new(v4_subnet.broadcast()).unwrap()
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to the IPv4 limited broadcast address.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v4_dev,
                Ipv4::LIMITED_BROADCAST_ADDRESS
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to a multicast address we're subscribed to.
        crate::ip::device::join_ip_multicast::<Ipv4, _, _>(
            &mut core_ctx.context(),
            bindings_ctx,
            &v4_dev,
            Ipv4::ALL_ROUTERS_MULTICAST_ADDRESS,
        );
        assert_eq!(
            receive_ipv4_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v4_dev,
                Ipv4::ALL_ROUTERS_MULTICAST_ADDRESS.into_specified()
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to the all-nodes multicast address.
        assert_eq!(
            receive_ipv6_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v6_dev,
                Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.into_specified()
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to a multicast address we're subscribed to.
        assert_eq!(
            receive_ipv6_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v6_dev,
                v6_config.local_ip.to_solicited_node_address().into_specified(),
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to a tentative address.
        {
            // Construct a one-off context that has DAD enabled. The context
            // built above has DAD disabled, and so addresses start off in the
            // assigned state rather than the tentative state.
            let mut ctx = FakeCtx::default();
            let local_mac = v6_config.local_mac;
            let eth_device =
                ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                    EthernetCreationProperties {
                        mac: local_mac,
                        max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                    },
                    DEFAULT_INTERFACE_METRIC,
                );
            let device = eth_device.clone().into();
            let _: Ipv6DeviceConfigurationUpdate = ctx
                .core_api()
                .device_ip::<Ipv6>()
                .update_configuration(
                    &device,
                    Ipv6DeviceConfigurationUpdate {
                        // Doesn't matter as long as DAD is enabled.
                        dad_transmits: Some(NonZeroU16::new(1)),
                        // Auto-generate a link-local address.
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
            let Ctx { core_ctx, bindings_ctx } = &mut ctx;
            let tentative: UnicastAddr<Ipv6Addr> = local_mac.to_ipv6_link_local().addr().get();
            assert_eq!(
                receive_ipv6_packet_action(
                    &mut core_ctx.context(),
                    bindings_ctx,
                    &device,
                    tentative.into_specified()
                ),
                ReceivePacketAction::Drop { reason: DropReason::Tentative }
            );
            // Clean up secondary context.
            core::mem::drop(device);
            ctx.core_api().device().remove_device(eth_device).into_removed();
        }

        // Receive packet destined to a remote address when forwarding is
        // disabled on the inbound interface.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v4_dev,
                v4_config.remote_ip
            ),
            ReceivePacketAction::Drop { reason: DropReason::ForwardingDisabledInboundIface }
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v6_dev,
                v6_config.remote_ip
            ),
            ReceivePacketAction::Drop { reason: DropReason::ForwardingDisabledInboundIface }
        );

        // Receive packet destined to a remote address when forwarding is
        // enabled both globally and on the inbound device.
        set_forwarding_enabled::<_, Ipv4>(&mut ctx, &v4_dev, true);
        set_forwarding_enabled::<_, Ipv6>(&mut ctx, &v6_dev, true);
        let Ctx { core_ctx, bindings_ctx } = &mut ctx;
        assert_eq!(
            receive_ipv4_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v4_dev,
                v4_config.remote_ip
            ),
            ReceivePacketAction::Forward {
                dst: Destination { next_hop: NextHop::RemoteAsNeighbor, device: v4_dev.clone() }
            }
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v6_dev,
                v6_config.remote_ip
            ),
            ReceivePacketAction::Forward {
                dst: Destination { next_hop: NextHop::RemoteAsNeighbor, device: v6_dev.clone() }
            }
        );

        // Receive packet destined to a host with no route when forwarding is
        // enabled both globally and on the inbound device.
        *core_ctx.ipv4.inner.table.write() = Default::default();
        *core_ctx.ipv6.inner.table.write() = Default::default();
        assert_eq!(
            receive_ipv4_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v4_dev,
                v4_config.remote_ip
            ),
            ReceivePacketAction::SendNoRouteToDest
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut core_ctx.context(),
                bindings_ctx,
                &v6_dev,
                v6_config.remote_ip
            ),
            ReceivePacketAction::SendNoRouteToDest
        );

        // Cleanup all device references.
        core::mem::drop((v4_dev, v6_dev));
        for device in device_ids {
            ctx.core_api().device().remove_device(device).into_removed();
        }
    }

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum Device {
        First,
        Second,
        Loopback,
    }

    impl Device {
        fn index(self) -> usize {
            match self {
                Self::First => 0,
                Self::Second => 1,
                Self::Loopback => 2,
            }
        }

        fn from_index(index: usize) -> Self {
            match index {
                0 => Self::First,
                1 => Self::Second,
                2 => Self::Loopback,
                x => panic!("index out of bounds: {x}"),
            }
        }

        fn ip_address<A: IpAddress>(self) -> IpDeviceAddr<A>
        where
            A::Version: TestIpExt,
        {
            match self {
                Self::First | Self::Second => <A::Version as TestIpExt>::get_other_ip_address(
                    (self.index() + 1).try_into().unwrap(),
                )
                .try_into()
                .unwrap(),
                Self::Loopback => <A::Version as Ip>::LOOPBACK_ADDRESS.try_into().unwrap(),
            }
        }

        fn mac(self) -> UnicastAddr<Mac> {
            UnicastAddr::new(Mac::new([0, 1, 2, 3, 4, self.index().try_into().unwrap()])).unwrap()
        }

        fn link_local_addr(self) -> IpDeviceAddr<Ipv6Addr> {
            match self {
                Self::First | Self::Second => SpecifiedAddr::new(Ipv6Addr::new([
                    0xfe80,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    self.index().try_into().unwrap(),
                ]))
                .unwrap()
                .try_into()
                .unwrap(),
                Self::Loopback => panic!("should not generate link local addresses for loopback"),
            }
        }
    }

    fn remote_ip<I: TestIpExt>() -> SpecifiedAddr<I::Addr> {
        I::get_other_ip_address(27)
    }

    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    fn make_test_ctx<I: Ip + TestIpExt + crate::IpExt>(
    ) -> (Ctx<FakeBindingsCtx>, Vec<DeviceId<FakeBindingsCtx>>) {
        let mut builder = FakeEventDispatcherBuilder::default();
        for device in [Device::First, Device::Second] {
            let ip: SpecifiedAddr<I::Addr> = device.ip_address().into();
            let subnet =
                AddrSubnet::from_witness(ip, <I::Addr as IpAddress>::BYTES * 8).unwrap().subnet();
            let index = builder.add_device_with_ip(device.mac(), ip.get(), subnet);
            assert_eq!(index, device.index());
        }
        let (mut ctx, device_ids) = builder.build();
        let mut device_ids = device_ids.into_iter().map(Into::into).collect::<Vec<_>>();

        if I::VERSION.is_v6() {
            for device in [Device::First, Device::Second] {
                ctx.core_api()
                    .device_ip::<Ipv6>()
                    .add_ip_addr_subnet(
                        &device_ids[device.index()],
                        AddrSubnet::new(device.link_local_addr().addr(), 64).unwrap(),
                    )
                    .unwrap();
            }
        }

        let loopback_id = ctx
            .core_api()
            .device::<LoopbackDevice>()
            .add_device_with_default_state(
                LoopbackCreationProperties { mtu: Ipv6::MINIMUM_LINK_MTU },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        crate::device::testutil::enable_device(&mut ctx, &loopback_id);
        ctx.core_api()
            .device_ip::<I>()
            .add_ip_addr_subnet(
                &loopback_id,
                AddrSubnet::from_witness(I::LOOPBACK_ADDRESS, I::LOOPBACK_SUBNET.prefix()).unwrap(),
            )
            .unwrap();
        assert_eq!(device_ids.len(), Device::Loopback.index());
        device_ids.push(loopback_id);
        (ctx, device_ids)
    }

    fn do_route_lookup<I: Ip + TestIpExt + IpDeviceStateIpExt>(
        ctx: &mut FakeCtx,
        device_ids: Vec<DeviceId<FakeBindingsCtx>>,
        egress_device: Option<Device>,
        local_ip: Option<IpDeviceAddr<I::Addr>>,
        dest_ip: RoutableIpAddr<I::Addr>,
    ) -> Result<ResolvedRoute<I, Device>, ResolveRouteError>
    where
        for<'a> UnlockedCoreCtx<'a, FakeBindingsCtx>:
            IpSocketContext<I, FakeBindingsCtx, DeviceId = DeviceId<FakeBindingsCtx>>,
    {
        let egress_device = egress_device.map(|d| &device_ids[d.index()]);

        let (mut core_ctx, bindings_ctx) = ctx.contexts();
        IpSocketContext::<I, _>::lookup_route(
            &mut core_ctx,
            bindings_ctx,
            egress_device,
            local_ip,
            dest_ip,
        )
        // Convert device IDs in any route so it's easier to compare.
        .map(|ResolvedRoute { src_addr, device, local_delivery_device, next_hop }| {
            let device = Device::from_index(device_ids.iter().position(|d| d == &device).unwrap());
            let local_delivery_device = local_delivery_device.map(|device| {
                Device::from_index(device_ids.iter().position(|d| d == &device).unwrap())
            });
            ResolvedRoute { src_addr, device, local_delivery_device, next_hop }
        })
    }

    #[ip_test]
    #[test_case(None,
                None,
                Device::First.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::Loopback,
                    local_delivery_device: Some(Device::First), next_hop: NextHop::RemoteAsNeighbor
                }); "local delivery")]
    #[test_case(Some(Device::First.ip_address()),
                None,
                Device::First.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::Loopback,
                    local_delivery_device: Some(Device::First), next_hop: NextHop::RemoteAsNeighbor
                }); "local delivery specified local addr")]
    #[test_case(Some(Device::First.ip_address()),
                Some(Device::First),
                Device::First.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::Loopback,
                    local_delivery_device: Some(Device::First), next_hop: NextHop::RemoteAsNeighbor
                }); "local delivery specified device and addr")]
    #[test_case(None,
                Some(Device::Loopback),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable);
                "local delivery specified loopback device no addr")]
    #[test_case(None,
                Some(Device::Loopback),
                Device::Loopback.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::Loopback.ip_address(),
                    device: Device::Loopback, local_delivery_device: Some(Device::Loopback),
                    next_hop: NextHop::RemoteAsNeighbor,
                }); "local delivery to loopback addr via specified loopback device no addr")]
    #[test_case(None,
                Some(Device::Second),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable);
                "local delivery specified mismatched device no addr")]
    #[test_case(Some(Device::First.ip_address()),
                Some(Device::Loopback),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable); "local delivery specified loopback device")]
    #[test_case(Some(Device::First.ip_address()),
                Some(Device::Second),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable); "local delivery specified mismatched device")]
    #[test_case(None,
                None,
                remote_ip::<I>().try_into().unwrap(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "remote delivery")]
    #[test_case(Some(Device::First.ip_address()),
                None,
                remote_ip::<I>().try_into().unwrap(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "remote delivery specified addr")]
    #[test_case(Some(Device::Second.ip_address()), None, remote_ip::<I>().try_into().unwrap(),
                Err(ResolveRouteError::NoSrcAddr); "remote delivery specified addr no route")]
    #[test_case(None,
                Some(Device::First),
                remote_ip::<I>().try_into().unwrap(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "remote delivery specified device")]
    #[test_case(None, Some(Device::Second), remote_ip::<I>().try_into().unwrap(),
                Err(ResolveRouteError::Unreachable); "remote delivery specified device no route")]
    #[test_case(Some(Device::Second.ip_address()),
                None,
                Device::First.ip_address(),
                Ok(ResolvedRoute {src_addr: Device::Second.ip_address(), device: Device::Loopback,
                    local_delivery_device: Some(Device::Second),
                    next_hop: NextHop::RemoteAsNeighbor });
                "local delivery cross device")]
    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    fn lookup_route<I: Ip + TestIpExt + crate::IpExt>(
        local_ip: Option<IpDeviceAddr<I::Addr>>,
        egress_device: Option<Device>,
        dest_ip: RoutableIpAddr<I::Addr>,
        expected_result: Result<ResolvedRoute<I, Device>, ResolveRouteError>,
    ) {
        set_logger_for_test();

        let (mut ctx, device_ids) = make_test_ctx::<I>();

        // Add a route to the remote address only for Device::First.
        ctx.test_api()
            .add_route(AddableEntryEither::without_gateway(
                Subnet::new(*remote_ip::<I>(), <I::Addr as IpAddress>::BYTES * 8).unwrap().into(),
                device_ids[Device::First.index()].clone(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            ))
            .unwrap();

        let result = do_route_lookup(&mut ctx, device_ids, egress_device, local_ip, dest_ip);
        assert_eq!(result, expected_result);
    }

    #[ip_test]
    #[test_case(None,
                None,
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "no constraints")]
    #[test_case(Some(Device::First.ip_address()),
                None,
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "constrain local addr")]
    #[test_case(Some(Device::Second.ip_address()), None,
                Ok(ResolvedRoute { src_addr: Device::Second.ip_address(), device: Device::Second,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "constrain local addr to second device")]
    #[test_case(None,
                Some(Device::First),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "constrain device")]
    #[test_case(None, Some(Device::Second),
                Ok(ResolvedRoute { src_addr: Device::Second.ip_address(), device: Device::Second,
                    local_delivery_device: None, next_hop: NextHop::RemoteAsNeighbor });
                "constrain to second device")]
    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    fn lookup_route_multiple_devices_with_route<I: Ip + TestIpExt + crate::IpExt>(
        local_ip: Option<IpDeviceAddr<I::Addr>>,
        egress_device: Option<Device>,
        expected_result: Result<ResolvedRoute<I, Device>, ResolveRouteError>,
    ) {
        set_logger_for_test();

        let (mut ctx, device_ids) = make_test_ctx::<I>();

        // Add a route to the remote address for both devices, with preference
        // for the first.
        for device in [Device::First, Device::Second] {
            ctx.test_api()
                .add_route(AddableEntryEither::without_gateway(
                    Subnet::new(*remote_ip::<I>(), <I::Addr as IpAddress>::BYTES * 8)
                        .unwrap()
                        .into(),
                    device_ids[device.index()].clone(),
                    AddableMetric::ExplicitMetric(RawMetric(device.index().try_into().unwrap())),
                ))
                .unwrap();
        }

        let result = do_route_lookup(
            &mut ctx,
            device_ids,
            egress_device,
            local_ip,
            remote_ip::<I>().try_into().unwrap(),
        );
        assert_eq!(result, expected_result);
    }

    #[test_case(None, None, Device::Second.link_local_addr(),
                Ok(ResolvedRoute { src_addr: Device::Second.link_local_addr(),
                    device: Device::Loopback, local_delivery_device: Some(Device::Second),
                    next_hop: NextHop::RemoteAsNeighbor });
                "local delivery no local address to link-local")]
    #[test_case(Some(Device::Second.ip_address()), None, Device::Second.link_local_addr(),
                Ok(ResolvedRoute { src_addr: Device::Second.ip_address(), device: Device::Loopback,
                    local_delivery_device: Some(Device::Second),
                    next_hop: NextHop::RemoteAsNeighbor });
                "local delivery same device to link-local")]
    #[test_case(Some(Device::Second.link_local_addr()), None, Device::Second.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::Second.link_local_addr(),
                    device: Device::Loopback, local_delivery_device: Some(Device::Second),
                    next_hop: NextHop::RemoteAsNeighbor });
                "local delivery same device from link-local")]
    #[test_case(Some(Device::First.ip_address()), None, Device::Second.link_local_addr(),
                Err(ResolveRouteError::NoSrcAddr);
                "local delivery cross device to link-local")]
    #[test_case(Some(Device::First.link_local_addr()), None, Device::Second.ip_address(),
                Err(ResolveRouteError::NoSrcAddr);
                "local delivery cross device from link-local")]
    fn lookup_route_v6only(
        local_ip: Option<IpDeviceAddr<Ipv6Addr>>,
        egress_device: Option<Device>,
        dest_ip: RoutableIpAddr<Ipv6Addr>,
        expected_result: Result<ResolvedRoute<Ipv6, Device>, ResolveRouteError>,
    ) {
        set_logger_for_test();

        let (mut ctx, device_ids) = make_test_ctx::<Ipv6>();

        let result = do_route_lookup(&mut ctx, device_ids, egress_device, local_ip, dest_ip);
        assert_eq!(result, expected_result);
    }
}
