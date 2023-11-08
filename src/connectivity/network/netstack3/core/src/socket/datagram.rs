// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Shared code for implementing datagram sockets.

use alloc::collections::HashSet;
use core::{
    convert::Infallible as Never,
    fmt::Debug,
    hash::Hash,
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU8},
    ops::{Deref, DerefMut},
};

use dense_map::{DenseMap, Entry as DenseMapEntry, EntryKey};
use derivative::Derivative;
use either::Either;
use explicit::UnreachableExt as _;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, Ipv4, Ipv6},
    MulticastAddr, MulticastAddress as _, NonMappedAddr, SpecifiedAddr, ZonedAddr,
};
use packet::{BufferMut, Serializer};
use packet_formats::ip::IpProtoExt;
use thiserror::Error;

use crate::{
    algorithm::ProtocolFlowId,
    context::RngContext,
    convert::{BidirectionalConverter, OwnedOrRefsBidirectionalConverter, UninstantiableConverter},
    device::{self, AnyDevice, DeviceIdContext, Id, WeakId},
    error::{LocalAddressError, NotFoundError, RemoteAddressError, SocketError, ZonedAddressError},
    ip::{
        device::state::IpDeviceStateIpExt,
        socket::{
            BufferIpSocketHandler, IpSock, IpSockCreateAndSendError, IpSockCreationError,
            IpSockSendError, IpSocketHandler, SendOneShotIpPacketError, SendOptions,
        },
        BufferTransportIpContext, EitherDeviceId, HopLimits, MulticastMembershipHandler,
        ResolveRouteError, TransportIpContext,
    },
    socket::{
        self,
        address::{
            AddrIsMappedError, AddrVecIter, ConnAddr, ConnIpAddr, DualStackConnIpAddr,
            DualStackListenerIpAddr, ListenerIpAddr, SocketIpAddr, SocketZonedIpAddr,
        },
        AddrVec, BoundSocketMap, ExistsError, InsertError, ListenerAddr, Shutdown,
        SocketMapAddrSpec, SocketMapConflictPolicy, SocketMapStateSpec,
    },
};

/// Datagram demultiplexing map.
pub(crate) type BoundSockets<I, D, A, S> = BoundSocketMap<I, D, A, S>;

/// Storage of state for all datagram sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct SocketsState<I: IpExt, D: device::WeakId, S: DatagramSocketSpec>(
    DenseMap<SocketState<I, D, S>>,
);

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> Deref for SocketsState<I, D, S> {
    type Target = DenseMap<SocketState<I, D, S>>;

    fn deref(&self) -> &Self::Target {
        let Self(idmap) = self;
        idmap
    }
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> DerefMut for SocketsState<I, D, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let Self(idmap) = self;
        idmap
    }
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> SocketsState<I, D, S> {
    pub(crate) fn get_socket_state(&self, id: &S::SocketId<I>) -> Option<&SocketState<I, D, S>> {
        self.get(id.get_key_index())
    }
}

impl<I: IpExt, NewIp: IpExt, D: device::WeakId, S: DatagramSocketSpec> GenericOverIp<NewIp>
    for SocketsState<I, D, S>
{
    type Type = SocketsState<NewIp, D, S>;
}

pub(crate) trait IpExt:
    crate::ip::IpExt + DualStackIpExt + crate::ip::icmp::IcmpIpExt
{
}
impl<I: crate::ip::IpExt + DualStackIpExt + crate::ip::icmp::IcmpIpExt> IpExt for I {}

#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum SocketState<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    Unbound(UnboundSocketState<I, D, S>),
    Bound(BoundSocketState<I, D, S>),
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for SocketState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            Self::Unbound(unbound) => unbound.as_ref(),
            Self::Bound(bound) => bound.as_ref(),
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum BoundSocketState<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    Listener { state: ListenerState<I, D, S>, sharing: S::SharingState },
    Connected { state: S::ConnState<I, D>, sharing: S::SharingState },
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for BoundSocketState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            Self::Listener { state, sharing: _ } => state.as_ref(),
            Self::Connected { state, sharing: _ } => state.as_ref(),
        }
    }
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: DatagramSocketMapSpec<I, D, A>>
    BoundSockets<I, D, A, S>
{
    pub(crate) fn iter_receivers(
        &self,
        (src_ip, src_port): (Option<SocketIpAddr<I::Addr>>, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SocketIpAddr<I::Addr>, A::LocalIdentifier),
        device: D,
    ) -> Option<
        FoundSockets<
            AddrEntry<'_, I, D, A, S>,
            impl Iterator<Item = AddrEntry<'_, I, D, A, S>> + '_,
        >,
    > {
        self.lookup((src_ip, src_port), (dst_ip, dst_port), device)
    }
}

pub(crate) enum FoundSockets<A, It> {
    /// A single recipient was found for the address.
    Single(A),
    /// Indicates the looked-up address was multicast, and holds an iterator of
    /// the found receivers.
    Multicast(It),
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: DatagramSocketMapSpec<I, D, A>>
    BoundSocketMap<I, D, A, S>
{
    /// Finds the socket(s) that should receive an incoming packet.
    ///
    /// Uses the provided addresses and receiving device to look up sockets that
    /// should receive a matching incoming packet. Returns `None` if no sockets
    /// were found, or the results of the lookup.
    fn lookup(
        &self,
        (src_ip, src_port): (Option<SocketIpAddr<I::Addr>>, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SocketIpAddr<I::Addr>, A::LocalIdentifier),
        device: D,
    ) -> Option<
        FoundSockets<
            AddrEntry<'_, I, D, A, S>,
            impl Iterator<Item = AddrEntry<'_, I, D, A, S>> + '_,
        >,
    > {
        let mut matching_entries = AddrVecIter::with_device(
            match (src_ip, src_port) {
                (Some(specified_src_ip), Some(src_port)) => {
                    ConnIpAddr { local: (dst_ip, dst_port), remote: (specified_src_ip, src_port) }
                        .into()
                }
                _ => ListenerIpAddr { addr: Some(dst_ip), identifier: dst_port }.into(),
            },
            device,
        )
        .filter_map(move |addr: AddrVec<I, D, A>| match addr {
            AddrVec::Listen(l) => {
                self.listeners().get_by_addr(&l).map(|state| AddrEntry::Listen(state, l))
            }
            AddrVec::Conn(c) => self.conns().get_by_addr(&c).map(|state| AddrEntry::Conn(state, c)),
        });

        if dst_ip.addr().is_multicast() {
            Some(FoundSockets::Multicast(matching_entries))
        } else {
            let single_entry: Option<_> = matching_entries.next();
            single_entry.map(FoundSockets::Single)
        }
    }
}

pub(crate) enum AddrEntry<'a, I: Ip, D, A: SocketMapAddrSpec, S: SocketMapStateSpec> {
    Listen(&'a S::ListenerAddrState, ListenerAddr<ListenerIpAddr<I::Addr, A::LocalIdentifier>, D>),
    Conn(
        &'a S::ConnAddrState,
        ConnAddr<ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>, D>,
    ),
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"), Default(bound = ""))]
pub(crate) struct UnboundSocketState<I: IpExt, D, S: DatagramSocketSpec> {
    device: Option<D>,
    sharing: S::SharingState,
    ip_options: IpOptions<I, D, S>,
}

impl<I: IpExt, D, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>> for UnboundSocketState<I, D, S> {
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        &self.ip_options
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct ListenerState<I: IpExt, D: Hash + Eq + Debug, S: DatagramSocketSpec + ?Sized> {
    pub(crate) ip_options: IpOptions<I, D, S>,
    pub(crate) addr: ListenerAddr<S::ListenerIpAddr<I>, D>,
}

impl<I: IpExt, D: Debug + Hash + Eq, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for ListenerState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        &self.ip_options
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug, O: Debug"))]
pub(crate) struct ConnState<I: IpExt, D: Eq + Hash, S: DatagramSocketSpec + ?Sized, O> {
    pub(crate) socket: IpSock<I, D, O>,
    pub(crate) shutdown: Shutdown,
    pub(crate) addr: ConnAddr<
        ConnIpAddr<
            I::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
        D,
    >,
    /// Determines whether a call to disconnect this socket should also clear
    /// the device on the socket address.
    ///
    /// This will only be `true` if
    ///   1) the corresponding address has a bound device
    ///   2) the local address does not require a zone
    ///   3) the remote address does require a zone
    ///   4) the device was not set via [`set_unbound_device`]
    ///
    /// In that case, when the socket is disconnected, the device should be
    /// cleared since it was set as part of a `connect` call, not explicitly.
    ///
    /// TODO(http://fxbug.dev/110370): Implement this by changing socket
    /// addresses.
    pub(crate) clear_device_on_disconnect: bool,

    /// The extra state for the connection.
    ///
    /// For UDP it should be [`()`], for ICMP it should be [`NonZeroU16`] to
    /// remember the remote ID set by connect.
    pub(crate) extra: S::ConnStateExtra,
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec, O> AsRef<O> for ConnState<I, D, S, O> {
    fn as_ref(&self) -> &O {
        self.socket.options()
    }
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec, O> AsMut<O> for ConnState<I, D, S, O> {
    fn as_mut(&mut self) -> &mut O {
        self.socket.options_mut()
    }
}

impl<I: IpExt, D: Eq + Hash, S: DatagramSocketSpec, O> ConnState<I, D, S, O> {
    pub(crate) fn should_receive(&self) -> bool {
        let Self { shutdown, socket: _, clear_device_on_disconnect: _, addr: _, extra: _ } = self;
        let Shutdown { receive, send: _ } = shutdown;
        !*receive
    }
}

/// Connection state belong to either this-stack or the other-stack.
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum DualStackConnState<
    I: IpExt + DualStackIpExt,
    D: Eq + Hash,
    S: DatagramSocketSpec + ?Sized,
> {
    /// The [`ConnState`] for a socked connected with [`I::Version`].
    ThisStack(ConnState<I, D, S, IpOptions<I, D, S>>),
    /// The [`ConnState`] for a socked connected with [`I::OtherVersion`].
    // TODO(https://fxbug.dev/21198): Require use once datagram sockets can
    // connect to a dual-stack address.
    #[allow(unused)]
    OtherStack(ConnState<I::OtherVersion, D, S, IpOptions<I, D, S>>),
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for DualStackConnState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            DualStackConnState::ThisStack(state) => state.as_ref(),
            DualStackConnState::OtherStack(state) => state.as_ref(),
        }
    }
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsMut<IpOptions<I, D, S>>
    for DualStackConnState<I, D, S>
{
    fn as_mut(&mut self) -> &mut IpOptions<I, D, S> {
        match self {
            DualStackConnState::ThisStack(state) => state.as_mut(),
            DualStackConnState::OtherStack(state) => state.as_mut(),
        }
    }
}

#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Clone(bound = "D: Clone"), Debug(bound = "D: Debug"), Default(bound = ""))]
pub(crate) struct IpOptions<I: IpExt, D, S: DatagramSocketSpec + ?Sized> {
    multicast_memberships: MulticastMemberships<I::Addr, D>,
    hop_limits: SocketHopLimits,
    other_stack: S::OtherStackIpOptions<I>,
    transparent: bool,
}

impl<I: IpExt, D, S: DatagramSocketSpec> AsRef<Self> for IpOptions<I, D, S> {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl<I: IpExt, D, S: DatagramSocketSpec> IpOptions<I, D, S> {
    pub(crate) fn other_stack(&self) -> &S::OtherStackIpOptions<I> {
        &self.other_stack
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SocketHopLimits {
    unicast: Option<NonZeroU8>,
    // TODO(https://fxbug.dev/108323): Make this an Option<u8> to allow sending
    // multicast packets destined only for the local machine.
    multicast: Option<NonZeroU8>,
}

impl SocketHopLimits {
    pub(crate) fn set_unicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
        move |limits| limits.unicast = value
    }

    pub(crate) fn set_multicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
        move |limits| limits.multicast = value
    }

    fn get_limits_with_defaults(&self, defaults: &HopLimits) -> HopLimits {
        let Self { unicast, multicast } = self;
        HopLimits {
            unicast: unicast.unwrap_or(defaults.unicast),
            multicast: multicast.unwrap_or(defaults.multicast),
        }
    }
}

impl<I: IpExt, D, S: DatagramSocketSpec> SendOptions<I> for IpOptions<I, D, S> {
    fn hop_limit(&self, destination: &SpecifiedAddr<I::Addr>) -> Option<NonZeroU8> {
        if destination.is_multicast() {
            self.hop_limits.multicast
        } else {
            self.hop_limits.unicast
        }
    }
}

#[derive(Clone, Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct MulticastMemberships<A, D>(HashSet<(MulticastAddr<A>, D)>);

#[cfg_attr(test, derive(Debug, PartialEq))]
pub(crate) enum MulticastMembershipChange {
    Join,
    Leave,
}

impl<A: Eq + Hash, D: Eq + Hash + Clone> MulticastMemberships<A, D> {
    pub(crate) fn apply_membership_change(
        &mut self,
        address: MulticastAddr<A>,
        device: &D,
        want_membership: bool,
    ) -> Option<MulticastMembershipChange> {
        let device = device.clone();

        let Self(map) = self;
        if want_membership {
            map.insert((address, device)).then(|| MulticastMembershipChange::Join)
        } else {
            map.remove(&(address, device)).then(|| MulticastMembershipChange::Leave)
        }
    }
}

impl<A: Eq + Hash, D: Eq + Hash> IntoIterator for MulticastMemberships<A, D> {
    type Item = (MulticastAddr<A>, D);
    type IntoIter = <HashSet<(MulticastAddr<A>, D)> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        let Self(memberships) = self;
        memberships.into_iter()
    }
}

impl<A: IpAddress, D: crate::device::Id> ConnAddr<ConnIpAddr<A, NonZeroU16, NonZeroU16>, D> {
    pub(crate) fn from_protocol_flow_and_local_port(
        id: &ProtocolFlowId<SocketIpAddr<A>>,
        local_port: NonZeroU16,
    ) -> Self {
        Self {
            ip: ConnIpAddr {
                local: (*id.local_addr(), local_port),
                remote: (*id.remote_addr(), id.remote_port()),
            },
            device: None,
        }
    }
}

fn leave_all_joined_groups<A: IpAddress, C, SC: MulticastMembershipHandler<A::Version, C>>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    memberships: MulticastMemberships<A, SC::WeakDeviceId>,
) {
    for (addr, device) in memberships {
        let Some(device) = sync_ctx.upgrade_weak_device_id(&device) else {
            continue;
        };
        sync_ctx.leave_multicast_group(ctx, &device, addr)
    }
}

pub(crate) trait LocalIdentifierAllocator<
    I: Ip,
    D: Id,
    A: SocketMapAddrSpec,
    C,
    S: SocketMapStateSpec,
>
{
    fn try_alloc_local_id(
        &mut self,
        bound: &BoundSocketMap<I, D, A, S>,
        ctx: &mut C,
        flow: DatagramFlowId<I::Addr, A::RemoteIdentifier>,
    ) -> Option<A::LocalIdentifier>;
}

#[derive(Hash)]
pub(crate) struct DatagramFlowId<A: IpAddress, RI> {
    pub(crate) local_ip: SocketIpAddr<A>,
    pub(crate) remote_ip: SocketIpAddr<A>,
    pub(crate) remote_id: RI,
}

pub(crate) trait DatagramStateContext<I: IpExt, C, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callback provided to methods.
    type SocketsStateCtx<'a>: DatagramBoundStateContext<I, C, S>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_sockets_state<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_sockets_state_mut<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &mut SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with access to a [`DatagramBoundStateContext`].
    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketsStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

pub(crate) trait DatagramBoundStateContext<I: IpExt + DualStackIpExt, C, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callback provided to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, C>
        + MulticastMembershipHandler<I, C>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>;

    /// Context for dual-stack socket state access.
    ///
    /// This type type provides access, via an implementation of the
    /// [`DualStackDatagramBoundStateContext`] trait, to state necessary for
    /// implementing dual-stack socket operations. While a type must always be
    /// provided, implementations of [`DatagramBoundStateContext`] for socket
    /// types that don't support dual-stack operation (like ICMP and raw IP
    /// sockets, and UDPv4) can use the [`UninstantiableDualStackContext`] type,
    /// which is uninstantiable.
    type DualStackContext: DualStackDatagramBoundStateContext<
        I,
        C,
        S,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Context for single-stack socket access.
    ///
    /// This type provides access, via an implementation of the
    /// [`NonDualStackDatagramBoundStateContext`] trait, to functionality
    /// necessary to implement sockets that do not support dual-stack operation.
    type NonDualStackContext: NonDualStackDatagramBoundStateContext<
        I,
        C,
        S,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// The additional allocator passed to the callback provided to
    /// `with_sockets_mut`.
    type LocalIdAllocator: LocalIdentifierAllocator<
        I,
        Self::WeakDeviceId,
        S::AddrSpec,
        C,
        S::SocketMapSpec<I, Self::WeakDeviceId>,
    >;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_bound_sockets<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Provides access to either the dual-stack or non-dual-stack context.
    ///
    /// For socket types that don't support dual-stack operation (like ICMP,
    /// raw IP sockets, and UDPv4), this method should always return a reference
    /// to the non-dual-stack context to allow the caller to access
    /// non-dual-stack state. Otherwise it should provide an instance of the
    /// `DualStackContext`, which can be used by the caller to access dual-stack
    /// state.
    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext>;

    /// Calls the function with only the inner context.
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

/// Control flow type containing either a dual-stack or non-dual-stack context.
///
/// This type exists to provide nice names to the result of
/// [`BoundStateContext::dual_stack_context`], and to allow generic code to
/// match on when checking whether a socket protocol and IP version support
/// dual-stack operation. If dual-stack operation is supported, a
/// [`MaybeDualStack::DualStack`] value will be held, otherwise a `NonDualStack`
/// value.
///
/// Note that the templated types to not have trait bounds; those are provided
/// by the trait with the `dual_stack_context` function.
///
/// In monomorphized code, this type frequently has exactly one template
/// parameter that is uninstantiable (it contains an instance of
/// [`core::convert::Infallible`] or some other empty enum, or a reference to
/// the same)! That lets the compiler optimize it out completely, creating no
/// actual runtime overhead.
#[derive(Debug)]
pub(crate) enum MaybeDualStack<DS, NDS> {
    DualStack(DS),
    NotDualStack(NDS),
}

// Implement `GenericOverIp` for a `MaybeDualStack` whose `DS` and `NDS` also
// implement `GenericOverIp`.
impl<I: DualStackIpExt, DS: GenericOverIp<I>, NDS: GenericOverIp<I>> GenericOverIp<I>
    for MaybeDualStack<DS, NDS>
{
    type Type = MaybeDualStack<<DS as GenericOverIp<I>>::Type, <NDS as GenericOverIp<I>>::Type>;
}

impl<'a, DS, NDS> MaybeDualStack<&'a mut DS, &'a mut NDS> {
    fn to_converter<I: IpExt, C, S: DatagramSocketSpec>(
        self,
    ) -> MaybeDualStack<DS::Converter, NDS::Converter>
    where
        DS: DualStackDatagramBoundStateContext<I, C, S>,
        NDS: NonDualStackDatagramBoundStateContext<I, C, S>,
    {
        match self {
            MaybeDualStack::DualStack(ds) => MaybeDualStack::DualStack(ds.converter()),
            MaybeDualStack::NotDualStack(nds) => MaybeDualStack::NotDualStack(nds.converter()),
        }
    }
}

/// Provides access to dual-stack socket state.
pub(crate) trait DualStackDatagramBoundStateContext<I: IpExt, C, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callbacks to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, C>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        // Allow creating IP sockets for the other IP version.
        + TransportIpContext<I::OtherVersion, C>;

    /// Returns if the socket state indicates dual-stack operation is enabled.
    fn dual_stack_enabled(&self, state: &impl AsRef<IpOptions<I, Self::WeakDeviceId, S>>) -> bool;

    /// Asserts that the socket state indicates dual-stack operation is enabled.
    ///
    /// Provided trait function.
    fn assert_dual_stack_enabled(&self, state: &impl AsRef<IpOptions<I, Self::WeakDeviceId, S>>) {
        debug_assert!(self.dual_stack_enabled(state), "socket must be dual-stack enabled")
    }

    /// A type for converting between address types.
    ///
    /// This allows converting between the possibly-dual-stack
    /// `S::ListenerIpAddr<I>` and the concrete dual-stack
    /// [`DualStackListenerIpAddr`].
    type Converter: OwnedOrRefsBidirectionalConverter<
            S::ListenerIpAddr<I>,
            DualStackListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnIpAddr<I>,
            DualStackConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnState<I, Self::WeakDeviceId>,
            DualStackConnState<I, Self::WeakDeviceId, S>,
        >;

    /// Returns an instance of a type that implements [`BidirectionalConverter`]
    /// for addresses.
    ///
    /// The returned object can be used to convert between the
    /// `S::ListenerIpAddr<I>` and the appropriate [`DualStackListenerIpAddr`].
    fn converter(&self) -> Self::Converter;

    /// Converts a socket ID to a bound socket ID.
    ///
    /// Converts a socket ID for IP version `I` into a bound socket ID that can
    /// be inserted into the demultiplexing map for IP version `I::OtherVersion`.
    fn to_other_bound_socket_id(
        &self,
        id: S::SocketId<I>,
    ) -> <S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
    >>::BoundSocketId;

    /// Converts an other-IP-version address to an address for IP version `I`.
    fn from_other_ip_addr(&self, addr: <I::OtherVersion as Ip>::Addr) -> I::Addr;

    /// The local id allocator for sockets in the `I` stack.
    type LocalIdAllocator: LocalIdentifierAllocator<
        I,
        Self::WeakDeviceId,
        S::AddrSpec,
        C,
        S::SocketMapSpec<I, Self::WeakDeviceId>,
    >;

    /// The local id allocator for sockets in the `I::OtherVersion` stack
    type OtherLocalIdAllocator: LocalIdentifierAllocator<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
        C,
        S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
    >;

    /// Calls the provided callback with mutable access to both the
    /// demultiplexing maps.
    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
            &mut Self::OtherLocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the provided callback with mutable access to the demultiplexing
    /// map for the other IP version.
    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

/// Provides access to socket state for a single IP version.
pub(crate) trait NonDualStackDatagramBoundStateContext<I: IpExt, C, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// A type for converting between address types.
    ///
    /// This allows converting between the possibly-dual-stack
    /// `S::ListenerIpAddr<I>` and the concrete not-dual-stack
    /// [``ListenerIpAddr`].
    type Converter: OwnedOrRefsBidirectionalConverter<
            S::ListenerIpAddr<I>,
            ListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnIpAddr<I>,
            ConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnState<I, Self::WeakDeviceId>,
            ConnState<I, Self::WeakDeviceId, S, IpOptions<I, Self::WeakDeviceId, S>>,
        >;

    /// Returns an instance of a type that implements [`BidirectionalConverter`]
    /// for addresses.
    ///
    /// The returned object can be used to convert between the
    /// `S::ListenerIpAddr<I>` and the appropriate `ListenerIpAddr`.
    fn converter(&self) -> Self::Converter;
}

/// An uninstantiable type that implements  [`LocalIdentifierAllocator`].
pub(crate) struct UninstantiableAllocator(Never);

impl AsRef<Never> for UninstantiableAllocator {
    fn as_ref(&self) -> &Never {
        let Self(never) = self;
        &never
    }
}

impl<I: Ip, D: device::Id, A: SocketMapAddrSpec, C, S: SocketMapStateSpec>
    LocalIdentifierAllocator<I, D, A, C, S> for UninstantiableAllocator
{
    fn try_alloc_local_id(
        &mut self,
        _bound: &BoundSocketMap<I, D, A, S>,
        _ctx: &mut C,
        _flow: DatagramFlowId<I::Addr, A::RemoteIdentifier>,
    ) -> Option<A::LocalIdentifier> {
        self.uninstantiable_unreachable()
    }
}

/// An uninstantiable type that ipmlements
/// [`DualStackDatagramBoundStateContext`].
pub(crate) struct UninstantiableContext<I, S, P>(Never, PhantomData<(I, S, P)>);

impl<I, S, P> AsRef<Never> for UninstantiableContext<I, S, P> {
    fn as_ref(&self) -> &Never {
        let Self(never, _marker) = self;
        &never
    }
}

impl<I, S, P: DeviceIdContext<AnyDevice>> DeviceIdContext<AnyDevice>
    for UninstantiableContext<I, S, P>
{
    type DeviceId = P::DeviceId;
    type WeakDeviceId = P::WeakDeviceId;
    fn downgrade_device_id(&self, _device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        self.uninstantiable_unreachable()
    }
    fn upgrade_weak_device_id(
        &self,
        _weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    DatagramBoundStateContext<I, C, S> for UninstantiableContext<I, S, P>
{
    type IpSocketsCtx<'a> = P::IpSocketsCtx<'a>;
    type DualStackContext = P::DualStackContext;
    type NonDualStackContext = P::NonDualStackContext;
    type LocalIdAllocator = P::LocalIdAllocator;
    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        self.uninstantiable_unreachable()
    }
    fn with_bound_sockets<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &BoundSockets<
                I,
                Self::WeakDeviceId,
                <S as DatagramSocketSpec>::AddrSpec,
                <S as DatagramSocketSpec>::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                <S as DatagramSocketSpec>::AddrSpec,
                <S as DatagramSocketSpec>::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    NonDualStackDatagramBoundStateContext<I, C, S> for UninstantiableContext<I, S, P>
{
    type Converter = UninstantiableConverter;
    fn converter(&self) -> Self::Converter {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    DualStackDatagramBoundStateContext<I, C, S> for UninstantiableContext<I, S, P>
where
    for<'a> P::IpSocketsCtx<'a>: TransportIpContext<I::OtherVersion, C>,
{
    type IpSocketsCtx<'a> = P::IpSocketsCtx<'a>;

    fn dual_stack_enabled(&self, _state: &impl AsRef<IpOptions<I, Self::WeakDeviceId, S>>) -> bool {
        self.uninstantiable_unreachable()
    }

    type Converter = UninstantiableConverter;
    fn converter(&self) -> Self::Converter {
        self.uninstantiable_unreachable()
    }

    fn to_other_bound_socket_id(
        &self,
        _id: S::SocketId<I>,
    ) -> <S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
    >>::BoundSocketId {
        self.uninstantiable_unreachable()
    }

    fn from_other_ip_addr(&self, _addr: <I::OtherVersion as Ip>::Addr) -> I::Addr {
        self.uninstantiable_unreachable()
    }

    type LocalIdAllocator = UninstantiableAllocator;
    type OtherLocalIdAllocator = UninstantiableAllocator;

    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
            &mut Self::OtherLocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }

    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<<I>::OtherVersion, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
}

pub(crate) trait DatagramStateNonSyncContext<I: Ip, S>: RngContext {}
impl<C: RngContext, I: Ip, S> DatagramStateNonSyncContext<I, S> for C {}

pub(crate) trait BufferDatagramStateContext<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut>:
    DatagramStateContext<I, C, S>
{
    type BufferSocketStateCtx<'a>: BufferDatagramBoundStateContext<
        I,
        C,
        S,
        B,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_sockets_state_buf<
        O,
        F: FnOnce(&mut Self::BufferSocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_sockets_state_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferSocketStateCtx<'_>,
            &mut SocketsState<I, Self::WeakDeviceId, S>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

pub(crate) trait BufferDatagramBoundStateContext<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut>:
    DatagramBoundStateContext<I, C, S>
{
    type BufferIpSocketsCtx<'a>: BufferTransportIpContext<
        I,
        C,
        B,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_bound_sockets_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
            &BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_bound_sockets_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with only the inner context.
    fn with_transport_context_buf<O, F: FnOnce(&mut Self::BufferIpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

impl<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut, SC: DatagramStateContext<I, C, S>>
    BufferDatagramStateContext<I, C, S, B> for SC
where
    for<'a> SC::SocketsStateCtx<'a>: BufferDatagramBoundStateContext<I, C, S, B>,
{
    type BufferSocketStateCtx<'a> = SC::SocketsStateCtx<'a>;

    fn with_sockets_state_buf<
        O,
        F: FnOnce(&mut Self::BufferSocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_sockets_state(self, cb)
    }

    fn with_sockets_state_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferSocketStateCtx<'_>,
            &mut SocketsState<I, Self::WeakDeviceId, S>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_sockets_state_mut(self, cb)
    }
}

impl<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut, SC: DatagramBoundStateContext<I, C, S>>
    BufferDatagramBoundStateContext<I, C, S, B> for SC
where
    for<'a> SC::IpSocketsCtx<'a>: BufferTransportIpContext<I, C, B>,
{
    type BufferIpSocketsCtx<'a> = SC::IpSocketsCtx<'a>;

    fn with_bound_sockets_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
            &BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_bound_sockets(self, cb)
    }

    fn with_bound_sockets_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_bound_sockets_mut(self, cb)
    }

    fn with_transport_context_buf<O, F: FnOnce(&mut Self::BufferIpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_transport_context(self, cb)
    }
}

/// Types and behavior for datagram socket demultiplexing map.
///
/// `I: Ip` describes the type of packets that can be received by sockets in
/// the map.
pub(crate) trait DatagramSocketMapSpec<I: Ip, D: Id, A: SocketMapAddrSpec>:
    SocketMapStateSpec<ListenerId = Self::BoundSocketId, ConnId = Self::BoundSocketId>
    + SocketMapConflictPolicy<
        ListenerAddr<ListenerIpAddr<I::Addr, A::LocalIdentifier>, D>,
        <Self as SocketMapStateSpec>::ListenerSharingState,
        I,
        D,
        A,
    > + SocketMapConflictPolicy<
        ConnAddr<ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>, D>,
        <Self as SocketMapStateSpec>::ConnSharingState,
        I,
        D,
        A,
    >
{
    /// The type of IDs stored in a [`BoundSocketMap`] for which this is the
    /// specification.
    ///
    /// This can be the same as [`DatagramSocketSpec::SocketId`] but doesn't
    /// have to be. In the case of
    /// dual-stack sockets, for example, an IPv4 socket will have type
    /// `DatagramSocketSpec::SocketId<Ipv4>` but the IPv4 demultiplexing map
    /// might have `BoundSocketId=Either<DatagramSocketSpec::SocketId<Ipv4>,
    /// DatagramSocketSpec::SocketId<Ipv6>>` to allow looking up IPv6 sockets
    /// when receiving IPv4 packets.
    type BoundSocketId: Clone + Debug;
}

/// Common features of dual-stack sockets that vary by IP version.
///
/// This trait exists to provide per-IP-version associated types that are
/// useful for implementing dual-stack sockets. The types are intentionally
/// asymmetric - `DualStackIpExt::Xxx` has a different shape for the [`Ipv4`]
/// and [`Ipv6`] impls.
// TODO(https://fxbug.dev/21198): Move this somewhere more general once the
// approach is proven to work for datagram sockets. When implementing dual-stack
// TCP, extract a supertrait from `DatagramSocketSpec` as described in the TODO
// below and put that together with this trait.
pub(crate) trait DualStackIpExt: Ip + crate::ip::IpExt {
    /// The "other" IP version, e.g. [`Ipv4`] for [`Ipv6`] and vice-versa.
    type OtherVersion: Ip
        + IpDeviceStateIpExt
        + DualStackIpExt<OtherVersion = Self>
        + crate::ip::IpExt;

    /// The type of socket that can receive an IP packet.
    ///
    /// For `Ipv4`, this is [`EitherIpSocket<S>`], and for `Ipv6` it is just
    /// `S::SocketId<Ipv6>`.
    ///
    /// [`EitherIpSocket<S>]`: [EitherIpSocket]
    // TODO(https://fxbug.dev/21198): Extract the necessary GAT from
    // DatagramSocketSpec into its own trait and use that as the bound here.
    type DualStackBoundSocketId<S: DatagramSocketSpec>: Clone + Debug + Eq;

    /// The IP options type for the other stack that will be held for a socket.
    ///
    /// For [`Ipv4`], this is `()`, and for [`Ipv6`] it is `State`. For a
    /// protocol like UDP or TCP where the IPv6 socket is dual-stack capable,
    /// the generic state struct can have a field with type
    /// `I::OtherStackIpOptions<Ipv4InIpv6Options>`.
    type OtherStackIpOptions<State: Clone + Debug + Default>: Clone + Debug + Default;

    /// A listener address for dual-stack operation.
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug>: Clone + Debug;

    /// A connected address for dual-stack operation.
    type DualStackConnIpAddr<S: DatagramSocketSpec>: Clone + Debug;

    /// Connection state for a dual-stack socket.
    type DualStackConnState<D: Eq + Hash + Debug, S: DatagramSocketSpec>: Debug
        + AsRef<IpOptions<Self, D, S>>
        + AsMut<IpOptions<Self, D, S>>;

    /// Convert a socket ID into a `Self::DualStackBoundSocketId`.
    ///
    /// For coherency reasons this can't be a `From` bound on
    /// `DualStackBoundSocketId`. If more methods are added, consider moving
    /// this to its own dedicated trait that bounds `DualStackBoundSocketId`.
    fn into_dual_stack_bound_socket_id<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackBoundSocketId<S>
    where
        Self: IpExt;

    /// Convert a `Self::DualStackBoundSocketId` into an [`DualStackIpSocket`].
    fn as_dual_stack_ip_socket<'a, S: DatagramSocketSpec>(
        id: &'a Self::DualStackBoundSocketId<S>,
    ) -> DualStackIpSocket<'a, Self, S>;

    /// Retrieves the associated connection address from the connection state.
    fn conn_addr_from_state<D: Eq + Hash + Debug + Clone, S: DatagramSocketSpec>(
        state: &Self::DualStackConnState<D, S>,
    ) -> ConnAddr<Self::DualStackConnIpAddr<S>, D>;
}

/// An IP Socket ID that is either `Ipv4` or `Ipv6`.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Debug(bound = ""),
    Eq(bound = "S::SocketId<Ipv4>: Eq, S::SocketId<Ipv6>: Eq"),
    PartialEq(bound = "S::SocketId<Ipv4>: PartialEq, S::SocketId<Ipv6>: PartialEq")
)]
pub(crate) enum EitherIpSocket<S: DatagramSocketSpec> {
    V4(S::SocketId<Ipv4>),
    V6(S::SocketId<Ipv6>),
}

/// An IP Socket ID that is either `I` or `I::OtherVersion`.
pub(crate) enum DualStackIpSocket<'a, I: IpExt, S: DatagramSocketSpec> {
    CurrentStack(&'a S::SocketId<I>),
    OtherStack(&'a S::SocketId<I::OtherVersion>),
}

impl DualStackIpExt for Ipv4 {
    type OtherVersion = Ipv6;

    /// Incoming IPv4 packets may be received by either IPv4 or IPv6 sockets.
    type DualStackBoundSocketId<S: DatagramSocketSpec> = EitherIpSocket<S>;
    type OtherStackIpOptions<State: Clone + Debug + Default> = ();
    /// IPv4 sockets can't listen on dual-stack addresses.
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug> =
        ListenerIpAddr<Self::Addr, LocalIdentifier>;
    /// IPv4 sockets cannot connect on dual-stack addresses.
    type DualStackConnIpAddr<S: DatagramSocketSpec> = ConnIpAddr<
        Self::Addr,
        <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    >;
    /// IPv4 sockets cannot connect on dual-stack addresses.
    type DualStackConnState<D: Eq + Hash + Debug, S: DatagramSocketSpec> =
        ConnState<Self, D, S, IpOptions<Self, D, S>>;

    fn into_dual_stack_bound_socket_id<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackBoundSocketId<S> {
        EitherIpSocket::V4(id)
    }

    fn as_dual_stack_ip_socket<'a, S: DatagramSocketSpec>(
        id: &'a Self::DualStackBoundSocketId<S>,
    ) -> DualStackIpSocket<'a, Self, S> {
        match id {
            EitherIpSocket::V4(id) => DualStackIpSocket::CurrentStack(id),
            EitherIpSocket::V6(id) => DualStackIpSocket::OtherStack(id),
        }
    }

    fn conn_addr_from_state<D: Clone + Debug + Eq + Hash, S: DatagramSocketSpec>(
        state: &Self::DualStackConnState<D, S>,
    ) -> ConnAddr<Self::DualStackConnIpAddr<S>, D> {
        let ConnState { socket: _, shutdown: _, addr, clear_device_on_disconnect: _, extra: _ } =
            state;
        addr.clone()
    }
}

impl DualStackIpExt for Ipv6 {
    type OtherVersion = Ipv4;

    /// Incoming IPv6 packets may only be received by IPv6 sockets.
    type DualStackBoundSocketId<S: DatagramSocketSpec> = S::SocketId<Self>;
    type OtherStackIpOptions<State: Clone + Debug + Default> = State;
    /// IPv6 listeners can listen on dual-stack addresses (if the protocol
    /// and socket are dual-stack-enabled).
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug> =
        DualStackListenerIpAddr<Self::Addr, LocalIdentifier>;
    /// IPv6 sockets can connect on dual-stack addresses (if the protocol and
    /// socket are dual-stack-enabled).
    type DualStackConnIpAddr<S: DatagramSocketSpec> = DualStackConnIpAddr<
        Self::Addr,
        <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    >;
    /// IPv6 sockets can connect on dual-stack addresses (if the protocol and
    /// socket are dual-stack-enabled).
    type DualStackConnState<D: Eq + Hash + Debug, S: DatagramSocketSpec> =
        DualStackConnState<Self, D, S>;

    fn into_dual_stack_bound_socket_id<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackBoundSocketId<S> {
        id
    }

    fn as_dual_stack_ip_socket<'a, S: DatagramSocketSpec>(
        id: &'a Self::DualStackBoundSocketId<S>,
    ) -> DualStackIpSocket<'a, Self, S> {
        DualStackIpSocket::CurrentStack(id)
    }

    fn conn_addr_from_state<D: Clone + Debug + Eq + Hash, S: DatagramSocketSpec>(
        state: &Self::DualStackConnState<D, S>,
    ) -> ConnAddr<Self::DualStackConnIpAddr<S>, D> {
        match state {
            DualStackConnState::ThisStack(state) => {
                let ConnState { addr, .. } = state;
                let ConnAddr { ip, device } = addr.clone();
                ConnAddr { ip: DualStackConnIpAddr::ThisStack(ip), device }
            }
            DualStackConnState::OtherStack(state) => {
                let ConnState {
                    socket: _,
                    shutdown: _,
                    addr,
                    clear_device_on_disconnect: _,
                    extra: _,
                } = state;
                let ConnAddr { ip, device } = addr.clone();
                ConnAddr { ip: DualStackConnIpAddr::OtherStack(ip), device }
            }
        }
    }
}

/// Types and behavior for datagram sockets.
///
/// These sockets may or may not support dual-stack operation.
pub(crate) trait DatagramSocketSpec {
    /// The socket address spec for the datagram socket type.
    ///
    /// This describes the types of identifiers the socket uses, e.g.
    /// local/remote port for UDP.
    type AddrSpec: SocketMapAddrSpec;

    /// Identifier for an individual socket for a given IP version.
    ///
    /// Corresponds uniquely to a socket resource. This is the type that will
    /// be returned by [`create`] and used to identify which socket is being
    /// acted on by calls like [`listen`], [`connect`], [`remove`], etc.
    type SocketId<I: IpExt>: Clone + Debug + EntryKey + From<usize> + Eq;

    /// IP-level options for sending `I::OtherVersion` IP packets.
    type OtherStackIpOptions<I: IpExt>: Clone + Debug + Default;

    /// The type of a listener IP address.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ListenerIpAddr`], which will be one of
    /// [`ListenerIpAddr`] or [`DualStackListenerIpAddr`].
    /// Non-dual-stack-capable protocols (like ICMP and raw IP sockets) should
    /// just use [`ListenerIpAddr`].
    type ListenerIpAddr<I: IpExt>: Clone + Debug;

    /// The sharing state for a socket.
    ///
    /// NB: The underlying [`BoundSocketMap`]` uses separate types for the
    /// sharing state of connected vs listening sockets. At the moment, datagram
    /// sockets have no need for differentiated sharing states, so consolidate
    /// them under one type.
    type SharingState: Clone + Debug + Default;

    /// The type of an IP address for a connected socket.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ConnIpAddr`], which will be one of
    /// [`ConnIpAddr`] or [`DualStackConnIpAddr`].
    /// Non-dual-stack-capable protocols (like ICMP and raw IP sockets) should
    /// just use [`ConnIpAddr`].
    type ConnIpAddr<I: IpExt>: Clone + Debug;

    /// The type of a state held by a connected socket.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ConnState`], which will be one of [`ConnState`] or
    /// [`DualStackConnState`]. Non-dual-stack-capable protocols (like ICMP and
    /// raw IP sockets) should just use [`ConnState`].
    type ConnState<I: IpExt, D: Debug + Eq + Hash>: Debug
        + AsRef<IpOptions<I, D, Self>>
        + AsMut<IpOptions<I, D, Self>>;

    /// The extra state that a connection state want to remember.
    ///
    /// For example: UDP sockets does not have any extra state to remember, so
    /// it should just be `()`; ICMP sockets need to remember the remote ID the
    /// socket is 'connected' to, the remote ID is not used when sending nor
    /// participating in the demuxing decisions. So it will be stored in the
    /// extra state so that it can be retrieved later, i.e, it should be
    /// `NonZeroU16` for ICMP sockets.
    type ConnStateExtra: Debug;

    /// The specification for the [`BoundSocketMap`] for a given IP version.
    ///
    /// Describes the per-address and per-socket values held in the
    /// demultiplexing map for a given IP version.
    type SocketMapSpec<I: IpExt + DualStackIpExt, D: device::WeakId>: DatagramSocketMapSpec<
        I,
        D,
        Self::AddrSpec,
        ListenerSharingState = Self::SharingState,
        ConnSharingState = Self::SharingState,
    >;

    /// Returns the IP protocol of this datagram specification.
    fn ip_proto<I: IpProtoExt>() -> I::Proto;

    /// Converts [`Self::SocketId`] to [`DatagramSocketMapSpec::BoundSocketId`].
    ///
    /// Constructs a socket identifier to its in-demultiplexing map form. For
    /// protocols with dual-stack sockets, like UDP, implementations should
    /// perform a transformation. Otherwise it should be the identity function.
    fn make_bound_socket_map_id<I: IpExt, D: device::WeakId>(
        s: Self::SocketId<I>,
    ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::BoundSocketId;

    /// The type of serializer returned by [`DatagramSocketSpec::make_packet`]
    /// for a given IP version and buffer type.
    type Serializer<I: IpExt, B: BufferMut>: Serializer<Buffer = B>;
    /// The potential error for serializing a packet. For example, in UDP, this
    /// should be infallible but for ICMP, there will be an error if the input
    /// is not an echo request.
    type SerializeError;
    fn make_packet<I: IpExt, B: BufferMut>(
        body: B,
        addr: &ConnIpAddr<
            I::Addr,
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    ) -> Result<Self::Serializer<I, B>, Self::SerializeError>;

    /// Attempts to allocate a local identifier for a listening socket.
    ///
    /// Returns the identifier on success, or `None` on failure.
    fn try_alloc_listen_identifier<I: IpExt, D: device::WeakId>(
        rng: &mut impl RngContext,
        is_available: impl Fn(
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        ) -> Result<(), InUseError>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>;

    /// Retrieves the associated connection ip addr from the connection state.
    fn conn_addr_from_state<I: IpExt, D: Clone + Debug + Eq + Hash>(
        state: &Self::ConnState<I, D>,
    ) -> ConnAddr<Self::ConnIpAddr<I>, D>;
}

pub(crate) struct InUseError;

pub(crate) fn create<I: IpExt, S: DatagramSocketSpec, C, SC: DatagramStateContext<I, C, S>>(
    sync_ctx: &mut SC,
) -> S::SocketId<I> {
    sync_ctx.with_sockets_state_mut(|_sync_ctx, state| {
        state.push(SocketState::Unbound(UnboundSocketState::default())).into()
    })
}

#[derive(Debug)]
pub(crate) enum SocketInfo<I: Ip + DualStackIpExt, D, S: DatagramSocketSpec> {
    Unbound,
    Listener(ListenerAddr<S::ListenerIpAddr<I>, D>),
    Connected(ConnAddr<S::ConnIpAddr<I>, D>),
}

pub(crate) fn close<
    I: IpExt,
    S: DatagramSocketSpec,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
) -> SocketInfo<I, SC::WeakDeviceId, S> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let (ip_options, info) = match state.remove(id.get_key_index()).expect("invalid socket ID")
        {
            SocketState::Unbound(UnboundSocketState { device: _, sharing: _, ip_options }) => {
                (ip_options, SocketInfo::Unbound)
            }
            SocketState::Bound(state) => match sync_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    let op = DualStackRemoveOperation::parse_from_state(dual_stack, id, &state);
                    dual_stack
                        .with_both_bound_sockets_mut(
                            |_sync_ctx, sockets, other_sockets, _alloc, _other_alloc| {
                                op.apply(sockets, other_sockets)
                            },
                        )
                        .into_options_and_info()
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    let op =
                        SingleStackRemoveOperation::parse_from_state(not_dual_stack, id, &state);
                    sync_ctx
                        .with_bound_sockets_mut(|_sync_ctx, sockets, _allocator| op.apply(sockets))
                        .into_options_and_info()
                }
            },
        };
        DatagramBoundStateContext::<I, _, _>::with_transport_context(sync_ctx, |sync_ctx| {
            leave_all_joined_groups(sync_ctx, ctx, ip_options.multicast_memberships)
        });
        info
    })
}

/// State associated with removing a single-stack socket.
enum SingleStackRemove<I: IpExt, D: WeakId, S: DatagramSocketSpec> {
    Listener {
        // The socket's address, stored as a concrete `ListenerIpAddr`.
        concrete_addr: ListenerAddr<
            ListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
            D,
        >,
        // The socket's address, stored as an associated `S::ListenerIpAddr`.
        associated_addr: ListenerAddr<S::ListenerIpAddr<I>, D>,
        ip_options: IpOptions<I, D, S>,
        sharing: S::SharingState,
        socket_id:
            <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId,
    },
    Connected {
        // The socket's address, stored as a concrete `ConnIpAddr`.
        concrete_addr: ConnAddr<
            ConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
            D,
        >,
        // The socket's address, stored as an associated `S:ConnIpAddr`.
        associated_addr: ConnAddr<S::ConnIpAddr<I>, D>,
        ip_options: IpOptions<I, D, S>,
        sharing: S::SharingState,
        socket_id:
            <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId,
    },
}

/// The yet-to-be-performed removal of a single-stack socket.
struct SingleStackRemoveOperation<I: IpExt, D: WeakId, S: DatagramSocketSpec>(
    SingleStackRemove<I, D, S>,
);

impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> SingleStackRemoveOperation<I, D, S> {
    /// Parse the existing socket state, constructing the removal operation.
    fn parse_from_state<C, SC: NonDualStackDatagramBoundStateContext<I, C, S, WeakDeviceId = D>>(
        sync_ctx: &mut SC,
        socket_id: S::SocketId<I>,
        state: &BoundSocketState<I, D, S>,
    ) -> Self {
        SingleStackRemoveOperation(match state {
            BoundSocketState::Listener { state, sharing } => {
                let ListenerState { addr: associated_addr, ip_options } = state;
                let ListenerAddr { ip, device } = associated_addr.clone();
                let concrete_addr = ListenerAddr { ip: sync_ctx.converter().convert(ip), device };
                SingleStackRemove::Listener {
                    concrete_addr,
                    associated_addr: associated_addr.clone(),
                    ip_options: ip_options.clone(),
                    sharing: sharing.clone(),
                    socket_id: S::make_bound_socket_map_id(socket_id),
                }
            }
            BoundSocketState::Connected { state, sharing } => {
                let ConnState {
                    addr,
                    socket,
                    clear_device_on_disconnect: _,
                    shutdown: _,
                    extra: _,
                } = sync_ctx.converter().convert(state);
                let ConnAddr { ip, device } = addr.clone();
                let associated_addr =
                    ConnAddr { ip: sync_ctx.converter().convert_back(ip), device };
                SingleStackRemove::Connected {
                    concrete_addr: addr.clone(),
                    associated_addr,
                    ip_options: socket.options().clone(),
                    sharing: sharing.clone(),
                    socket_id: S::make_bound_socket_map_id(socket_id),
                }
            }
        })
    }

    /// Apply this remove operation to the given `BoundSocketMap`.
    ///
    /// # Panics
    ///
    /// Panics if the given socket map does not contain the socket specified by
    /// this removal operation.
    fn apply(
        self,
        sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
    ) -> SingleStackRemoveInfo<I, D, S> {
        let SingleStackRemoveOperation(single_stack_remove) = self;
        match &single_stack_remove {
            SingleStackRemove::Listener {
                concrete_addr,
                associated_addr: _,
                ip_options: _,
                sharing: _,
                socket_id,
            } => {
                let ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device } =
                    concrete_addr;
                BoundStateHandler::<_, S, _>::remove_listener(
                    sockets,
                    addr,
                    *identifier,
                    device,
                    socket_id,
                );
            }
            SingleStackRemove::Connected {
                concrete_addr,
                associated_addr: _,
                ip_options: _,
                sharing: _,
                socket_id,
            } => {
                sockets
                    .conns_mut()
                    .remove(socket_id, concrete_addr)
                    .expect("UDP connection not found");
            }
        }
        SingleStackRemoveInfo(single_stack_remove)
    }
}

/// Information for a recently-removed single-stack socket.
struct SingleStackRemoveInfo<I: IpExt, D: WeakId, S: DatagramSocketSpec>(
    SingleStackRemove<I, D, S>,
);

impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> SingleStackRemoveInfo<I, D, S> {
    fn into_options_and_info(self) -> (IpOptions<I, D, S>, SocketInfo<I, D, S>) {
        let SingleStackRemoveInfo(single_stack_remove) = self;
        match single_stack_remove {
            SingleStackRemove::Listener {
                concrete_addr: _,
                associated_addr,
                ip_options,
                sharing: _,
                socket_id: _,
            } => (ip_options, SocketInfo::Listener(associated_addr)),
            SingleStackRemove::Connected {
                concrete_addr: _,
                associated_addr,
                ip_options,
                sharing: _,
                socket_id: _,
            } => (ip_options, SocketInfo::Connected(associated_addr)),
        }
    }

    /// Undo this removal by reinserting the socket into the `BoundSocketMap`.
    ///
    /// # Panics
    ///
    /// Panics if the socket can not be inserted into the given map (i.e. if it
    /// already exists.)
    fn reinsert(self, sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>) {
        let SingleStackRemoveInfo(single_stack_remove) = self;
        match single_stack_remove {
            SingleStackRemove::Listener {
                concrete_addr: ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device },
                associated_addr: _,
                ip_options: _,
                sharing,
                socket_id,
            } => {
                BoundStateHandler::<_, S, _>::try_insert_listener(
                    sockets, addr, identifier, device, sharing, socket_id,
                )
                .expect("reinserting just-removed listener failed");
            }
            SingleStackRemove::Connected {
                concrete_addr,
                associated_addr: _,
                ip_options: _,
                sharing,
                socket_id,
            } => {
                let _entry = sockets
                    .conns_mut()
                    .try_insert(concrete_addr, sharing, socket_id)
                    .expect("reinserting just-removed connected failed");
            }
        }
    }
}

/// State associated with removing a dual-stack socket.
enum DualStackRemove<I: IpExt, D: WeakId, S: DatagramSocketSpec> {
    // A socket bound in the current stack is a single-stack removal.
    CurrentStack(SingleStackRemove<I, D, S>),
    ListenerOtherStack {
        // The socket's address, stored as a concrete `ListenerIpAddr`.
        concrete_addr: ListenerAddr<
            ListenerIpAddr<
                <I::OtherVersion as Ip>::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            >,
            D,
        >,
        // The socket's address, stored as an associated `S::ListenerIpAddr`.
        associated_addr: ListenerAddr<S::ListenerIpAddr<I>, D>,
        ip_options: IpOptions<I, D, S>,
        sharing: S::SharingState,
        socket_id: <S::SocketMapSpec<I::OtherVersion, D> as DatagramSocketMapSpec<
            I::OtherVersion,
            D,
            S::AddrSpec,
        >>::BoundSocketId,
    },
    ListenerBothStacks {
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        // The socket's address, stored as an associated `S::ListenerIpAddr`.
        // NB: The addr must be the dual-stack any address, hence there's no
        // need to store it's concrete form.
        associated_addr: ListenerAddr<S::ListenerIpAddr<I>, D>,
        ip_options: IpOptions<I, D, S>,
        sharing: S::SharingState,
        socket_id:
            <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId,
        other_socket_id: <S::SocketMapSpec<I::OtherVersion, D> as DatagramSocketMapSpec<
            I::OtherVersion,
            D,
            S::AddrSpec,
        >>::BoundSocketId,
    },
    // TODO(https://fxbug.dev/135204): Support removing connected sockets in the
    // other stack.
}

struct DualStackRemoveOperation<I: IpExt, D: WeakId, S: DatagramSocketSpec>(
    DualStackRemove<I, D, S>,
);

impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> DualStackRemoveOperation<I, D, S> {
    /// Parse the existing socket state, constructing the removal operation.
    fn parse_from_state<C, SC: DualStackDatagramBoundStateContext<I, C, S, WeakDeviceId = D>>(
        sync_ctx: &mut SC,
        socket_id: S::SocketId<I>,
        state: &BoundSocketState<I, D, S>,
    ) -> Self {
        DualStackRemoveOperation(match state {
            BoundSocketState::Listener { state, sharing } => {
                let ListenerState { addr: associated_addr, ip_options } = state;
                let ListenerAddr { ip, device } = associated_addr.clone();
                match (sync_ctx.converter().convert(ip), sync_ctx.dual_stack_enabled(&ip_options)) {
                    // Dual-stack enabled, bound in both stacks.
                    (DualStackListenerIpAddr::BothStacks(identifier), true) => {
                        DualStackRemove::ListenerBothStacks {
                            identifier: identifier.clone(),
                            device: device,
                            associated_addr: associated_addr.clone(),
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: S::make_bound_socket_map_id(socket_id.clone()),
                            other_socket_id: sync_ctx.to_other_bound_socket_id(socket_id),
                        }
                    }
                    // Bound in this stack, with/without dual-stack enabled.
                    (DualStackListenerIpAddr::ThisStack(addr), true | false) => {
                        DualStackRemove::CurrentStack(SingleStackRemove::Listener {
                            concrete_addr: ListenerAddr { ip: addr, device },
                            associated_addr: associated_addr.clone(),
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: S::make_bound_socket_map_id(socket_id),
                        })
                    }
                    // Dual-stack enabled, bound only in the other stack.
                    (DualStackListenerIpAddr::OtherStack(addr), true) => {
                        DualStackRemove::ListenerOtherStack {
                            concrete_addr: ListenerAddr { ip: addr, device },
                            associated_addr: associated_addr.clone(),
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: sync_ctx.to_other_bound_socket_id(socket_id),
                        }
                    }
                    (DualStackListenerIpAddr::OtherStack(_), false)
                    | (DualStackListenerIpAddr::BothStacks(_), false) => {
                        unreachable!("dual-stack disabled socket cannot use the other stack")
                    }
                }
            }
            BoundSocketState::Connected { state, sharing } => {
                match sync_ctx.converter().convert(state) {
                    DualStackConnState::ThisStack(state) => {
                        let ConnState {
                            addr,
                            socket,
                            clear_device_on_disconnect: _,
                            shutdown: _,
                            extra: _,
                        } = state;
                        let ConnAddr { ip, device } = addr.clone();
                        let ip = DualStackConnIpAddr::ThisStack(ip);
                        let associated_addr =
                            ConnAddr { ip: sync_ctx.converter().convert_back(ip), device };
                        DualStackRemove::CurrentStack(SingleStackRemove::Connected {
                            concrete_addr: addr.clone(),
                            associated_addr,
                            ip_options: socket.options().clone(),
                            sharing: sharing.clone(),
                            socket_id: S::make_bound_socket_map_id(socket_id),
                        })
                    }
                    DualStackConnState::OtherStack(state) => {
                        sync_ctx.assert_dual_stack_enabled(&state);
                        todo!("https://fxbug.dev/135204: Support removing dual-stack connected")
                    }
                }
            }
        })
    }

    /// Apply this remove operation to the given `BoundSocketMap`s.
    ///
    /// # Panics
    ///
    /// Panics if the given socket maps does not contain the socket specified by
    /// this removal operation.
    fn apply(
        self,
        sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
        other_sockets: &mut BoundSocketMap<
            I::OtherVersion,
            D,
            S::AddrSpec,
            S::SocketMapSpec<I::OtherVersion, D>,
        >,
    ) -> DualStackRemoveInfo<I, D, S> {
        let DualStackRemoveOperation(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(single_stack_remove) => {
                let SingleStackRemoveInfo(single_stack_remove) =
                    SingleStackRemoveOperation(single_stack_remove).apply(sockets);
                DualStackRemoveInfo(DualStackRemove::CurrentStack(single_stack_remove))
            }
            DualStackRemove::ListenerOtherStack {
                concrete_addr,
                associated_addr,
                ip_options,
                sharing,
                socket_id,
            } => {
                let ListenerAddr { ip: ListenerIpAddr { addr: inner_addr, identifier }, device } =
                    &concrete_addr;
                BoundStateHandler::<_, S, _>::remove_listener(
                    other_sockets,
                    inner_addr,
                    *identifier,
                    device,
                    &socket_id,
                );
                DualStackRemoveInfo(DualStackRemove::ListenerOtherStack {
                    concrete_addr,
                    associated_addr,
                    ip_options,
                    sharing,
                    socket_id,
                })
            }
            DualStackRemove::ListenerBothStacks {
                identifier,
                device,
                associated_addr,
                ip_options,
                sharing,
                socket_id,
                other_socket_id,
            } => {
                let paired_bound_socket_ids =
                    PairedBoundSocketIds { this: socket_id, other: other_socket_id };
                PairedSocketMapMut::<_, _, S> { bound: sockets, other_bound: other_sockets }
                    .remove_listener(
                        &DualStackUnspecifiedAddr,
                        identifier,
                        &device,
                        &paired_bound_socket_ids,
                    );
                let PairedBoundSocketIds { this: socket_id, other: other_socket_id } =
                    paired_bound_socket_ids;
                DualStackRemoveInfo(DualStackRemove::ListenerBothStacks {
                    identifier,
                    device,
                    associated_addr,
                    ip_options,
                    sharing,
                    socket_id,
                    other_socket_id,
                })
            }
        }
    }
}

struct DualStackRemoveInfo<I: IpExt, D: WeakId, S: DatagramSocketSpec>(DualStackRemove<I, D, S>);

impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> DualStackRemoveInfo<I, D, S> {
    fn into_options_and_info(self) -> (IpOptions<I, D, S>, SocketInfo<I, D, S>) {
        let DualStackRemoveInfo(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(single_stack_remove) => {
                SingleStackRemoveInfo(single_stack_remove).into_options_and_info()
            }
            DualStackRemove::ListenerOtherStack {
                concrete_addr: _,
                associated_addr,
                ip_options,
                sharing: _,
                socket_id: _,
            } => (ip_options, SocketInfo::Listener(associated_addr)),
            DualStackRemove::ListenerBothStacks {
                identifier: _,
                device: _,
                associated_addr,
                ip_options,
                sharing: _,
                socket_id: _,
                other_socket_id: _,
            } => (ip_options, SocketInfo::Listener(associated_addr)),
        }
    }

    /// Undo this removal by reinserting the socket into the `BoundSocketMap`s.
    ///
    /// # Panics
    ///
    /// Panics if the socket can not be inserted into the given maps (i.e. if it
    /// already exists.)
    fn reinsert(
        self,
        sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
        _other_sockets: &mut BoundSocketMap<
            I::OtherVersion,
            D,
            S::AddrSpec,
            S::SocketMapSpec<I::OtherVersion, D>,
        >,
    ) {
        let DualStackRemoveInfo(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(single_stack_remove) => {
                SingleStackRemoveInfo(single_stack_remove).reinsert(sockets);
            }
            _ => todo!("https://fxbug.dev/135204: Support dual-stack reinsertion"),
        }
    }
}

pub(crate) fn get_info<
    I: IpExt,
    S: DatagramSocketSpec,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
) -> SocketInfo<I, SC::WeakDeviceId, S> {
    sync_ctx.with_sockets_state(|_sync_ctx, state| {
        match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) => SocketInfo::Unbound,
            SocketState::Bound(BoundSocketState::Listener { state, sharing: _ }) => {
                let ListenerState { addr, ip_options: _ } = state;
                SocketInfo::Listener(addr.clone())
            }
            SocketState::Bound(BoundSocketState::Connected { state, sharing: _ }) => {
                SocketInfo::Connected(S::conn_addr_from_state(state))
            }
        }
    })
}

pub(crate) fn listen<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    addr: Option<SocketZonedIpAddr<I::Addr, SC::DeviceId>>,
    local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        listen_inner::<_, C, _, S>(sync_ctx, ctx, state, id, addr, local_id)
    })
}

/// Abstraction for operations over one or two demultiplexing maps.
trait BoundStateHandler<I: IpExt, S: DatagramSocketSpec, D: device::WeakId> {
    /// The type of address that can be inserted or removed for listeners.
    type ListenerAddr: Clone;
    /// The type of ID that can be inserted or removed.
    type BoundSocketId;

    /// Checks whether an entry could be inserted for the specified address and
    /// identifier.
    ///
    /// Returns `true` if a value could be inserted at the specified address and
    /// local ID, with the provided sharing state; otherwise returns `false`.
    fn is_listener_entry_available(
        &self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing_state: &S::SharingState,
    ) -> bool;

    /// Inserts `id` at a listener address or returns an error.
    ///
    /// Inserts the identifier `id` at the listener address for `addr` and
    /// local `identifier` with device `device` and the given sharing state. If
    /// the insertion conflicts with an existing socket, a `LocalAddressError`
    /// is returned.
    fn try_insert_listener(
        &mut self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: S::SharingState,
        id: Self::BoundSocketId,
    ) -> Result<(), LocalAddressError>;

    /// Removes `id` at listener address, assuming it exists.
    ///
    /// Panics if `id` does not exit.
    fn remove_listener(
        &mut self,
        addr: &Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: &Option<D>,
        id: &Self::BoundSocketId,
    );
}

/// A sentinel type for the unspecified address in a dual-stack context.
///
/// This is kind of like [`Ipv6::UNSPECIFIED_ADDRESS`], but makes it clear that
/// the value is being used in a dual-stack context.
#[derive(Copy, Clone, Debug)]
struct DualStackUnspecifiedAddr;

/// Implementation of BoundStateHandler for a single demultiplexing map.
impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> BoundStateHandler<I, S, D>
    for BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>
{
    type ListenerAddr = Option<SocketIpAddr<I::Addr>>;
    type BoundSocketId =
        <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId;
    fn is_listener_entry_available(
        &self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing: &S::SharingState,
    ) -> bool {
        let check_addr = ListenerAddr { device: None, ip: ListenerIpAddr { identifier, addr } };
        match self.listeners().could_insert(&check_addr, sharing) {
            Ok(()) => true,
            Err(
                InsertError::Exists
                | InsertError::IndirectConflict
                | InsertError::ShadowAddrExists
                | InsertError::ShadowerExists,
            ) => false,
        }
    }

    fn try_insert_listener(
        &mut self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: S::SharingState,
        id: Self::BoundSocketId,
    ) -> Result<(), LocalAddressError> {
        try_insert_single_listener(self, addr, identifier, device, sharing, id).map(|_entry| ())
    }

    fn remove_listener(
        &mut self,
        addr: &Self::ListenerAddr,
        identifier: <<S as DatagramSocketSpec>::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: &Option<D>,
        id: &Self::BoundSocketId,
    ) {
        remove_single_listener(self, addr, identifier, device, id)
    }
}

struct PairedSocketMapMut<'a, I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    bound: &'a mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
    other_bound: &'a mut BoundSocketMap<
        I::OtherVersion,
        D,
        S::AddrSpec,
        S::SocketMapSpec<I::OtherVersion, D>,
    >,
}

struct PairedBoundSocketIds<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    this: <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId,
    other: <S::SocketMapSpec<I::OtherVersion, D> as DatagramSocketMapSpec<
        I::OtherVersion,
        D,
        S::AddrSpec,
    >>::BoundSocketId,
}

/// Implementation for a pair of demultiplexing maps for different IP versions.
impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> BoundStateHandler<I, S, D>
    for PairedSocketMapMut<'_, I, D, S>
{
    type ListenerAddr = DualStackUnspecifiedAddr;
    type BoundSocketId = PairedBoundSocketIds<I, D, S>;

    fn is_listener_entry_available(
        &self,
        DualStackUnspecifiedAddr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing: &S::SharingState,
    ) -> bool {
        let PairedSocketMapMut { bound, other_bound } = self;
        BoundStateHandler::<I, S, D>::is_listener_entry_available(*bound, None, identifier, sharing)
            && BoundStateHandler::<I::OtherVersion, S, D>::is_listener_entry_available(
                *other_bound,
                None,
                identifier,
                sharing,
            )
    }

    fn try_insert_listener(
        &mut self,
        DualStackUnspecifiedAddr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: S::SharingState,
        id: Self::BoundSocketId,
    ) -> Result<(), LocalAddressError> {
        let PairedSocketMapMut { bound: this, other_bound: other } = self;
        let PairedBoundSocketIds { this: this_id, other: other_id } = id;
        try_insert_single_listener(this, None, identifier, device.clone(), sharing.clone(), this_id)
            .and_then(|first_entry| {
                match try_insert_single_listener(other, None, identifier, device, sharing, other_id)
                {
                    Ok(_second_entry) => Ok(()),
                    Err(e) => {
                        first_entry.remove();
                        Err(e)
                    }
                }
            })
    }

    fn remove_listener(
        &mut self,
        DualStackUnspecifiedAddr: &Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: &Option<D>,
        id: &PairedBoundSocketIds<I, D, S>,
    ) {
        let PairedSocketMapMut { bound: this, other_bound: other } = self;
        let PairedBoundSocketIds { this: this_id, other: other_id } = id;
        remove_single_listener(this, &None, identifier, device, this_id);
        remove_single_listener(other, &None, identifier, device, other_id);
    }
}

fn try_insert_single_listener<
    I: IpExt,
    D: device::WeakId,
    A: SocketMapAddrSpec,
    S: DatagramSocketMapSpec<I, D, A>,
>(
    bound: &mut BoundSocketMap<I, D, A, S>,
    addr: Option<SocketIpAddr<I::Addr>>,
    identifier: A::LocalIdentifier,
    device: Option<D>,
    sharing: S::ListenerSharingState,
    id: S::ListenerId,
) -> Result<socket::SocketStateEntry<'_, I, D, A, S, socket::Listener>, LocalAddressError> {
    bound
        .listeners_mut()
        .try_insert(ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device }, sharing, id)
        .map_err(|e| match e {
            (
                InsertError::ShadowAddrExists
                | InsertError::Exists
                | InsertError::IndirectConflict
                | InsertError::ShadowerExists,
                sharing,
            ) => {
                let _: S::ListenerSharingState = sharing;
                LocalAddressError::AddressInUse
            }
        })
}

fn remove_single_listener<
    I: IpExt,
    D: device::WeakId,
    A: SocketMapAddrSpec,
    S: DatagramSocketMapSpec<I, D, A>,
>(
    bound: &mut BoundSocketMap<I, D, A, S>,
    addr: &Option<SocketIpAddr<I::Addr>>,
    identifier: A::LocalIdentifier,
    device: &Option<D>,
    id: &S::ListenerId,
) {
    let addr =
        ListenerAddr { ip: ListenerIpAddr { addr: *addr, identifier }, device: device.clone() };
    bound
        .listeners_mut()
        .remove(id, &addr)
        .unwrap_or_else(|NotFoundError| panic!("socket ID {:?} not found for {:?}", id, addr))
}

fn try_pick_identifier<
    I: IpExt,
    S: DatagramSocketSpec,
    D: device::WeakId,
    BS: BoundStateHandler<I, S, D>,
    C: RngContext,
>(
    addr: BS::ListenerAddr,
    bound: &BS,
    ctx: &mut C,
    sharing: &S::SharingState,
) -> Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
    S::try_alloc_listen_identifier::<I, D>(ctx, move |identifier| {
        bound
            .is_listener_entry_available(addr.clone(), identifier, sharing)
            .then_some(())
            .ok_or(InUseError)
    })
}

fn try_pick_bound_address<I: IpExt, SC: TransportIpContext<I, C>, C, LI>(
    addr: Option<ZonedAddr<SocketIpAddr<I::Addr>, SC::DeviceId>>,
    device: &Option<SC::WeakDeviceId>,
    sync_ctx: &mut SC,
    identifier: LI,
) -> Result<
    (Option<SocketIpAddr<I::Addr>>, Option<EitherDeviceId<SC::DeviceId, SC::WeakDeviceId>>, LI),
    LocalAddressError,
> {
    let (addr, device, identifier) = match addr {
        Some(addr) => {
            // Extract the specified address and the device. The device
            // is either the one from the address or the one to which
            // the socket was previously bound.
            let (addr, device) = crate::transport::resolve_addr_with_device(addr, device.clone())?;

            // Binding to multicast addresses is allowed regardless.
            // Other addresses can only be bound to if they are assigned
            // to the device.
            if !addr.addr().is_multicast() {
                let mut assigned_to = TransportIpContext::<I, _>::get_devices_with_assigned_addr(
                    sync_ctx,
                    addr.into(),
                );
                if let Some(device) = &device {
                    if !assigned_to.any(|d| device == &EitherDeviceId::Strong(d)) {
                        return Err(LocalAddressError::AddressMismatch);
                    }
                } else {
                    if !assigned_to.any(|_: SC::DeviceId| true) {
                        return Err(LocalAddressError::CannotBindToAddress);
                    }
                }
            }
            (Some(addr), device, identifier)
        }
        None => (None, device.clone().map(EitherDeviceId::Weak), identifier),
    };
    Ok((addr, device, identifier))
}

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
enum TryUnmapResult<I: DualStackIpExt, D> {
    /// The address does not have an un-mapped representation.
    ///
    /// This spits back the input address unmodified.
    CannotBeUnmapped(ZonedAddr<SocketIpAddr<I::Addr>, D>),
    /// The address in the other stack that corresponds to the input.
    ///
    /// Since [`SocketZonedIpAddr`] is guaranteed to hold a specified address,
    /// this must hold an `Option<SocketZonedIpAddr>`. Since `::FFFF:0.0.0.0` is
    /// a legal IPv4-mapped IPv6 address, this allows us to represent it as the
    /// unspecified IPv4 address.
    Mapped(Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, D>>),
}

/// Try to convert a specified address into the address that maps to it from
/// the other stack.
///
/// This is an IP-generic function that tries to invert the
/// IPv4-to-IPv4-mapped-IPv6 conversion that is performed by
/// [`Ipv4Addr::to_ipv6_mapped`].
///
/// The only inputs that will produce [`TryUnmapResult::Mapped`] are
/// IPv4-mapped IPv6 addresses. All other inputs will produce
/// [`TryUnmapResult::CannotBeUnmapped`].
// TODO(https://fxbug.dev/21198): Move this onto a method on the dual-stack
// context trait so that it doesn't need map_ip.
fn try_unmap<A: IpAddress, D>(addr: SocketZonedIpAddr<A, D>) -> TryUnmapResult<A::Version, D>
where
    A::Version: DualStackIpExt,
{
    <A::Version as Ip>::map_ip(
        addr.into_inner(),
        |v4| {
            let addr = SocketIpAddr::new_ipv4_specified(v4.addr());
            TryUnmapResult::CannotBeUnmapped(ZonedAddr::Unzoned(addr))
        },
        |v6| match v6.addr().to_ipv4_mapped() {
            Some(v4) => {
                let addr = SpecifiedAddr::new(v4).map(SocketIpAddr::new_ipv4_specified);
                TryUnmapResult::Mapped(addr.map(ZonedAddr::Unzoned))
            }
            None => {
                let (addr, zone) = v6.into_addr_zone();
                let addr: SocketIpAddr<_> =
                    addr.try_into().unwrap_or_else(|AddrIsMappedError {}| {
                        unreachable!(
                            "addr cannot be mapped because `to_ipv4_mapped` returned `None`"
                        )
                    });
                TryUnmapResult::CannotBeUnmapped(ZonedAddr::new(addr, zone).unwrap_or_else(|| {
                    unreachable!("addr should still be scopeable after wrapping in `SocketIpAddr`")
                }))
            }
        },
    )
}

fn listen_inner<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramBoundStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    state: &mut SocketsState<I, SC::WeakDeviceId, S>,
    id: S::SocketId<I>,
    addr: Option<SocketZonedIpAddr<I::Addr, SC::DeviceId>>,
    local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
    /// Possible operations that might be performed, depending on whether the
    /// socket state spec supports dual-stack operation and what the address
    /// looks like.
    #[derive(Debug, GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    enum BoundOperation<'a, I: IpExt, DS: DeviceIdContext<AnyDevice>, NDS> {
        /// Bind to the "any" address on both stacks.
        DualStackAnyAddr(&'a mut DS),
        /// Bind to a non-dual-stack address only on the current stack.
        OnlyCurrentStack(
            MaybeDualStack<&'a mut DS, &'a mut NDS>,
            Option<ZonedAddr<SocketIpAddr<I::Addr>, DS::DeviceId>>,
        ),
        /// Bind to an address only on the other stack.
        OnlyOtherStack(
            &'a mut DS,
            Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, DS::DeviceId>>,
        ),
    }

    let mut entry = match state.entry(id.get_key_index()) {
        DenseMapEntry::Vacant(_) => panic!("unbound ID {:?} is invalid", id),
        DenseMapEntry::Occupied(o) => o,
    };
    let UnboundSocketState { device, sharing, ip_options } = match entry.get() {
        SocketState::Unbound(state) => state,
        SocketState::Bound(_) => return Err(Either::Left(ExpectedUnboundError)),
    };

    let dual_stack = sync_ctx.dual_stack_context();
    let bound_operation: BoundOperation<'_, I, _, _> = match (dual_stack, addr) {
        // Dual-stack support and unspecified address.
        (MaybeDualStack::DualStack(dual_stack), None) => {
            match dual_stack.dual_stack_enabled(ip_options) {
                // Socket is dual-stack enabled, bind in both stacks.
                true => BoundOperation::DualStackAnyAddr(dual_stack),
                // Dual-stack support but not enabled, so bind unspecified in the
                // current stack.
                false => {
                    BoundOperation::OnlyCurrentStack(MaybeDualStack::DualStack(dual_stack), None)
                }
            }
        }
        // There is dual-stack support and the address is not unspecified so how
        // to proceed is going to depend on the value of `addr`.
        (MaybeDualStack::DualStack(dual_stack), Some(addr)) => match try_unmap(addr) {
            // `addr` can't be represented in the other stack.
            TryUnmapResult::CannotBeUnmapped(addr) => {
                BoundOperation::OnlyCurrentStack(MaybeDualStack::DualStack(dual_stack), Some(addr))
            }
            // There's a representation in the other stack, so use that if possible.
            TryUnmapResult::Mapped(addr) => match dual_stack.dual_stack_enabled(ip_options) {
                true => BoundOperation::OnlyOtherStack(dual_stack, addr),
                false => return Err(Either::Right(LocalAddressError::CannotBindToAddress)),
            },
        },
        // No dual-stack support, so only bind on the current stack.
        (MaybeDualStack::NotDualStack(single_stack), None) => {
            BoundOperation::OnlyCurrentStack(MaybeDualStack::NotDualStack(single_stack), None)
        }
        // No dual-stack support, so check the address is allowed in the current
        // stack.
        (MaybeDualStack::NotDualStack(single_stack), Some(addr)) => match try_unmap(addr) {
            // The address is only representable in the current stack.
            TryUnmapResult::CannotBeUnmapped(addr) => BoundOperation::OnlyCurrentStack(
                MaybeDualStack::NotDualStack(single_stack),
                Some(addr),
            ),
            // The address has a representation in the other stack but there's
            // no dual-stack support!
            TryUnmapResult::Mapped(_addr) => {
                let _: Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, _>> = _addr;
                return Err(Either::Right(LocalAddressError::CannotBindToAddress));
            }
        },
    };

    fn try_bind_single_stack<
        I: IpExt,
        S: DatagramSocketSpec,
        SC: TransportIpContext<I, C>,
        C: RngContext,
    >(
        sync_ctx: &mut SC,
        ctx: &mut C,
        bound: &mut BoundSocketMap<
            I,
            SC::WeakDeviceId,
            S::AddrSpec,
            S::SocketMapSpec<I, SC::WeakDeviceId>,
        >,
        addr: Option<ZonedAddr<SocketIpAddr<I::Addr>, SC::DeviceId>>,
        device: &Option<SC::WeakDeviceId>,
        local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        id: <S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerId,
        sharing: S::SharingState,
    ) -> Result<
        ListenerAddr<
            ListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
            SC::WeakDeviceId,
        >,
        LocalAddressError,
    > {
        let identifier = match local_id {
            Some(id) => Some(id),
            None => try_pick_identifier::<I, S, _, _, _>(
                addr.as_ref().map(ZonedAddr::addr),
                bound,
                ctx,
                &sharing,
            ),
        }
        .ok_or(LocalAddressError::FailedToAllocateLocalPort)?;
        let (addr, device, identifier) =
            try_pick_bound_address::<I, _, _, _>(addr, device, sync_ctx, identifier)?;
        let weak_device = device.map(|d| d.as_weak(sync_ctx).into_owned());

        // TODO(https://fxbug.dev/132092): Delete conversion once `ZonedAddr`
        // holds `NonMappedAddr`.
        let addr =
            addr.map(SocketIpAddr::try_from).transpose().expect("ZonedAddr should be non-mapped");
        BoundStateHandler::<_, S, _>::try_insert_listener(
            bound,
            addr,
            identifier,
            weak_device.clone(),
            sharing,
            id,
        )
        .map(|()| ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device: weak_device })
    }

    let bound_addr: ListenerAddr<S::ListenerIpAddr<I>, SC::WeakDeviceId> = match bound_operation {
        BoundOperation::OnlyCurrentStack(either_dual_stack, addr) => {
            let converter = either_dual_stack.to_converter();
            sync_ctx
                .with_bound_sockets_mut(|sync_ctx, bound, _allocator| {
                    let id = S::make_bound_socket_map_id(id);

                    try_bind_single_stack::<I, S, _, _>(
                        sync_ctx,
                        ctx,
                        bound,
                        addr,
                        device,
                        local_id,
                        id,
                        sharing.clone(),
                    )
                })
                .map(|ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device }| {
                    let ip = match converter {
                        MaybeDualStack::DualStack(converter) => converter.convert_back(
                            DualStackListenerIpAddr::ThisStack(ListenerIpAddr { addr, identifier }),
                        ),
                        MaybeDualStack::NotDualStack(converter) => {
                            converter.convert_back(ListenerIpAddr { addr, identifier })
                        }
                    };
                    ListenerAddr { ip, device }
                })
        }
        BoundOperation::OnlyOtherStack(sync_ctx, addr) => {
            let id = sync_ctx.to_other_bound_socket_id(id);
            sync_ctx
                .with_other_bound_sockets_mut(|sync_ctx, other_bound| {
                    try_bind_single_stack::<_, S, _, _>(
                        sync_ctx,
                        ctx,
                        other_bound,
                        addr,
                        device,
                        local_id,
                        id,
                        sharing.clone(),
                    )
                })
                .map(|ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device }| {
                    ListenerAddr {
                        ip: sync_ctx.converter().convert_back(DualStackListenerIpAddr::OtherStack(
                            ListenerIpAddr { addr, identifier },
                        )),
                        device,
                    }
                })
        }
        BoundOperation::DualStackAnyAddr(sync_ctx) => {
            let ids = PairedBoundSocketIds {
                this: S::make_bound_socket_map_id(id.clone()),
                other: sync_ctx.to_other_bound_socket_id(id),
            };
            sync_ctx
                .with_both_bound_sockets_mut(
                    |sync_ctx, bound, other_bound, _alloc, _other_alloc| {
                        let mut bound_pair = PairedSocketMapMut { bound, other_bound };
                        let sharing = sharing.clone();

                        let identifier = match local_id {
                            Some(id) => Some(id),
                            None => try_pick_identifier::<I, S, _, _, _>(
                                DualStackUnspecifiedAddr,
                                &bound_pair,
                                ctx,
                                &sharing,
                            ),
                        }
                        .ok_or(LocalAddressError::FailedToAllocateLocalPort)?;
                        let (_addr, device, identifier) = try_pick_bound_address::<I, _, _, _>(
                            None, device, sync_ctx, identifier,
                        )?;
                        let weak_device = device.map(|d| d.as_weak(sync_ctx).into_owned());

                        BoundStateHandler::<_, S, _>::try_insert_listener(
                            &mut bound_pair,
                            DualStackUnspecifiedAddr,
                            identifier,
                            weak_device.clone(),
                            sharing,
                            ids,
                        )
                        .map(|()| (identifier, weak_device))
                    },
                )
                .map(|(identifier, device)| ListenerAddr {
                    ip: sync_ctx
                        .converter()
                        .convert_back(DualStackListenerIpAddr::BothStacks(identifier)),
                    device,
                })
        }
    }
    .map_err(Either::Right)?;

    // Replace the unbound state only after we're sure the
    // insertion has succeeded.
    *entry.get_mut() = SocketState::Bound(BoundSocketState::Listener {
        state: ListenerState {
            // TODO(https://fxbug.dev/131970): Remove this clone().
            ip_options: ip_options.clone(),
            addr: bound_addr,
        },
        sharing: sharing.clone(),
    });
    Ok(())
}

/// An error when attempting to create a datagram socket.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConnectError {
    /// An error was encountered creating an IP socket.
    #[error("{}", _0)]
    Ip(#[from] IpSockCreationError),
    /// No local port was specified, and none could be automatically allocated.
    #[error("a local port could not be allocated")]
    CouldNotAllocateLocalPort,
    /// The specified socket addresses (IP addresses and ports) conflict with an
    /// existing socket.
    #[error("the socket's IP address and port conflict with an existing socket")]
    SockAddrConflict,
    /// There was a problem with the provided address relating to its zone.
    #[error("{}", _0)]
    Zone(#[from] ZonedAddressError),
}

pub(crate) fn connect<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, SC::DeviceId>>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    extra: S::ConnStateExtra,
) -> Result<(), ConnectError> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let mut entry = match state.entry(id.get_key_index()) {
            DenseMapEntry::Vacant(_) => panic!("socket ID {:?} is invalid", id),
            DenseMapEntry::Occupied(o) => o,
        };

        let (local_ip, local_id, sharing, device, ip_options) = match entry.get() {
            SocketState::Unbound(state) => {
                let UnboundSocketState { device, sharing: unbound_sharing, ip_options } = state;
                (None, None, unbound_sharing.clone(), device.as_ref(), ip_options)
            }
            SocketState::Bound(state) => match state {
                BoundSocketState::Listener { state, sharing: listener_sharing } => {
                    let ListenerState { ip_options, addr: ListenerAddr { device, ip } } = state;
                    let ip = match sync_ctx.dual_stack_context() {
                        MaybeDualStack::DualStack(dual_stack) => {
                            match dual_stack.converter().convert(ip.clone()) {
                                DualStackListenerIpAddr::OtherStack(_)  => {
                                    dual_stack.assert_dual_stack_enabled(state);
                                    todo!("https://fxbug.dev/21198: connecting dual-stack sockets")
                                }
                                DualStackListenerIpAddr::BothStacks(identifier)=> {
                                    // TODO(https://fxbug.dev/135204): Use the
                                    // `remote_ip` to dictate which stack to
                                    // connect to.
                                    dual_stack.assert_dual_stack_enabled(state);
                                    ListenerIpAddr{addr: None, identifier}
                                }
                                DualStackListenerIpAddr::ThisStack(addr) => addr,
                            }
                        }
                        MaybeDualStack::NotDualStack(single_stack) => {
                            single_stack.converter().convert(ip.clone())
                        }
                    };
                    let ListenerIpAddr { addr, identifier } = ip;
                    (
                        addr,
                        Some(identifier),
                        listener_sharing.clone(),
                        device.as_ref(),
                        ip_options,
                    )
                }
                BoundSocketState::Connected { state, sharing: conn_sharing } => {
                    let ConnState {
                        socket,
                        clear_device_on_disconnect,
                        shutdown: _,
                        addr: ConnAddr {
                                ip: ConnIpAddr { local: (ip, identifier), remote: _ },
                                device,
                            },
                        extra: _,
                    } = match sync_ctx.dual_stack_context() {
                        MaybeDualStack::DualStack(dual_stack) => {
                            match dual_stack.converter().convert(state) {
                                DualStackConnState::ThisStack(state) => state,
                                DualStackConnState::OtherStack(state) => {
                                    dual_stack.assert_dual_stack_enabled(state);
                                    todo!("https://fxbug.dev/21198: Support dual-stack connect");
                                }
                            }
                        }
                        MaybeDualStack::NotDualStack(not_dual_stack) => {
                            not_dual_stack.converter().convert(state)
                        }
                    };
                    (
                        Some(*ip),
                        Some(*identifier),
                        conn_sharing.clone(),
                        device.as_ref().and_then(|d| (!clear_device_on_disconnect).then_some(d)),
                        socket.options(),
                    )
                }
            },
        };
        let remote_ip = crate::socket::specify_unspecified_remote::<I, _>(remote_ip);
        let (remote_ip, socket_device) =
            crate::transport::resolve_addr_with_device::<I::Addr, _, _, _>(
                remote_ip.into_inner(),
                device.cloned(),
            )?;

        // TODO(https://fxbug.dev/21198): Support dual-stack connect.
        let remote_ip = match remote_ip.try_into() {
            Ok(ip) => ip,
            Err(AddrIsMappedError {}) => {
                return Err(ConnectError::Ip(IpSockCreationError::Route(
                    ResolveRouteError::Unreachable,
                )));
            }
        };

        let clear_device_on_disconnect = device.is_none() && socket_device.is_some();

        let (mut ip_sock, bound_addr) = match sync_ctx.dual_stack_context() {
            // TODO(https://fxbug.dev/135204): Support dual-stack connect and
            // deduplicate these two match arms.
            MaybeDualStack::DualStack(ds) => {
                let remove_original_socket_op = match entry.get() {
                    SocketState::Unbound(_) => None,
                    SocketState::Bound(state) => Some(
                        DualStackRemoveOperation::parse_from_state(ds, id.clone(), state)
                    )
                };
                ds.with_both_bound_sockets_mut(
                    |sync_ctx, bound, other_bound, alloc, _other_alloc| {
                        let ip_sock = IpSocketHandler::<I, _>::new_ip_socket(
                            sync_ctx,
                            ctx,
                            socket_device.as_ref().map(|d| d.as_ref()),
                            local_ip.map(SocketIpAddr::into),
                            remote_ip,
                            S::ip_proto::<I>(),
                            Default::default(),
                        )
                        .map_err(|(e, _ip_options)| e)?;
                        let local_id = match local_id {
                            Some(id) => id.clone(),
                            None => alloc
                                .try_alloc_local_id(
                                    bound,
                                    ctx,
                                    DatagramFlowId {
                                        local_ip: *ip_sock.local_ip(),
                                        remote_ip: *ip_sock.remote_ip(),
                                        remote_id: remote_id.clone(),
                                    },
                                )
                                .ok_or(ConnectError::CouldNotAllocateLocalPort)?,
                        };
                        let conn_addr = ConnAddr {
                            ip: ConnIpAddr {
                                local: (*ip_sock.local_ip(), local_id),
                                remote: (*ip_sock.remote_ip(), remote_id),
                            },
                            device: ip_sock.device().cloned(),
                        };
                        // Now that all the other checks have been done,
                        // actually remove the ID from the socket map.
                        let removed_bound_socket = remove_original_socket_op.map(
                            |op| op.apply(bound, other_bound));
                        // Try to insert the new connection, restoring the
                        // original state on failure.
                        let bound_addr = match bound.conns_mut().try_insert(
                            conn_addr,
                            sharing.clone(),
                            S::make_bound_socket_map_id(id.clone()),
                        ) {
                            Ok(bound_entry) => bound_entry.get_addr().clone(),
                            Err((
                                InsertError::Exists
                                | InsertError::IndirectConflict
                                | InsertError::ShadowerExists
                                | InsertError::ShadowAddrExists,
                                _sharing,
                            )) => {
                                if let Some(removed_bound_socket) = removed_bound_socket {
                                    removed_bound_socket.reinsert(bound, other_bound);
                                }
                                return Err(ConnectError::SockAddrConflict)
                            }
                        };
                        Ok((ip_sock, bound_addr))
                    }
                )?
            }
            MaybeDualStack::NotDualStack(nds) => {
                let remove_original_socket_op = match entry.get() {
                    SocketState::Unbound(_) => None,
                    SocketState::Bound(state) => Some(
                        SingleStackRemoveOperation::parse_from_state(nds, id.clone(), state)
                    )
                };
                DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                    sync_ctx,
                    |sync_ctx, bound, alloc| {
                        let ip_sock = IpSocketHandler::<I, _>::new_ip_socket(
                            sync_ctx,
                            ctx,
                            socket_device.as_ref().map(|d| d.as_ref()),
                            local_ip.map(SocketIpAddr::into),
                            remote_ip,
                            S::ip_proto::<I>(),
                            Default::default(),
                        )
                        .map_err(|(e, _ip_options)| e)?;
                        let local_id = match local_id {
                            Some(id) => id.clone(),
                            None => alloc
                                .try_alloc_local_id(
                                    bound,
                                    ctx,
                                    DatagramFlowId {
                                        local_ip: *ip_sock.local_ip(),
                                        remote_ip: *ip_sock.remote_ip(),
                                        remote_id: remote_id.clone(),
                                    },
                                )
                                .ok_or(ConnectError::CouldNotAllocateLocalPort)?,
                        };
                        let conn_addr = ConnAddr {
                            ip: ConnIpAddr {
                                local: (*ip_sock.local_ip(), local_id),
                                remote: (*ip_sock.remote_ip(), remote_id),
                            },
                            device: ip_sock.device().cloned(),
                        };
                        // Now that all the other checks have been done,
                        // actually remove the ID from the socket map.
                        let removed_bound_socket = remove_original_socket_op.map(
                            |op| op.apply(bound));
                        // Try to insert the new connection, restoring the
                        // original state on failure.
                        let bound_addr = match bound.conns_mut().try_insert(
                            conn_addr,
                            sharing.clone(),
                            S::make_bound_socket_map_id(id.clone()),
                        ) {
                            Ok(bound_entry) => bound_entry.get_addr().clone(),
                            Err((
                                InsertError::Exists
                                | InsertError::IndirectConflict
                                | InsertError::ShadowerExists
                                | InsertError::ShadowAddrExists,
                                _sharing,
                            )) => {
                                if let Some(removed_bound_socket) = removed_bound_socket {
                                    removed_bound_socket.reinsert(bound);
                                }
                                return Err(ConnectError::SockAddrConflict)
                            }
                        };
                        Ok((ip_sock, bound_addr))
                    }
                )?
            }
        };

        // Update the bound state after there's no further path for errors.
        // TODO(https://fxbug.dev/131970): Remove this clone.
        *ip_sock.options_mut() = ip_options.clone();
        let conn_state = ConnState {
            socket: ip_sock,
            clear_device_on_disconnect,
            shutdown: Shutdown::default(),
            addr: bound_addr,
            extra,
        };
        *entry.get_mut() = SocketState::Bound(BoundSocketState::Connected {
            state: match sync_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    dual_stack.converter().convert_back(DualStackConnState::ThisStack(conn_state))
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    not_dual_stack.converter().convert_back(conn_state)
                }
            },
            sharing,
        });
        Ok(())
    })
}

/// A connected socket was expected.
#[derive(Copy, Clone, Debug, Default, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub struct ExpectedConnError;

/// An unbound socket was expected.
#[derive(Copy, Clone, Debug, Default, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub struct ExpectedUnboundError;

pub(crate) fn disconnect_connected<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
) -> Result<(), ExpectedConnError> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let mut entry = match state.entry(id.get_key_index()) {
            DenseMapEntry::Vacant(_) => panic!("unbound ID {:?} is invalid", id),
            DenseMapEntry::Occupied(o) => o,
        };
        let (conn_state, sharing) = match entry.get_mut() {
            SocketState::Unbound(_)
            | SocketState::Bound(BoundSocketState::Listener { state: _, sharing: _ }) => {
                return Err(ExpectedConnError)
            }
            SocketState::Bound(BoundSocketState::Connected { state, sharing }) => (state, sharing),
        };

        let ConnState { socket, clear_device_on_disconnect, shutdown: _, addr, extra: _ } =
            match sync_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    match dual_stack.converter().convert(conn_state) {
                        DualStackConnState::ThisStack(conn_state) => conn_state,
                        DualStackConnState::OtherStack(conn_state) => {
                            dual_stack.assert_dual_stack_enabled(conn_state);
                            todo!(
                                "https://fxbug.dev/21198: Support dual-stack disconnect connected"
                            );
                        }
                    }
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    not_dual_stack.converter().convert(conn_state)
                }
            };
        let dual_stack_context = sync_ctx.dual_stack_context();
        let make_listener_ip = match dual_stack_context {
            MaybeDualStack::DualStack(dual_stack) => {
                if dual_stack.dual_stack_enabled(socket.options()) {
                    todo!("https://fxbug.dev/21198: Support disconnecting dual-stack sockets")
                }
                let converter = dual_stack.converter();
                MaybeDualStack::DualStack(move |ListenerIpAddr { addr, identifier }| {
                    converter.convert_back(DualStackListenerIpAddr::ThisStack(ListenerIpAddr {
                        addr,
                        identifier,
                    }))
                })
            }
            MaybeDualStack::NotDualStack(non_dual) => {
                let converter = non_dual.converter();
                MaybeDualStack::NotDualStack(move |ip| converter.convert_back(ip))
            }
        };

        let id = S::make_bound_socket_map_id(id);
        let bound_addr = DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
            sync_ctx,
            |_sync_ctx, bound, _allocator| {
                let _bound_addr =
                    bound.conns_mut().remove(&id, &addr).expect("connection not found");

                let ConnAddr {
                    ip: ConnIpAddr { local: (local_ip, identifier), remote: _ },
                    mut device,
                } = addr.clone();
                if *clear_device_on_disconnect {
                    device = None
                }

                let addr = ListenerAddr {
                    ip: ListenerIpAddr { addr: Some(local_ip), identifier },
                    device,
                };

                let bound_entry = bound
                    .listeners_mut()
                    .try_insert(addr, sharing.clone(), id)
                    .expect("inserting listener for disconnected socket failed");
                let ListenerAddr { ip, device } = bound_entry.get_addr().clone();
                let ip = match make_listener_ip {
                    MaybeDualStack::DualStack(f) => f(ip),
                    MaybeDualStack::NotDualStack(f) => f(ip),
                };
                ListenerAddr { ip, device }
            },
        );

        // TODO(https://fxbug.dev/131970): Remove this clone.
        let ip_options = socket.options().clone();
        *entry.get_mut() = SocketState::Bound(BoundSocketState::Listener {
            state: ListenerState { ip_options, addr: bound_addr },
            sharing: sharing.clone(),
        });
        Ok(())
    })
}

/// Which direction(s) to shut down for a socket.
#[derive(Copy, Clone, Debug, GenericOverIp)]
#[generic_over_ip()]
pub enum ShutdownType {
    /// Prevent sending packets on the socket.
    Send,
    /// Prevent receiving packets on the socket.
    Receive,
    /// Prevent sending and receiving packets on the socket.
    SendAndReceive,
}

pub(crate) fn shutdown_connected<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
    which: ShutdownType,
) -> Result<(), ExpectedConnError> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let state = match state.get_mut(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_)
            | SocketState::Bound(BoundSocketState::Listener { state: _, sharing: _ }) => {
                return Err(ExpectedConnError)
            }
            SocketState::Bound(BoundSocketState::Connected { state, sharing: _ }) => state,
        };
        let ConnState { socket: _, clear_device_on_disconnect: _, shutdown, addr: _, extra: _ } =
            match sync_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    match dual_stack.converter().convert(state) {
                        DualStackConnState::ThisStack(state) => state,
                        DualStackConnState::OtherStack(state) => {
                            dual_stack.assert_dual_stack_enabled(state);
                            todo!("https://fxbug.dev/21198: Support dual-stack shutdown connected");
                        }
                    }
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    not_dual_stack.converter().convert(state)
                }
            };
        let (shutdown_send, shutdown_receive) = match which {
            ShutdownType::Send => (true, false),
            ShutdownType::Receive => (false, true),
            ShutdownType::SendAndReceive => (true, true),
        };
        let Shutdown { send, receive } = shutdown;
        *send |= shutdown_send;
        *receive |= shutdown_receive;
        Ok(())
    })
}

pub(crate) fn get_shutdown_connected<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
) -> Option<ShutdownType> {
    sync_ctx.with_sockets_state(|sync_ctx, state| {
        let state = match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_)
            | SocketState::Bound(BoundSocketState::Listener { state: _, sharing: _ }) => {
                return None
            }
            SocketState::Bound(BoundSocketState::Connected { state, sharing: _ }) => state,
        };

        let ConnState { socket: _, clear_device_on_disconnect: _, shutdown, addr: _, extra: _ } =
            match sync_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    match dual_stack.converter().convert(state) {
                        DualStackConnState::ThisStack(state) => state,
                        DualStackConnState::OtherStack(state) => {
                            dual_stack.assert_dual_stack_enabled(state);
                            todo!(
                                "https://fxbug.dev/21198: Support dual-stack get shutdown connected"
                            );
                        }
                    }
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    not_dual_stack.converter().convert(state)
                }
            };
        let Shutdown { send, receive } = shutdown;
        Some(match (send, receive) {
            (false, false) => return None,
            (true, false) => ShutdownType::Send,
            (false, true) => ShutdownType::Receive,
            (true, true) => ShutdownType::SendAndReceive,
        })
    })
}

/// Error encountered when sending a datagram on a socket.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip()]
pub enum SendError<SE> {
    /// The socket is not connected,
    NotConnected,
    /// The socket is not writeable.
    NotWriteable,
    /// There was a problem sending the IP packet.
    IpSock(IpSockSendError),
    /// There was a problem when serializing the packet.
    SerializeError(SE),
}

pub(crate) fn send_conn<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: BufferDatagramStateContext<I, C, S, B>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    body: B,
) -> Result<(), SendError<S::SerializeError>> {
    sync_ctx.with_sockets_state_buf(|sync_ctx, state| {
        let state = match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_)
            | SocketState::Bound(BoundSocketState::Listener { state: _, sharing: _ }) => {
                return Err(SendError::NotConnected)
            }
            SocketState::Bound(BoundSocketState::Connected { state, sharing: _ }) => state,
        };

        let ConnState {
            socket,
            clear_device_on_disconnect: _,
            shutdown: Shutdown { send: shutdown_send, receive: _ },
            addr,
            extra: _,
        } = match sync_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(dual_stack) => match dual_stack.converter().convert(state) {
                DualStackConnState::ThisStack(state) => state,
                DualStackConnState::OtherStack(state) => {
                    dual_stack.assert_dual_stack_enabled(state);
                    todo!("https://fxbug.dev/21198: Support dual-stack send connected");
                }
            },
            MaybeDualStack::NotDualStack(not_dual_stack) => {
                not_dual_stack.converter().convert(state)
            }
        };
        if *shutdown_send {
            return Err(SendError::NotWriteable);
        }

        let ConnAddr { ip, device: _ } = addr;

        let packet = S::make_packet::<I, _>(body, &ip).map_err(SendError::SerializeError)?;
        sync_ctx.with_transport_context_buf(|sync_ctx| {
            sync_ctx
                .send_ip_packet(ctx, &socket, packet, None)
                .map_err(|(_serializer, send_error)| SendError::IpSock(send_error))
        })
    })
}

/// An error encountered while sending a datagram packet to an alternate address.
#[derive(Debug)]
pub enum SendToError<SE> {
    /// The socket is not writeable.
    NotWriteable,
    /// There was a problem with the remote address relating to its zone.
    Zone(ZonedAddressError),
    /// An error was encountered while trying to create a temporary IP socket
    /// to use for the send operation.
    CreateAndSend(IpSockCreateAndSendError),
    /// The remote address is mapped (i.e. an ipv4-mapped-ipv6 address), but the
    /// socket is not dual-stack enabled.
    RemoteUnexpectedlyMapped,
    /// The provided buffer is not vailid.
    SerializeError(SE),
}

pub(crate) fn send_to<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: BufferDatagramStateContext<I, C, S, B>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, SC::DeviceId>>,
    remote_identifier: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    body: B,
) -> Result<(), Either<LocalAddressError, SendToError<S::SerializeError>>> {
    sync_ctx.with_sockets_state_mut_buf(|sync_ctx, state| {
        match listen_inner(sync_ctx, ctx, state, id.clone(), None, None) {
            Ok(()) | Err(Either::Left(ExpectedUnboundError)) => (),
            Err(Either::Right(e)) => return Err(Either::Left(e)),
        };
        let state = match state.get(id.get_key_index()).expect("no such socket") {
            SocketState::Unbound(_) => panic!("expected bound socket"),
            SocketState::Bound(state) => state,
        };
        let (local, device, ip_options) = match state {
            BoundSocketState::Connected { state, sharing: _ } => {
                let ConnState {
                    socket,
                    clear_device_on_disconnect: _,
                    shutdown,
                    addr: ConnAddr { ip: ConnIpAddr { local, remote: _ }, device },
                    extra: _,
                } = match sync_ctx.dual_stack_context() {
                    MaybeDualStack::DualStack(dual_stack) => {
                        match dual_stack.converter().convert(state) {
                            DualStackConnState::ThisStack(state) => state,
                            DualStackConnState::OtherStack(state) => {
                                dual_stack.assert_dual_stack_enabled(&state);
                                todo!("https://fxbug.dev/21198: Support dual-stack send_to");
                            }
                        }
                    }
                    MaybeDualStack::NotDualStack(not_dual_stack) => {
                        not_dual_stack.converter().convert(state)
                    }
                };

                let Shutdown { send: shutdown_write, receive: _ } = shutdown;
                if *shutdown_write {
                    return Err(Either::Right(SendToError::NotWriteable));
                }
                let (local_ip, local_id) = local;

                ((Some(*local_ip), local_id.clone()), device, socket.options())
            }
            BoundSocketState::Listener { state, sharing: _ } => {
                let ListenerState { ip_options, addr: ListenerAddr { ip, device } } = state;

                let (local_ip, local_port) = match sync_ctx.dual_stack_context() {
                    MaybeDualStack::DualStack(dual_stack) => {
                        match dual_stack.converter().convert(ip.clone()) {
                            DualStackListenerIpAddr::ThisStack(ListenerIpAddr {
                                addr,
                                identifier,
                            }) => (addr, identifier),
                            DualStackListenerIpAddr::BothStacks(identifier) => {
                                // TODO(https://fxbug.dev/135204): Use the
                                // `remote_ip` to dictate which stack to send
                                // to.
                                dual_stack.assert_dual_stack_enabled(state);
                                (None, identifier)
                            }
                            DualStackListenerIpAddr::OtherStack(_) => {
                                dual_stack.assert_dual_stack_enabled(state);
                                // To implement: perform a one-shot send on the
                                // other IP stack's transport context.
                                todo!("https://fxbug.dev/21198: implement dual-stack send")
                            }
                        }
                    }
                    MaybeDualStack::NotDualStack(non_dual) => {
                        let ListenerIpAddr { addr, identifier } =
                            non_dual.converter().convert(ip.clone());
                        (addr, identifier)
                    }
                };

                ((local_ip, local_port), device, ip_options)
            }
        };

        let remote_ip = crate::socket::specify_unspecified_remote::<I, _>(remote_ip);

        // TODO(https://fxbug.dev/21198): For now, short circuit when asked to
        // send to a mapped remote address, which prevents panics further in the
        // send pipeline when converting remote to a `SocketIpAddr`.
        //
        // This should eventually translate the remote to its non-mapped form,
        // or fail if the socket doesn't support dual-stack operations.
        match NonMappedAddr::new(remote_ip.deref().addr()) {
            Some(_addr) => {}
            None => {
                // Prevent sending to ipv4-mapped-ipv6 addrs for now.
                return Err(Either::Right(SendToError::RemoteUnexpectedlyMapped));
            }
        }

        sync_ctx
            .with_transport_context_buf(|sync_ctx| {
                send_oneshot::<_, S, _, _, _>(
                    sync_ctx,
                    ctx,
                    local,
                    remote_ip,
                    remote_identifier,
                    device,
                    ip_options,
                    body,
                )
            })
            .map_err(Either::Right)
    })
}

fn send_oneshot<
    I: IpExt,
    S: DatagramSocketSpec,
    SC: BufferIpSocketHandler<I, C, B>,
    C,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    (local_ip, local_id): (
        Option<SocketIpAddr<I::Addr>>,
        <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
    ),
    remote_ip: SocketZonedIpAddr<I::Addr, SC::DeviceId>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    device: &Option<SC::WeakDeviceId>,
    ip_options: &IpOptions<I, SC::WeakDeviceId, S>,
    body: B,
) -> Result<(), SendToError<S::SerializeError>> {
    let (remote_ip, device) = match crate::transport::resolve_addr_with_device::<I::Addr, _, _, _>(
        remote_ip.into_inner(),
        device.clone(),
    ) {
        Ok(addr) => addr,
        Err(e) => return Err(SendToError::Zone(e)),
    };

    // TODO(https://fxbug.dev/132092): Delete conversion once
    // `SocketZonedIpAddr` holds `SocketIpAddr`.
    let remote_ip: SocketIpAddr<_> = remote_ip.try_into().unwrap();

    sync_ctx
        .send_oneshot_ip_packet_with_fallible_serializer(
            ctx,
            device.as_ref().map(|d| d.as_ref()),
            local_ip,
            remote_ip,
            S::ip_proto::<I>(),
            ip_options,
            |local_ip| {
                S::make_packet::<I, _>(
                    body,
                    &ConnIpAddr { local: (local_ip, local_id), remote: (remote_ip, remote_id) },
                )
            },
            None,
        )
        .map_err(|err| match err {
            SendOneShotIpPacketError::CreateAndSendError { err, options: _ } => {
                SendToError::CreateAndSend(err)
            }
            SendOneShotIpPacketError::SerializeError(err) => SendToError::SerializeError(err),
        })
}

pub(crate) fn set_device<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    new_device: Option<&SC::DeviceId>,
) -> Result<(), SocketError> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        match state.get_mut(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(state) => {
                let UnboundSocketState { ref mut device, sharing: _, ip_options: _ } = state;
                *device = new_device.map(|d| sync_ctx.downgrade_device_id(d));
                Ok(())
            }
            SocketState::Bound(state) => match state {
                BoundSocketState::Listener { state, sharing: _ } => {
                    // Don't allow changing the device if one of the IP addresses in the socket
                    // address vector requires a zone (scope ID).
                    let ListenerState { ip_options, addr } = state;
                    let ListenerAddr { device: old_device, ip } = addr;
                    let ip = match sync_ctx.dual_stack_context() {
                        MaybeDualStack::DualStack(dual_stack) => {
                            match dual_stack.converter().convert(ip.clone()) {
                                DualStackListenerIpAddr::ThisStack(addr) => addr,
                                DualStackListenerIpAddr::OtherStack(_)
                                | DualStackListenerIpAddr::BothStacks(_) => {
                                    dual_stack.assert_dual_stack_enabled(ip_options);
                                    todo!(
                                        "https://fxbug.dev/21198: implement dual-stack set_device"
                                    )
                                }
                            }
                        }
                        MaybeDualStack::NotDualStack(single_stack) => {
                            single_stack.converter().convert(ip.clone())
                        }
                    };

                    let ListenerIpAddr { addr: ip_addr, identifier: _ } = &ip;
                    // TODO(https://fxbug.dev/21198): Use the real
                    // possibly-dual-stack addr.
                    let bound_addr = ListenerAddr { ip: ip.clone(), device: old_device.clone() };

                    if !socket::can_device_change(
                        ip_addr.as_ref().map(|a| a.as_ref()), /* local_ip */
                        None,                                 /* remote_ip */
                        old_device.as_ref(),
                        new_device,
                    ) {
                        return Err(SocketError::Local(LocalAddressError::Zone(
                            ZonedAddressError::DeviceZoneMismatch,
                        )));
                    }

                    let new_addr = DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                        sync_ctx,
                        |sync_ctx, bound, _allocator| {
                            let entry = bound
                                .listeners_mut()
                                .entry(&S::make_bound_socket_map_id(id.clone()), &bound_addr)
                                .unwrap_or_else(|| panic!("invalid listener ID {:?}", id));
                            let new_addr = ListenerAddr {
                                device: new_device.map(|d| sync_ctx.downgrade_device_id(d)),
                                ..bound_addr.clone()
                            };
                            let new_entry = entry.try_update_addr(new_addr).map_err(
                                |(ExistsError {}, _entry)| LocalAddressError::AddressInUse,
                            )?;
                            Ok::<_, LocalAddressError>(new_entry.get_addr().clone())
                        },
                    )?;

                    let new_addr = {
                        let ListenerAddr { device, ip } = new_addr;

                        let new_ip = match sync_ctx.dual_stack_context().to_converter() {
                            MaybeDualStack::DualStack(converter) => {
                                let ListenerIpAddr { addr, identifier } = ip;
                                converter.convert_back(DualStackListenerIpAddr::ThisStack(
                                    ListenerIpAddr { addr, identifier },
                                ))
                            }
                            MaybeDualStack::NotDualStack(converter) => converter.convert_back(ip),
                        };

                        ListenerAddr { ip: new_ip, device }
                    };

                    *addr = new_addr;
                    Ok(())
                }
                BoundSocketState::Connected { state, sharing: _ } => {
                    let ConnState {
                        socket,
                        clear_device_on_disconnect,
                        shutdown: _,
                        addr,
                        extra: _,
                    } = match sync_ctx.dual_stack_context() {
                        MaybeDualStack::DualStack(dual_stack) => {
                            match dual_stack.converter().convert(state) {
                                DualStackConnState::ThisStack(state) => state,
                                DualStackConnState::OtherStack(state) => {
                                    dual_stack.assert_dual_stack_enabled(state);
                                    todo!(
                                        "https://fxbug.dev/21198: Support dual-stack connected \
                                            set_device"
                                    );
                                }
                            }
                        }
                        MaybeDualStack::NotDualStack(not_dual_stack) => {
                            not_dual_stack.converter().convert(state)
                        }
                    };
                    let ConnAddr { device: old_device, ip } = addr;
                    let ConnIpAddr { local: (local_ip, _), remote: (remote_ip, _) } = *ip;
                    if !socket::can_device_change(
                        Some(local_ip.as_ref()),
                        Some(remote_ip.as_ref()),
                        old_device.as_ref(),
                        new_device,
                    ) {
                        return Err(SocketError::Local(LocalAddressError::Zone(
                            ZonedAddressError::DeviceZoneMismatch,
                        )));
                    }

                    let (new_addr, mut new_socket) =
                        DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                            sync_ctx,
                            |sync_ctx, bound, _allocator| {
                                let new_socket = sync_ctx
                                    .new_ip_socket(
                                        ctx,
                                        new_device.map(EitherDeviceId::Strong),
                                        Some(local_ip),
                                        remote_ip,
                                        socket.proto(),
                                        Default::default(),
                                    )
                                    .map_err(|_: (IpSockCreationError, IpOptions<_, _, _>)| {
                                        SocketError::Remote(RemoteAddressError::NoRoute)
                                    })?;

                                let entry = bound
                                    .conns_mut()
                                    .entry(&S::make_bound_socket_map_id(id.clone()), addr)
                                    .unwrap_or_else(|| panic!("invalid conn ID {:?}", id));
                                let new_addr = ConnAddr {
                                    device: new_socket.device().cloned(),
                                    ..addr.clone()
                                };

                                let entry = match entry.try_update_addr(new_addr) {
                                    Err((ExistsError, _entry)) => {
                                        return Err(SocketError::Local(
                                            LocalAddressError::AddressInUse,
                                        ))
                                    }
                                    Ok(entry) => entry,
                                };
                                Ok((entry.get_addr().clone(), new_socket))
                            },
                        )?;
                    // Since the move was successful, replace the old socket with
                    // the new one but move the options over.
                    let _: IpOptions<_, _, _> = new_socket.replace_options(socket.take_options());
                    *socket = new_socket;
                    *addr = new_addr;

                    // If this operation explicitly sets the device for the socket, it
                    // should no longer be cleared on disconnect.
                    if new_device.is_some() {
                        *clear_device_on_disconnect = false;
                    }
                    Ok(())
                }
            },
        }
    })
}

pub(crate) fn get_bound_device<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
) -> Option<SC::WeakDeviceId> {
    sync_ctx.with_sockets_state(|sync_ctx, state| {
        let (_, device): (&IpOptions<_, _, _>, _) =
            get_options_device(sync_ctx, state.get(id.get_key_index()).expect("missing socket"));
        device.clone()
    })
}

/// Error resulting from attempting to change multicast membership settings for
/// a socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SetMulticastMembershipError {
    /// The provided address does not match the provided device.
    AddressNotAvailable,
    /// The device does not exist.
    DeviceDoesNotExist,
    /// The provided address does not match any address on the host.
    NoDeviceWithAddress,
    /// No device or address was specified and there is no device with a route
    /// to the multicast address.
    NoDeviceAvailable,
    /// Tried to join a group again.
    GroupAlreadyJoined,
    /// Tried to leave an unjoined group.
    GroupNotJoined,
    /// The socket is bound to a device that doesn't match the one specified.
    WrongDevice,
}

/// Selects the interface for the given remote address, optionally with a
/// constraint on the source address.
fn pick_interface_for_addr<
    A: IpAddress,
    S: DatagramSocketSpec,
    C: DatagramStateNonSyncContext<A::Version, S>,
    SC: DatagramBoundStateContext<A::Version, C, S>,
>(
    sync_ctx: &mut SC,
    _remote_addr: MulticastAddr<A>,
    source_addr: Option<SpecifiedAddr<A>>,
) -> Result<SC::DeviceId, SetMulticastMembershipError>
where
    A::Version: IpExt,
{
    sync_ctx.with_transport_context(|sync_ctx| {
        if let Some(source_addr) = source_addr {
            let mut devices = TransportIpContext::<A::Version, _>::get_devices_with_assigned_addr(
                sync_ctx,
                source_addr,
            );
            if let Some(d) = devices.next() {
                if devices.next() == None {
                    return Ok(d);
                }
            }
        }
        log_unimplemented!((), "https://fxbug.dev/39479: Implement this by looking up a route");
        Err(SetMulticastMembershipError::NoDeviceAvailable)
    })
}

/// Selector for the device to affect when changing multicast settings.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub enum MulticastInterfaceSelector<A: IpAddress, D> {
    /// Use the device with the assigned address.
    LocalAddress(SpecifiedAddr<A>),
    /// Use the device with the specified identifier.
    Interface(D),
}

/// Selector for the device to use when changing multicast membership settings.
///
/// This is like `Option<MulticastInterfaceSelector` except it specifies the
/// semantics of the `None` value as "pick any device".
#[derive(Copy, Clone, Debug, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip(A, IpAddress)]
pub enum MulticastMembershipInterfaceSelector<A: IpAddress, D> {
    /// Use the specified interface.
    Specified(MulticastInterfaceSelector<A, D>),
    /// Pick any device with a route to the multicast target address.
    AnyInterfaceWithRoute,
}

impl<A: IpAddress, D> From<MulticastInterfaceSelector<A, D>>
    for MulticastMembershipInterfaceSelector<A, D>
{
    fn from(selector: MulticastInterfaceSelector<A, D>) -> Self {
        Self::Specified(selector)
    }
}

/// Sets the specified socket's membership status for the given group.
///
/// If `id` is unbound, the membership state will take effect when it is bound.
/// An error is returned if the membership change request is invalid (e.g.
/// leaving a group that was not joined, or joining a group multiple times) or
/// if the device to use to join is unspecified or conflicts with the existing
/// socket state.
pub(crate) fn set_multicast_membership<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    multicast_group: MulticastAddr<I::Addr>,
    interface: MulticastMembershipInterfaceSelector<I::Addr, SC::DeviceId>,
    want_membership: bool,
) -> Result<(), SetMulticastMembershipError> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let (_, bound_device): (&IpOptions<_, _, _>, _) =
            get_options_device(sync_ctx, state.get(id.get_key_index()).expect("socket not found"));

        let interface = match interface {
            MulticastMembershipInterfaceSelector::Specified(selector) => match selector {
                MulticastInterfaceSelector::Interface(device) => {
                    if bound_device.as_ref().map_or(false, |d| d != &device) {
                        return Err(SetMulticastMembershipError::WrongDevice);
                    } else {
                        EitherDeviceId::Strong(device)
                    }
                }
                MulticastInterfaceSelector::LocalAddress(addr) => EitherDeviceId::Strong(
                    pick_interface_for_addr(sync_ctx, multicast_group, Some(addr))?,
                ),
            },
            MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute => {
                if let Some(bound_device) = bound_device.as_ref() {
                    EitherDeviceId::Weak(bound_device.clone())
                } else {
                    EitherDeviceId::Strong(pick_interface_for_addr(
                        sync_ctx,
                        multicast_group,
                        None,
                    )?)
                }
            }
        };

        let ip_options = get_options_mut(sync_ctx, state, id);

        let Some(strong_interface) = interface.as_strong(sync_ctx) else {
            return Err(SetMulticastMembershipError::DeviceDoesNotExist);
        };

        let change = ip_options
            .multicast_memberships
            .apply_membership_change(multicast_group, &interface.as_weak(sync_ctx), want_membership)
            .ok_or(if want_membership {
                SetMulticastMembershipError::GroupAlreadyJoined
            } else {
                SetMulticastMembershipError::GroupNotJoined
            })?;

        DatagramBoundStateContext::<I, _, _>::with_transport_context(sync_ctx, |sync_ctx| {
            match change {
                MulticastMembershipChange::Join => {
                    MulticastMembershipHandler::<I, _>::join_multicast_group(
                        sync_ctx,
                        ctx,
                        &strong_interface,
                        multicast_group,
                    )
                }
                MulticastMembershipChange::Leave => {
                    MulticastMembershipHandler::<I, _>::leave_multicast_group(
                        sync_ctx,
                        ctx,
                        &strong_interface,
                        multicast_group,
                    )
                }
            }
        });

        Ok(())
    })
}

fn get_options_device_from_conn_state<I: IpExt, D: Eq + Hash, S: DatagramSocketSpec, O>(
    ConnState {
        socket,
        clear_device_on_disconnect: _,
        shutdown: _,
        addr: ConnAddr { device, ip: _ },
        extra: _,
    }: &ConnState<I, D, S, O>,
) -> (&O, &Option<D>) {
    (socket.options(), device)
}

pub(crate) fn get_options_device<
    'a,
    I: IpExt,
    S: DatagramSocketSpec,
    C,
    SC: DatagramBoundStateContext<I, C, S>,
>(
    sync_ctx: &mut SC,
    state: &'a SocketState<I, SC::WeakDeviceId, S>,
) -> (&'a IpOptions<I, SC::WeakDeviceId, S>, &'a Option<SC::WeakDeviceId>) {
    match state {
        SocketState::Unbound(state) => {
            let UnboundSocketState { ip_options, device, sharing: _ } = state;
            (ip_options, device)
        }
        SocketState::Bound(BoundSocketState::Listener { state, sharing: _ }) => {
            let ListenerState { ip_options, addr: ListenerAddr { device, ip: _ } } = state;
            (ip_options, device)
        }
        SocketState::Bound(BoundSocketState::Connected { state, sharing: _ }) => {
            match sync_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    match dual_stack.converter().convert(state) {
                        DualStackConnState::ThisStack(state) => {
                            get_options_device_from_conn_state(state)
                        }
                        DualStackConnState::OtherStack(state) => {
                            dual_stack.assert_dual_stack_enabled(state);
                            get_options_device_from_conn_state(state)
                        }
                    }
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    get_options_device_from_conn_state(not_dual_stack.converter().convert(state))
                }
            }
        }
    }
}

fn get_options_mut<'a, I: IpExt, S: DatagramSocketSpec, C, SC: DatagramBoundStateContext<I, C, S>>(
    sync_ctx: &mut SC,
    state: &'a mut SocketsState<I, SC::WeakDeviceId, S>,
    id: S::SocketId<I>,
) -> &'a mut IpOptions<I, SC::WeakDeviceId, S>
where
    S::SocketId<I>: EntryKey,
{
    match state.get_mut(id.get_key_index()).expect("socket not found") {
        SocketState::Unbound(state) => {
            let UnboundSocketState { ip_options, device: _, sharing: _ } = state;
            ip_options
        }
        SocketState::Bound(BoundSocketState::Listener { state, sharing: _ }) => {
            let ListenerState { ip_options, addr: _ } = state;
            ip_options
        }
        SocketState::Bound(BoundSocketState::Connected { state, sharing: _ }) => {
            match sync_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    match dual_stack.converter().convert(state) {
                        DualStackConnState::ThisStack(state) => state.as_mut(),
                        DualStackConnState::OtherStack(state) => {
                            dual_stack.assert_dual_stack_enabled(state);
                            state.as_mut()
                        }
                    }
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    not_dual_stack.converter().convert(state).as_mut()
                }
            }
        }
    }
}

pub(crate) fn update_ip_hop_limit<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
    update: impl FnOnce(&mut SocketHopLimits),
) {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let options = get_options_mut(sync_ctx, state, id);

        update(&mut options.hop_limits)
    })
}

pub(crate) fn get_ip_hop_limits<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
) -> HopLimits {
    sync_ctx.with_sockets_state(|sync_ctx, state| {
        let (options, device) =
            get_options_device(sync_ctx, state.get(id.get_key_index()).expect("socket not found"));
        let device = device.as_ref().and_then(|d| sync_ctx.upgrade_weak_device_id(d));
        DatagramBoundStateContext::<I, _, _>::with_transport_context(sync_ctx, |sync_ctx| {
            options.hop_limits.get_limits_with_defaults(
                &TransportIpContext::<I, _>::get_default_hop_limits(sync_ctx, device.as_ref()),
            )
        })
    })
}

pub(crate) fn with_dual_stack_ip_options_mut<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
    cb: impl FnOnce(&mut S::OtherStackIpOptions<I>) -> R,
) -> R {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let options = get_options_mut(sync_ctx, state, id);

        cb(&mut options.other_stack)
    })
}

pub(crate) fn with_dual_stack_ip_options<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
    cb: impl FnOnce(&S::OtherStackIpOptions<I>) -> R,
) -> R {
    sync_ctx.with_sockets_state(|sync_ctx, state| {
        let (options, _device) =
            get_options_device(sync_ctx, state.get(id.get_key_index()).expect("not found"));

        cb(&options.other_stack)
    })
}

pub(crate) fn update_sharing<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec<SharingState = Sharing>,
    Sharing: Clone,
>(
    sync_ctx: &mut SC,
    id: S::SocketId<I>,
    new_sharing: Sharing,
) -> Result<(), ExpectedUnboundError> {
    sync_ctx.with_sockets_state_mut(|_sync_ctx, state| {
        let state = match state.get_mut(id.get_key_index()).expect("socket not found") {
            SocketState::Bound(_) => return Err(ExpectedUnboundError),
            SocketState::Unbound(state) => state,
        };

        let UnboundSocketState { device: _, sharing, ip_options: _ } = state;
        *sharing = new_sharing;
        Ok(())
    })
}

pub(crate) fn get_sharing<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec<SharingState = Sharing>,
    Sharing: Clone,
>(
    sync_ctx: &mut SC,
    id: S::SocketId<I>,
) -> Sharing {
    sync_ctx.with_sockets_state(|_sync_ctx, state| {
        match state.get(id.get_key_index()).expect("socket not found") {
            SocketState::Unbound(state) => {
                let UnboundSocketState { device: _, sharing, ip_options: _ } = state;
                sharing
            }
            SocketState::Bound(state) => match state {
                BoundSocketState::Listener { state: _, sharing } => sharing,
                BoundSocketState::Connected { state: _, sharing } => sharing,
            },
        }
        .clone()
    })
}

pub(crate) fn set_ip_transparent<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    id: S::SocketId<I>,
    value: bool,
) {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        get_options_mut(sync_ctx, state, id).transparent = value;
    })
}

pub(crate) fn get_ip_transparent<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    id: S::SocketId<I>,
) -> bool {
    sync_ctx.with_sockets_state(|sync_ctx, state| {
        let (options, _device) =
            get_options_device(sync_ctx, state.get(id.get_key_index()).expect("missing socket"));
        options.transparent
    })
}

#[cfg(test)]
mod test {
    use core::{convert::Infallible as Never, ops::DerefMut as _};

    use alloc::{vec, vec::Vec};
    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use derivative::Derivative;
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
    use packet::Buf;
    use packet_formats::ip::{Ipv4Proto, Ipv6Proto};
    use test_case::test_case;

    use crate::{
        context::testutil::{FakeNonSyncCtx, Wrapped, WrappedFakeSyncCtx},
        data_structures::socketmap::SocketMap,
        device::testutil::{FakeDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
        ip::{
            device::state::{IpDeviceState, IpDeviceStateIpExt},
            socket::testutil::{FakeDeviceConfig, FakeDualStackIpSocketCtx, FakeIpSocketCtx},
            testutil::{DualStackSendIpPacketMeta, FakeIpDeviceIdCtx},
            IpLayerIpExt, DEFAULT_HOP_LIMITS,
        },
        socket::{
            Bound, IncompatibleError, InsertError, ListenerAddrInfo, RemoveResult,
            SocketMapAddrStateSpec,
        },
        testutil::TestIpExt,
    };

    use super::*;

    trait DatagramIpExt:
        Ip + IpExt + IpDeviceStateIpExt + TestIpExt + DualStackIpExt + DualStackContextsIpExt
    {
    }
    impl<
            I: Ip + IpExt + IpDeviceStateIpExt + TestIpExt + DualStackIpExt + DualStackContextsIpExt,
        > DatagramIpExt for I
    {
    }

    #[derive(Debug)]
    enum FakeAddrSpec {}

    impl SocketMapAddrSpec for FakeAddrSpec {
        type LocalIdentifier = u8;
        type RemoteIdentifier = char;
    }

    #[derive(Debug)]
    enum FakeStateSpec {}

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Tag;

    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]

    enum Sharing {
        #[default]
        NoConflicts,
        // Any attempt to insert a connection with the following remote port
        // will conflict.
        ConnectionConflicts {
            remote_port: char,
        },
    }

    #[derive(Copy, Clone, Debug, Derivative)]
    #[derivative(Eq(bound = ""), PartialEq(bound = ""))]
    struct Id(usize);

    impl From<usize> for Id {
        fn from(u: usize) -> Self {
            Self(u)
        }
    }

    impl EntryKey for Id {
        fn get_key_index(&self) -> usize {
            let Self(u) = self;
            *u
        }
    }

    impl<I: Ip, D: crate::device::Id> SocketMapStateSpec for (FakeStateSpec, I, D) {
        type AddrVecTag = Tag;
        type ConnAddrState = Id;
        type ConnId = Id;
        type ConnSharingState = Sharing;
        type ListenerAddrState = Id;
        type ListenerId = Id;
        type ListenerSharingState = Sharing;
        fn listener_tag(_: ListenerAddrInfo, _state: &Self::ListenerAddrState) -> Self::AddrVecTag {
            Tag
        }
        fn connected_tag(_has_device: bool, _state: &Self::ConnAddrState) -> Self::AddrVecTag {
            Tag
        }
    }

    const FAKE_DATAGRAM_IPV4_PROTOCOL: Ipv4Proto = Ipv4Proto::Other(253);
    const FAKE_DATAGRAM_IPV6_PROTOCOL: Ipv6Proto = Ipv6Proto::Other(254);

    impl DatagramSocketSpec for FakeStateSpec {
        type AddrSpec = FakeAddrSpec;
        type SocketId<I: IpExt> = Id;
        type OtherStackIpOptions<I: IpExt> = ();
        type SocketMapSpec<I: IpExt, D: device::WeakId> = (Self, I, D);
        type SharingState = Sharing;
        type ListenerIpAddr<I: IpExt> = I::DualStackListenerIpAddr<u8>;
        type ConnIpAddr<I: IpExt> = I::DualStackConnIpAddr<Self>;
        type ConnStateExtra = ();
        type ConnState<I: IpExt, D: Debug + Eq + Hash> = I::DualStackConnState<D, Self>;

        fn ip_proto<I: IpProtoExt>() -> I::Proto {
            I::map_ip((), |()| FAKE_DATAGRAM_IPV4_PROTOCOL, |()| FAKE_DATAGRAM_IPV6_PROTOCOL)
        }

        fn make_bound_socket_map_id<I: IpExt, D: device::WeakId>(
            s: Self::SocketId<I>,
        ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::BoundSocketId
        {
            s
        }

        type Serializer<I: IpExt, B: BufferMut> = B;
        type SerializeError = Never;
        fn make_packet<I: IpExt, B: BufferMut>(
            body: B,
            _addr: &ConnIpAddr<
                I::Addr,
                <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        ) -> Result<Self::Serializer<I, B>, Never> {
            Ok(body)
        }
        fn try_alloc_listen_identifier<I: Ip, D: device::WeakId>(
            _ctx: &mut impl RngContext,
            is_available: impl Fn(u8) -> Result<(), InUseError>,
        ) -> Option<u8> {
            (0..=u8::MAX).find(|i| is_available(*i).is_ok())
        }

        fn conn_addr_from_state<I: IpExt, D: Clone + Debug + Eq + Hash>(
            state: &Self::ConnState<I, D>,
        ) -> ConnAddr<Self::ConnIpAddr<I>, D> {
            I::conn_addr_from_state(state)
        }
    }

    impl<I: IpExt, D: device::WeakId> DatagramSocketMapSpec<I, D, FakeAddrSpec>
        for (FakeStateSpec, I, D)
    {
        type BoundSocketId = Id;
    }

    impl<I: IpExt, D: device::WeakId>
        SocketMapConflictPolicy<
            ConnAddr<ConnIpAddr<I::Addr, u8, char>, D>,
            Sharing,
            I,
            D,
            FakeAddrSpec,
        > for (FakeStateSpec, I, D)
    {
        fn check_insert_conflicts(
            sharing: &Sharing,
            addr: &ConnAddr<ConnIpAddr<I::Addr, u8, char>, D>,
            _socketmap: &SocketMap<AddrVec<I, D, FakeAddrSpec>, Bound<Self>>,
        ) -> Result<(), InsertError> {
            let ConnAddr { ip: ConnIpAddr { local: _, remote: (_remote_ip, port) }, device: _ } =
                addr;
            match sharing {
                Sharing::NoConflicts => Ok(()),
                Sharing::ConnectionConflicts { remote_port } => {
                    if remote_port == port {
                        Err(InsertError::Exists)
                    } else {
                        Ok(())
                    }
                }
            }
        }
    }

    impl<I: IpExt, D: device::WeakId>
        SocketMapConflictPolicy<
            ListenerAddr<ListenerIpAddr<I::Addr, u8>, D>,
            Sharing,
            I,
            D,
            FakeAddrSpec,
        > for (FakeStateSpec, I, D)
    {
        fn check_insert_conflicts(
            sharing: &Sharing,
            _addr: &ListenerAddr<ListenerIpAddr<I::Addr, u8>, D>,
            _socketmap: &SocketMap<AddrVec<I, D, FakeAddrSpec>, Bound<Self>>,
        ) -> Result<(), InsertError> {
            match sharing {
                Sharing::NoConflicts => Ok(()),
                // Since this implementation is strictly for ListenerAddr,
                // ignore connection conflicts.
                Sharing::ConnectionConflicts { remote_port: _ } => Ok(()),
            }
        }
    }

    impl SocketMapAddrStateSpec for Id {
        type Id = Self;
        type SharingState = Sharing;
        type Inserter<'a> = Never where Self: 'a;

        fn new(_sharing: &Self::SharingState, id: Self) -> Self {
            id
        }
        fn contains_id(&self, id: &Self::Id) -> bool {
            self == id
        }
        fn try_get_inserter<'a, 'b>(
            &'b mut self,
            _new_sharing_state: &'a Self::SharingState,
        ) -> Result<Self::Inserter<'b>, IncompatibleError> {
            Err(IncompatibleError)
        }
        fn could_insert(
            &self,
            _new_sharing_state: &Self::SharingState,
        ) -> Result<(), IncompatibleError> {
            Err(IncompatibleError)
        }
        fn remove_by_id(&mut self, _id: Self::Id) -> RemoveResult {
            RemoveResult::IsLast
        }
    }

    #[derive(Derivative, GenericOverIp)]
    #[derivative(Default(bound = ""))]
    #[generic_over_ip()]
    struct FakeBoundSockets<D: FakeStrongDeviceId> {
        v4: BoundSockets<
            Ipv4,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, Ipv4, FakeWeakDeviceId<D>),
        >,
        v6: BoundSockets<
            Ipv6,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, Ipv6, FakeWeakDeviceId<D>),
        >,
    }

    impl<D: FakeStrongDeviceId, I: IpExt>
        AsRef<
            BoundSockets<
                I,
                FakeWeakDeviceId<D>,
                FakeAddrSpec,
                (FakeStateSpec, I, FakeWeakDeviceId<D>),
            >,
        > for FakeBoundSockets<D>
    {
        fn as_ref(
            &self,
        ) -> &BoundSockets<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, I, FakeWeakDeviceId<D>),
        > {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpExt, D: FakeStrongDeviceId>(
                &'a BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    (FakeStateSpec, I, FakeWeakDeviceId<D>),
                >,
            );
            let Wrap(state) = I::map_ip(self, |state| Wrap(&state.v4), |state| Wrap(&state.v6));
            state
        }
    }

    impl<D: FakeStrongDeviceId, I: IpExt>
        AsMut<
            BoundSockets<
                I,
                FakeWeakDeviceId<D>,
                FakeAddrSpec,
                (FakeStateSpec, I, FakeWeakDeviceId<D>),
            >,
        > for FakeBoundSockets<D>
    {
        fn as_mut(
            &mut self,
        ) -> &mut BoundSockets<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, I, FakeWeakDeviceId<D>),
        > {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpExt, D: FakeStrongDeviceId>(
                &'a mut BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    (FakeStateSpec, I, FakeWeakDeviceId<D>),
                >,
            );
            let Wrap(state) =
                I::map_ip(self, |state| Wrap(&mut state.v4), |state| Wrap(&mut state.v6));
            state
        }
    }

    type FakeSocketsState<I, D> = SocketsState<I, FakeWeakDeviceId<D>, FakeStateSpec>;

    type FakeInnerSyncCtx<D> = crate::context::testutil::FakeSyncCtx<
        FakeDualStackIpSocketCtx<D>,
        DualStackSendIpPacketMeta<D>,
        D,
    >;

    type FakeSyncCtx<I, D> =
        Wrapped<FakeSocketsState<I, D>, Wrapped<FakeBoundSockets<D>, FakeInnerSyncCtx<D>>>;

    impl<I: IpExt, D: FakeStrongDeviceId> FakeSyncCtx<I, D> {
        fn new_with_sockets(state: FakeSocketsState<I, D>, bound: FakeBoundSockets<D>) -> Self {
            Self {
                outer: state,
                inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                    FakeDualStackIpSocketCtx::default(),
                    bound,
                ),
            }
        }
    }

    impl<I: DatagramIpExt + IpLayerIpExt>
        DatagramStateContext<I, FakeNonSyncCtx<(), (), ()>, FakeStateSpec>
        for FakeSyncCtx<I, FakeDeviceId>
    {
        type SocketsStateCtx<'a> =
            Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>;

        fn with_sockets_state<
            O,
            F: FnOnce(
                &mut Self::SocketsStateCtx<'_>,
                &SocketsState<I, Self::WeakDeviceId, FakeStateSpec>,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer)
        }

        fn with_sockets_state_mut<
            O,
            F: FnOnce(
                &mut Self::SocketsStateCtx<'_>,
                &mut SocketsState<I, Self::WeakDeviceId, FakeStateSpec>,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer)
        }

        fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketsStateCtx<'_>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer: _, inner } = self;
            cb(inner)
        }
    }

    /// A test-only IpExt trait to specialize the `DualStackContext` and
    /// `NonDualStackContext` associated types on the
    /// `DatagramBoundStateContext`.
    ///
    /// This allows us to implement `DatagramBoundStateContext` for all `I`
    /// while also assigning its associated types different values for `Ipv4`
    /// and `Ipv6`.
    trait DualStackContextsIpExt: Ip + DualStackIpExt {
        type DualStackContext: DualStackDatagramBoundStateContext<
            Self,
            FakeNonSyncCtx<(), (), ()>,
            FakeStateSpec,
            DeviceId = FakeDeviceId,
            WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>,
        >;
        type NonDualStackContext: NonDualStackDatagramBoundStateContext<
            Self,
            FakeNonSyncCtx<(), (), ()>,
            FakeStateSpec,
            DeviceId = FakeDeviceId,
            WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>,
        >;
        fn dual_stack_context(
            sync_ctx: &mut Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext>;
    }

    impl DualStackContextsIpExt for Ipv4 {
        type DualStackContext = UninstantiableContext<
            Self,
            FakeStateSpec,
            Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>,
        >;
        type NonDualStackContext =
            Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>;
        fn dual_stack_context(
            sync_ctx: &mut Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            MaybeDualStack::NotDualStack(sync_ctx)
        }
    }

    impl DualStackContextsIpExt for Ipv6 {
        type DualStackContext =
            Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>;
        type NonDualStackContext = UninstantiableContext<
            Self,
            FakeStateSpec,
            Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>,
        >;
        fn dual_stack_context(
            sync_ctx: &mut Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            MaybeDualStack::DualStack(sync_ctx)
        }
    }

    impl<I: Ip + IpExt + IpDeviceStateIpExt + DualStackContextsIpExt>
        DatagramBoundStateContext<I, FakeNonSyncCtx<(), (), ()>, FakeStateSpec>
        for Wrapped<FakeBoundSockets<FakeDeviceId>, FakeInnerSyncCtx<FakeDeviceId>>
    {
        type IpSocketsCtx<'a> = FakeInnerSyncCtx<FakeDeviceId>;
        type LocalIdAllocator = ();
        type DualStackContext = I::DualStackContext;
        type NonDualStackContext = I::NonDualStackContext;

        fn with_bound_sockets<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &BoundSockets<
                    I,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, I, Self::WeakDeviceId),
                >,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_ref())
        }
        fn with_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<
                    I,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, I, Self::WeakDeviceId),
                >,
                &mut Self::LocalIdAllocator,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_mut(), &mut ())
        }

        fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer: _, inner } = self;
            cb(inner)
        }

        fn dual_stack_context(
            &mut self,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            I::dual_stack_context(self)
        }
    }

    impl<D: FakeStrongDeviceId>
        NonDualStackDatagramBoundStateContext<Ipv4, FakeNonSyncCtx<(), (), ()>, FakeStateSpec>
        for Wrapped<FakeBoundSockets<D>, FakeInnerSyncCtx<D>>
    {
        type Converter = ();
        fn converter(&self) -> Self::Converter {
            ()
        }
    }

    impl<D: FakeStrongDeviceId>
        DualStackDatagramBoundStateContext<Ipv6, FakeNonSyncCtx<(), (), ()>, FakeStateSpec>
        for Wrapped<FakeBoundSockets<D>, FakeInnerSyncCtx<D>>
    {
        type IpSocketsCtx<'a> = FakeInnerSyncCtx<D>;
        fn dual_stack_enabled(
            &self,
            _state: &impl AsRef<IpOptions<Ipv6, Self::WeakDeviceId, FakeStateSpec>>,
        ) -> bool {
            // For now, it's simplest to have dual-stack unconditionally enabled
            // for datagram tests. However, in the future this could be stateful
            // and follow an implementation similar to UDP's test fixture.
            true
        }
        fn assert_dual_stack_enabled(
            &self,
            state: &impl AsRef<IpOptions<Ipv6, Self::WeakDeviceId, FakeStateSpec>>,
        ) {
            assert!(self.dual_stack_enabled(state))
        }
        type Converter = ();
        fn converter(&self) -> Self::Converter {
            ()
        }

        fn from_other_ip_addr(&self, addr: Ipv4Addr) -> Ipv6Addr {
            addr.to_ipv6_mapped()
        }

        fn to_other_bound_socket_id(&self, id: Id) -> Id {
            id
        }

        type LocalIdAllocator = ();
        type OtherLocalIdAllocator = ();

        fn with_both_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<
                    Ipv6,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, Ipv6, Self::WeakDeviceId),
                >,
                &mut BoundSockets<
                    Ipv4,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, Ipv4, Self::WeakDeviceId),
                >,
                &mut Self::LocalIdAllocator,
                &mut Self::OtherLocalIdAllocator,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { ref mut outer, inner } = self;
            let FakeBoundSockets { v4, v6 } = outer;
            cb(inner, v6, v4, &mut (), &mut ())
        }

        fn with_other_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<
                    Ipv4,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, Ipv4, Self::WeakDeviceId),
                >,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_mut())
        }
    }

    impl<I: Ip + IpExt + IpDeviceStateIpExt, D: FakeStrongDeviceId>
        LocalIdentifierAllocator<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            FakeNonSyncCtx<(), (), ()>,
            (FakeStateSpec, I, FakeWeakDeviceId<D>),
        > for ()
    {
        fn try_alloc_local_id(
            &mut self,
            bound: &BoundSocketMap<
                I,
                FakeWeakDeviceId<D>,
                FakeAddrSpec,
                (FakeStateSpec, I, FakeWeakDeviceId<D>),
            >,
            _ctx: &mut FakeNonSyncCtx<(), (), ()>,
            _flow: DatagramFlowId<I::Addr, <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier>,
        ) -> Option<<FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
            (0..u8::MAX).find_map(|identifier| {
                bound
                    .listeners()
                    .could_insert(
                        &ListenerAddr {
                            device: None,
                            ip: ListenerIpAddr { addr: None, identifier },
                        },
                        &Default::default(),
                    )
                    .is_ok()
                    .then_some(identifier)
            })
        }
    }

    #[ip_test]
    fn set_get_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);
        const EXPECTED_HOP_LIMITS: HopLimits = HopLimits {
            unicast: const_unwrap_option(NonZeroU8::new(45)),
            multicast: const_unwrap_option(NonZeroU8::new(23)),
        };

        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), |limits| {
            *limits = SocketHopLimits {
                unicast: Some(EXPECTED_HOP_LIMITS.unicast),
                multicast: Some(EXPECTED_HOP_LIMITS.multicast),
            }
        });

        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            EXPECTED_HOP_LIMITS
        );
    }

    #[ip_test]
    fn set_get_device_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, _> {
            outer: FakeSocketsState::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: FakeDeviceId,
                    local_ips: Default::default(),
                    remote_ips: Default::default(),
                }]),
                Default::default(),
            ),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);
        set_device(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), Some(&FakeDeviceId)).unwrap();

        let HopLimits { mut unicast, multicast } = DEFAULT_HOP_LIMITS;
        unicast = unicast.checked_add(1).unwrap();
        {
            let ip_socket_ctx = sync_ctx.inner.inner.get_ref();
            let device_state: &IpDeviceState<_, I> =
                ip_socket_ctx.get_device_state(&FakeDeviceId).as_ref();
            let mut default_hop_limit = device_state.default_hop_limit.write();
            let default_hop_limit = default_hop_limit.deref_mut();
            assert_ne!(*default_hop_limit, unicast);
            *default_hop_limit = unicast;
        }
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            HopLimits { unicast, multicast }
        );

        // If the device is removed, use default hop limits.
        AsMut::<FakeIpDeviceIdCtx<_>>::as_mut(&mut sync_ctx.inner.inner.get_mut())
            .set_device_removed(FakeDeviceId, true);
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn default_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );

        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), |limits| {
            *limits = SocketHopLimits {
                unicast: Some(const_unwrap_option(NonZeroU8::new(1))),
                multicast: Some(const_unwrap_option(NonZeroU8::new(1))),
            }
        });

        // The limits no longer match the default.
        assert_ne!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );

        // Clear the hop limits set on the socket.
        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), |limits| {
            *limits = Default::default()
        });

        // The values should be back at the defaults.
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn bind_device_unbound<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx =
            FakeSyncCtx::<I, _>::new_with_sockets(Default::default(), Default::default());
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);

        set_device(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), Some(&FakeDeviceId)).unwrap();
        assert_eq!(
            get_bound_device(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        set_device(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), None).unwrap();
        assert_eq!(get_bound_device(&mut sync_ctx, &non_sync_ctx, unbound), None);
    }

    #[ip_test]
    fn send_to_binds_unbound<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, FakeDeviceId> {
            outer: Default::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![I::FAKE_CONFIG.remote_ip],
                }]),
                Default::default(),
            ),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let socket = create(&mut sync_ctx);
        let body = Buf::new(Vec::new(), ..);

        send_to(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
            'a',
            body,
        )
        .expect("succeeds");
        assert_matches!(
            get_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Listener(_)
        );
    }

    #[ip_test]
    fn send_to_no_route_still_binds<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, _> {
            outer: Default::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![],
                }]),
                Default::default(),
            ),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let socket = create(&mut sync_ctx);
        let body = Buf::new(Vec::new(), ..);

        assert_matches!(
            send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                'a',
                body,
            ),
            Err(Either::Right(SendToError::CreateAndSend(_)))
        );
        assert_matches!(
            get_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Listener(_)
        );
    }

    #[ip_test]
    #[test_case(true; "remove device b")]
    #[test_case(false; "dont remove device b")]
    fn multicast_membership_changes<I: Ip + DatagramIpExt + TestIpExt>(remove_device_b: bool) {
        let mut sync_ctx = FakeIpSocketCtx::<I, MultipleDevicesId>::new(
            MultipleDevicesId::all().into_iter().map(|device| FakeDeviceConfig {
                device,
                local_ips: Default::default(),
                remote_ips: Default::default(),
            }),
        );
        let mut non_sync_ctx = FakeNonSyncCtx::<(), (), ()>::default();

        let multicast_addr1 = I::get_multicast_addr(1);
        let mut memberships = MulticastMemberships::default();
        assert_eq!(
            memberships.apply_membership_change(
                multicast_addr1,
                &FakeWeakDeviceId(MultipleDevicesId::A),
                true /* want_membership */
            ),
            Some(MulticastMembershipChange::Join),
        );
        sync_ctx.join_multicast_group(&mut non_sync_ctx, &MultipleDevicesId::A, multicast_addr1);

        let multicast_addr2 = I::get_multicast_addr(2);
        assert_eq!(
            memberships.apply_membership_change(
                multicast_addr2,
                &FakeWeakDeviceId(MultipleDevicesId::B),
                true /* want_membership */
            ),
            Some(MulticastMembershipChange::Join),
        );
        sync_ctx.join_multicast_group(&mut non_sync_ctx, &MultipleDevicesId::B, multicast_addr2);

        for (device, addr, expected) in [
            (MultipleDevicesId::A, multicast_addr1, true),
            (MultipleDevicesId::A, multicast_addr2, false),
            (MultipleDevicesId::B, multicast_addr1, false),
            (MultipleDevicesId::B, multicast_addr2, true),
        ] {
            assert_eq!(
                sync_ctx.get_device_state(&device).multicast_groups.read().contains(&addr),
                expected,
                "device={:?}, addr={}",
                device,
                addr,
            );
        }

        if remove_device_b {
            AsMut::<FakeIpDeviceIdCtx<_>>::as_mut(&mut sync_ctx)
                .set_device_removed(MultipleDevicesId::B, true);
        }

        leave_all_joined_groups(&mut sync_ctx, &mut non_sync_ctx, memberships);
        for (device, addr, expected) in [
            (MultipleDevicesId::A, multicast_addr1, false),
            (MultipleDevicesId::A, multicast_addr2, false),
            (MultipleDevicesId::B, multicast_addr1, false),
            // Should not attempt to leave the multicast group on the device if
            // the device looks like it was removed. Note that although we mark
            // the device as removed, we do not destroy its state so we can
            // inspect it here.
            (MultipleDevicesId::B, multicast_addr2, remove_device_b),
        ] {
            assert_eq!(
                sync_ctx.get_device_state(&device).multicast_groups.read().contains(&addr),
                expected,
                "device={:?}, addr={}",
                device,
                addr,
            );
        }
    }

    #[ip_test]
    fn set_get_transparent<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, _> {
            outer: FakeSocketsState::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: FakeDeviceId,
                    local_ips: Default::default(),
                    remote_ips: Default::default(),
                }]),
                Default::default(),
            ),
        };

        let unbound = create(&mut sync_ctx);

        assert!(!get_ip_transparent(&mut sync_ctx, unbound.clone()));

        set_ip_transparent(&mut sync_ctx, unbound.clone(), true);

        assert!(get_ip_transparent(&mut sync_ctx, unbound.clone()));

        set_ip_transparent(&mut sync_ctx, unbound.clone(), false);

        assert!(!get_ip_transparent(&mut sync_ctx, unbound.clone()));
    }

    // Translates a listener addr from the associated type to a concrete type.
    fn convert_listener_addr<I: DualStackIpExt, LI: Clone + Debug>(
        addr: I::DualStackListenerIpAddr<LI>,
    ) -> MaybeDualStack<DualStackListenerIpAddr<I::Addr, LI>, ListenerIpAddr<I::Addr, LI>> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<I: DualStackIpExt, LI: Clone + Debug>(I::DualStackListenerIpAddr<LI>);
        I::map_ip(
            Wrapper(addr),
            |Wrapper(addr)| MaybeDualStack::NotDualStack(addr),
            |Wrapper(addr)| MaybeDualStack::DualStack(addr),
        )
    }

    // Translates a conn addr from the associated type to a concrete type.
    fn convert_conn_addr<I: DualStackIpExt, S: DatagramSocketSpec>(
        addr: I::DualStackConnIpAddr<S>,
    ) -> MaybeDualStack<
        DualStackConnIpAddr<
            I::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
        ConnIpAddr<
            I::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    > {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<I: DualStackIpExt, S: DatagramSocketSpec>(I::DualStackConnIpAddr<S>);
        I::map_ip(
            Wrapper(addr),
            |Wrapper(addr)| MaybeDualStack::NotDualStack(addr),
            |Wrapper(addr)| MaybeDualStack::DualStack(addr),
        )
    }

    enum OriginalSocketState {
        Unbound,
        Listener,
        Connected,
    }

    #[ip_test]
    #[test_case(OriginalSocketState::Unbound; "reinsert_unbound")]
    #[test_case(OriginalSocketState::Listener; "reinsert_listener")]
    #[test_case(OriginalSocketState::Connected; "reinsert_connected")]
    fn connect_reinserts_on_failure<I: Ip + DatagramIpExt + IpLayerIpExt>(
        original: OriginalSocketState,
    ) {
        let mut sync_ctx = FakeSyncCtx::<I, _> {
            outer: FakeSocketsState::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![I::FAKE_CONFIG.remote_ip],
                }]),
                Default::default(),
            ),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let socket = create(&mut sync_ctx);
        const LOCAL_PORT: u8 = 10;
        const ORIGINAL_REMOTE_PORT: char = 'a';
        const NEW_REMOTE_PORT: char = 'b';

        // Setup the original socket state.
        match original {
            OriginalSocketState::Unbound => {}
            OriginalSocketState::Listener => listen(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed"),
            OriginalSocketState::Connected => connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                ORIGINAL_REMOTE_PORT,
                Default::default(),
            )
            .expect("connect should succeed"),
        }

        // Update the sharing state to generate conflicts during the call to `connect`.
        sync_ctx.with_sockets_state_mut(|_sync_ctx, state| {
            let sharing = match state.get_mut(socket.get_key_index()).expect("socket not found") {
                SocketState::Unbound(UnboundSocketState { device: _, sharing, ip_options: _ }) => {
                    sharing
                }
                SocketState::Bound(BoundSocketState::Connected { state: _, sharing }) => sharing,
                SocketState::Bound(BoundSocketState::Listener { state: _, sharing }) => sharing,
            };
            *sharing = Sharing::ConnectionConflicts { remote_port: NEW_REMOTE_PORT };
        });

        // Try to connect and observe a conflict error.
        assert_matches!(
            connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                NEW_REMOTE_PORT,
                Default::default(),
            ),
            Err(ConnectError::SockAddrConflict)
        );

        // Verify the original socket state is intact.
        let info = get_info(&mut sync_ctx, &mut non_sync_ctx, socket);
        match original {
            OriginalSocketState::Unbound => assert_matches!(info, SocketInfo::Unbound),
            OriginalSocketState::Listener => {
                let ip =
                    assert_matches!(info, SocketInfo::Listener(ListenerAddr{ip, device: _}) => ip);
                let identifier = match convert_listener_addr::<I, _>(ip) {
                    MaybeDualStack::DualStack(DualStackListenerIpAddr::ThisStack(
                        ListenerIpAddr { addr: _, identifier },
                    )) => identifier,
                    MaybeDualStack::DualStack(DualStackListenerIpAddr::OtherStack(_))
                    | MaybeDualStack::DualStack(DualStackListenerIpAddr::BothStacks(_)) => {
                        panic!("socket unexpectedly bound in other stack")
                    }
                    MaybeDualStack::NotDualStack(ListenerIpAddr { addr: _, identifier }) => {
                        identifier
                    }
                };
                assert_eq!(LOCAL_PORT, identifier);
            }
            OriginalSocketState::Connected => {
                let ip =
                    assert_matches!(info, SocketInfo::Connected(ConnAddr{ip, device: _}) => ip);
                let identifier = match convert_conn_addr::<I, _>(ip) {
                    MaybeDualStack::DualStack(addr) => match addr {
                        DualStackConnIpAddr::ThisStack(ConnIpAddr {
                            local: _,
                            remote: (_addr, identifier),
                        }) => identifier,
                        DualStackConnIpAddr::OtherStack(_addr) => {
                            panic!("socket unexpectedly bound in other stack")
                        }
                    },
                    MaybeDualStack::NotDualStack(ConnIpAddr {
                        local: _,
                        remote: (_addr, identifier),
                    }) => identifier,
                };
                assert_eq!(ORIGINAL_REMOTE_PORT, identifier)
            }
        }
    }
}
