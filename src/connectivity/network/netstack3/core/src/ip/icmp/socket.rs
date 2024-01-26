// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! ICMP Echo Sockets.

use core::{
    fmt::Debug,
    num::{NonZeroU16, NonZeroU8},
};

use dense_map::EntryKey;
use derivative::Derivative;
use either::Either;

use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, IpVersionMarker},
    SpecifiedAddr, ZonedAddr,
};
use packet::{BufferMut, Serializer};
use packet_formats::{
    icmp::{IcmpEchoRequest, IcmpPacketBuilder},
    ip::{IpProtoExt, Ipv4Proto, Ipv6Proto},
};

use crate::{
    algorithm::{PortAlloc, PortAllocImpl},
    context::{ContextPair, RngContext},
    data_structures::socketmap::IterShadows as _,
    device::{AnyDevice, DeviceIdContext, Id, WeakId},
    error::{LocalAddressError, SocketError},
    ip::{
        icmp::{IcmpAddr, IcmpBindingsContext, IcmpIpExt, IcmpStateContext, InnerIcmpContext},
        socket::IpSock,
        IpExt,
    },
    socket::{
        self,
        address::{ConnAddr, ConnIpAddr, ListenerAddr, ListenerIpAddr},
        datagram::{
            self, DatagramBoundStateContext, DatagramFlowId, DatagramSocketMapSpec,
            DatagramSocketSpec, DatagramStateContext, ExpectedUnboundError, SocketHopLimits,
        },
        AddrVec, IncompatibleError, InsertError, ListenerAddrInfo, MaybeDualStack, ShutdownType,
        SocketMapAddrSpec, SocketMapAddrStateSpec, SocketMapConflictPolicy, SocketMapStateSpec,
    },
    sync::RwLock,
};

pub type SocketsState<I, D> = datagram::SocketsState<I, D, Icmp>;

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct IcmpSockets<I: IpExt + datagram::DualStackIpExt, D: WeakId> {
    // This will be used to store state for unbound sockets, like socket
    // options.
    pub(crate) state: RwLock<SocketsState<I, D>>,
    pub(crate) bound_and_id_allocator: RwLock<BoundSockets<I, D>>,
}

/// An identifier for an ICMP socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct SocketId<I: Ip>(usize, IpVersionMarker<I>);

impl<I: Ip> Into<usize> for SocketId<I> {
    fn into(self) -> usize {
        let Self(id, _marker) = self;
        id
    }
}

#[derive(Clone)]
pub(crate) struct IcmpConn<S> {
    icmp_id: u16,
    ip: S,
}

impl<'a, A: IpAddress, D> From<&'a IcmpConn<IpSock<A::Version, D, ()>>> for IcmpAddr<A>
where
    A::Version: IpExt,
{
    fn from(conn: &'a IcmpConn<IpSock<A::Version, D, ()>>) -> IcmpAddr<A> {
        IcmpAddr {
            local_addr: *conn.ip.local_ip(),
            remote_addr: *conn.ip.remote_ip(),
            icmp_id: conn.icmp_id,
        }
    }
}

/// The context required by the ICMP layer in order to deliver events related to
/// ICMP sockets.
pub trait IcmpEchoBindingsContext<I: IcmpIpExt, D> {
    /// Receives an ICMP echo reply.
    fn receive_icmp_echo_reply<B: BufferMut>(
        &mut self,
        conn: SocketId<I>,
        device_id: &D,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        id: u16,
        data: B,
    );
}

/// A Context that provides access to the sockets' states.
pub trait StateContext<I: IcmpIpExt + IpExt, BC: IcmpBindingsContext<I, Self::DeviceId>>:
    DeviceIdContext<AnyDevice>
{
    type SocketStateCtx<'a>: InnerIcmpContext<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + IcmpStateContext;

    /// Calls the function with an immutable reference to a socket's state.
    fn with_sockets_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to ICMP sockets.
    fn with_sockets_state_mut<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut SocketsState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function without access to ICMP socket state.
    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

/// Uninstantiatable type for implementing [`DatagramSocketSpec`].
pub enum Icmp {}

impl DatagramSocketSpec for Icmp {
    type AddrSpec = IcmpAddrSpec;

    type SocketId<I: datagram::IpExt> = SocketId<I>;

    type OtherStackIpOptions<I: datagram::IpExt> = ();

    type SharingState = ();

    type SocketMapSpec<I: datagram::IpExt + datagram::DualStackIpExt, D: WeakId> = (Self, I, D);

    fn ip_proto<I: IpProtoExt>() -> I::Proto {
        I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
    }

    fn make_bound_socket_map_id<I: datagram::IpExt, D: WeakId>(
        s: Self::SocketId<I>,
    ) -> <Self::SocketMapSpec<I, D> as datagram::DatagramSocketMapSpec<
        I,
        D,
        Self::AddrSpec,
    >>::BoundSocketId{
        s
    }

    type Serializer<I: datagram::IpExt, B: BufferMut> =
        packet::Nested<B, IcmpPacketBuilder<I, IcmpEchoRequest>>;
    type SerializeError = packet_formats::error::ParseError;

    fn make_packet<I: datagram::IpExt, B: BufferMut>(
        mut body: B,
        addr: &socket::address::ConnIpAddr<
            I::Addr,
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    ) -> Result<Self::Serializer<I, B>, Self::SerializeError> {
        let ConnIpAddr { local: (local_ip, id), remote: (remote_ip, ()) } = addr;
        // TODO(https://fxbug.dev/42124055): Instead of panic, make this trait
        // method fallible so that the caller can return errors. This will
        // become necessary once we use the datagram module for sending.
        let icmp_echo: packet_formats::icmp::IcmpPacketRaw<I, &[u8], IcmpEchoRequest> =
            body.parse()?;
        let icmp_builder = IcmpPacketBuilder::<I, _>::new(
            local_ip.addr(),
            remote_ip.addr(),
            packet_formats::icmp::IcmpUnusedCode,
            IcmpEchoRequest::new(id.get(), icmp_echo.message().seq()),
        );
        Ok(body.encapsulate(icmp_builder))
    }

    fn try_alloc_listen_identifier<I: datagram::IpExt, D: WeakId>(
        bindings_ctx: &mut impl RngContext,
        is_available: impl Fn(
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        ) -> Result<(), datagram::InUseError>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
        let mut port = IcmpBoundSockets::<I, D>::rand_ephemeral(&mut bindings_ctx.rng());
        for _ in IcmpBoundSockets::<I, D>::EPHEMERAL_RANGE {
            // We can unwrap here because we know that the EPHEMERAL_RANGE doesn't
            // include 0.
            let tryport = NonZeroU16::new(port.get()).unwrap();
            match is_available(tryport) {
                Ok(()) => return Some(tryport),
                Err(datagram::InUseError {}) => port.next(),
            }
        }
        None
    }

    type ListenerIpAddr<I: datagram::IpExt> = socket::address::ListenerIpAddr<I::Addr, NonZeroU16>;

    type ConnIpAddr<I: datagram::IpExt> = ConnIpAddr<
        I::Addr,
        <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    >;

    type ConnState<I: datagram::IpExt, D: Debug + Eq + core::hash::Hash> =
        datagram::ConnState<I, I, D, Self>;
    // Store the remote port/id set by `connect`. This does not participate in
    // demuxing, so not part of the socketmap, but we need to store it so that
    // it can be reported later.
    type ConnStateExtra = u16;

    fn conn_addr_from_state<I: datagram::IpExt, D: Clone + Debug + Eq + core::hash::Hash>(
        state: &Self::ConnState<I, D>,
    ) -> ConnAddr<Self::ConnIpAddr<I>, D> {
        let datagram::ConnState { addr, .. } = state;
        addr.clone()
    }
}

/// Uninstantiatable type for implementing [`SocketMapAddrSpec`].
pub enum IcmpAddrSpec {}

impl SocketMapAddrSpec for IcmpAddrSpec {
    type RemoteIdentifier = ();
    type LocalIdentifier = NonZeroU16;
}

type IcmpBoundSockets<I, D> = datagram::BoundSockets<I, D, IcmpAddrSpec, (Icmp, I, D)>;

impl<I: IpExt, D: WeakId> PortAllocImpl for IcmpBoundSockets<I, D> {
    const EPHEMERAL_RANGE: core::ops::RangeInclusive<u16> = 1..=u16::MAX;
    type Id = DatagramFlowId<I::Addr, ()>;

    fn is_port_available(&self, id: &Self::Id, port: u16) -> bool {
        // We can safely unwrap here, because the ports received in
        // `is_port_available` are guaranteed to be in `EPHEMERAL_RANGE`.
        let port = NonZeroU16::new(port).unwrap();
        let conn = ConnAddr {
            ip: ConnIpAddr { local: (id.local_ip, port), remote: (id.remote_ip, ()) },
            device: None,
        };

        // A port is free if there are no sockets currently using it, and if
        // there are no sockets that are shadowing it.
        AddrVec::from(conn).iter_shadows().all(|a| match &a {
            AddrVec::Listen(l) => self.listeners().get_by_addr(&l).is_none(),
            AddrVec::Conn(c) => self.conns().get_by_addr(&c).is_none(),
        } && self.get_shadower_counts(&a) == 0)
    }
}

// TODO(https://fxbug.dev/42083786): Remove the laziness by dropping `Option`.
type LocalIdAllocator<I, D> = Option<PortAlloc<IcmpBoundSockets<I, D>>>;

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct BoundSockets<I: IpExt, D: WeakId> {
    pub(crate) socket_map: IcmpBoundSockets<I, D>,
    pub(crate) allocator: LocalIdAllocator<I, D>,
}

impl<I: IpExt, D: WeakId, BC: RngContext>
    datagram::LocalIdentifierAllocator<I, D, IcmpAddrSpec, BC, (Icmp, I, D)>
    for LocalIdAllocator<I, D>
{
    fn try_alloc_local_id(
        &mut self,
        bound: &socket::BoundSocketMap<I, D, IcmpAddrSpec, (Icmp, I, D)>,
        bindings_ctx: &mut BC,
        flow: datagram::DatagramFlowId<I::Addr, ()>,
    ) -> Option<NonZeroU16> {
        let mut rng = bindings_ctx.rng();
        // Lazily init port_alloc if it hasn't been inited yet.
        let port_alloc = self.get_or_insert_with(|| PortAlloc::new(&mut rng));
        port_alloc.try_alloc(&flow, bound).and_then(NonZeroU16::new)
    }
}

impl<I, BC, CC> datagram::NonDualStackDatagramBoundStateContext<I, BC, Icmp> for CC
where
    I: IpExt + datagram::DualStackIpExt,
    BC: IcmpBindingsContext<I, Self::DeviceId>,
    CC: InnerIcmpContext<I, BC> + IcmpStateContext,
{
    type Converter = ();
    fn converter(&self) -> Self::Converter {
        ()
    }
}

impl<I, BC, CC> DatagramBoundStateContext<I, BC, Icmp> for CC
where
    I: IpExt + datagram::DualStackIpExt,
    BC: IcmpBindingsContext<I, Self::DeviceId>,
    CC: InnerIcmpContext<I, BC> + IcmpStateContext,
{
    type IpSocketsCtx<'a> = CC::IpSocketsCtx<'a>;

    // ICMP sockets doesn't support dual-stack operations.
    type DualStackContext = CC::DualStackContext;

    type NonDualStackContext = Self;

    type LocalIdAllocator = LocalIdAllocator<I, Self::WeakDeviceId>;

    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &IcmpBoundSockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(
            self,
            |ctx, BoundSockets { socket_map, allocator: _ }| cb(ctx, &socket_map),
        )
    }

    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut IcmpBoundSockets<I, Self::WeakDeviceId>,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(
            self,
            |ctx, BoundSockets { socket_map, allocator }| cb(ctx, socket_map, allocator),
        )
    }

    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        MaybeDualStack::NotDualStack(self)
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(self, |ctx, _sockets| cb(ctx))
    }
}

impl<I, BC, CC> DatagramStateContext<I, BC, Icmp> for CC
where
    I: IpExt + datagram::DualStackIpExt,
    BC: IcmpBindingsContext<I, Self::DeviceId>,
    CC: StateContext<I, BC> + IcmpStateContext,
{
    type SocketsStateCtx<'a> = CC::SocketStateCtx<'a>;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_sockets_state<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        StateContext::with_sockets_state(self, cb)
    }

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_sockets_state_mut<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &mut SocketsState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        StateContext::with_sockets_state_mut(self, cb)
    }

    /// Calls the function with access to a [`DatagramBoundStateContext`].
    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketsStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        StateContext::with_bound_state_context(self, cb)
    }
}

impl<I: IpExt, D: Id> SocketMapStateSpec for (Icmp, I, D) {
    type ListenerId = SocketId<I>;
    type ConnId = SocketId<I>;

    type AddrVecTag = ();

    type ListenerSharingState = ();
    type ConnSharingState = ();

    type ListenerAddrState = Self::ListenerId;

    type ConnAddrState = Self::ConnId;
    fn listener_tag(
        ListenerAddrInfo { has_device: _, specified_addr: _ }: ListenerAddrInfo,
        _state: &Self::ListenerAddrState,
    ) -> Self::AddrVecTag {
        ()
    }
    fn connected_tag(_has_device: bool, _state: &Self::ConnAddrState) -> Self::AddrVecTag {
        ()
    }
}

impl<I: IpExt> SocketMapAddrStateSpec for SocketId<I> {
    type Id = Self;

    type SharingState = ();

    type Inserter<'a> = core::convert::Infallible
    where
        Self: 'a;

    fn new(_new_sharing_state: &Self::SharingState, id: Self::Id) -> Self {
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

    fn remove_by_id(&mut self, _id: Self::Id) -> socket::RemoveResult {
        socket::RemoveResult::IsLast
    }
}

impl<I: IpExt, D: WeakId> DatagramSocketMapSpec<I, D, IcmpAddrSpec> for (Icmp, I, D) {
    type BoundSocketId = SocketId<I>;
}

impl<AA, I: IpExt, D: WeakId> SocketMapConflictPolicy<AA, (), I, D, IcmpAddrSpec> for (Icmp, I, D)
where
    AA: Into<AddrVec<I, D, IcmpAddrSpec>> + Clone,
{
    fn check_insert_conflicts(
        _new_sharing_state: &(),
        addr: &AA,
        socketmap: &crate::data_structures::socketmap::SocketMap<
            AddrVec<I, D, IcmpAddrSpec>,
            socket::Bound<Self>,
        >,
    ) -> Result<(), socket::InsertError> {
        let addr: AddrVec<_, _, _> = addr.clone().into();
        // Having a value present at a shadowed address is disqualifying.
        if addr.iter_shadows().any(|a| socketmap.get(&a).is_some()) {
            return Err(InsertError::ShadowAddrExists);
        }

        // Likewise, the presence of a value that shadows the target address is
        // also disqualifying.
        if socketmap.descendant_counts(&addr).len() != 0 {
            return Err(InsertError::ShadowerExists);
        }
        Ok(())
    }
}

impl<I: Ip> From<usize> for SocketId<I> {
    fn from(value: usize) -> Self {
        Self(value, IpVersionMarker::default())
    }
}

impl<I: Ip> EntryKey for SocketId<I> {
    fn get_key_index(&self) -> usize {
        let Self(index, _marker) = self;
        *index
    }
}

/// The ICMP Echo sockets API.
pub struct IcmpEchoSocketApi<I: Ip, C>(C, IpVersionMarker<I>);

impl<I: Ip, C> IcmpEchoSocketApi<I, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, IpVersionMarker::new())
    }
}

impl<I, C> IcmpEchoSocketApi<I, C>
where
    I: datagram::IpExt,
    C: ContextPair,
    C::CoreContext: StateContext<I, C::BindingsContext> + IcmpStateContext,
    C::BindingsContext:
        IcmpBindingsContext<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair, IpVersionMarker { .. }) = self;
        pair.core_ctx()
    }

    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, IpVersionMarker { .. }) = self;
        pair.contexts()
    }

    /// Creates a new unbound ICMP socket.
    pub fn create(&mut self) -> SocketId<I> {
        datagram::create(self.core_ctx())
    }

    /// Connects an ICMP socket to remote IP.
    ///
    /// If the socket is never bound, an local ID will be allocated.
    pub fn connect(
        &mut self,
        id: &SocketId<I>,
        remote_ip: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        remote_id: u16,
    ) -> Result<(), datagram::ConnectError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::connect(core_ctx, bindings_ctx, *id, remote_ip, (), remote_id)
    }

    /// Binds an ICMP socket to a local IP address and a local ID.
    ///
    /// Both the IP and the ID are optional. When IP is missing, the "any" IP is
    /// assumed; When the ID is missing, it will be allocated.
    pub fn bind(
        &mut self,
        id: &SocketId<I>,
        local_ip: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        icmp_id: Option<NonZeroU16>,
    ) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::listen(core_ctx, bindings_ctx, *id, local_ip, icmp_id)
    }

    /// Gets the information about an ICMP socket.
    pub fn get_info(
        &mut self,
        id: &SocketId<I>,
    ) -> SocketInfo<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId> {
        StateContext::with_sockets_state(self.core_ctx(), |_core_ctx, state| {
            match state.get(id.get_key_index()).expect("invalid socket ID") {
                datagram::SocketState::Unbound(_) => SocketInfo::Unbound,
                datagram::SocketState::Bound(datagram::BoundSocketState {
                    socket_type,
                    original_bound_addr: _,
                }) => match socket_type {
                    datagram::BoundSocketStateType::Listener { state, sharing: _ } => {
                        let datagram::ListenerState {
                            addr: ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device },
                            ip_options: _,
                        } = state;
                        SocketInfo::Bound {
                            local_ip: addr.map(Into::into),
                            id: *identifier,
                            device: device.clone(),
                        }
                    }
                    datagram::BoundSocketStateType::Connected { state, sharing: _ } => {
                        let datagram::ConnState {
                            addr:
                                ConnAddr {
                                    ip:
                                        ConnIpAddr { local: (local_ip, id), remote: (remote_ip, ()) },
                                    device,
                                },
                            extra: remote_id,
                            ..
                        } = state;
                        SocketInfo::Connected {
                            remote_ip: (*remote_ip).into(),
                            local_ip: (*local_ip).into(),
                            id: *id,
                            device: device.clone(),
                            remote_id: *remote_id,
                        }
                    }
                },
            }
        })
    }

    /// Sets the bound device for a socket.
    ///
    /// Sets the device to be used for sending and receiving packets for a
    /// socket. If the socket is not currently bound to a local address and
    /// port, the device will be used when binding.
    pub fn set_device(
        &mut self,
        id: &SocketId<I>,
        device_id: Option<&<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    ) -> Result<(), SocketError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::set_device(core_ctx, bindings_ctx, *id, device_id)
    }

    /// Gets the device the specified socket is bound to.
    pub fn get_bound_device(
        &mut self,
        id: &SocketId<I>,
    ) -> Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_bound_device(core_ctx, bindings_ctx, *id)
    }

    /// Disconnects an ICMP socket.
    pub fn disconnect(&mut self, id: &SocketId<I>) -> Result<(), datagram::ExpectedConnError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::disconnect_connected(core_ctx, bindings_ctx, *id)
    }

    /// Shuts down an ICMP socket.
    pub fn shutdown(
        &mut self,
        id: &SocketId<I>,
        shutdown_type: ShutdownType,
    ) -> Result<(), datagram::ExpectedConnError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::shutdown_connected(core_ctx, bindings_ctx, *id, shutdown_type)
    }

    /// Gets the current shutdown state of an ICMP socket.
    pub fn get_shutdown(&mut self, id: &SocketId<I>) -> Option<ShutdownType> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_shutdown_connected(core_ctx, bindings_ctx, *id)
    }

    /// Closes an ICMP socket.
    pub fn close(&mut self, id: SocketId<I>) {
        let (core_ctx, bindings_ctx) = self.contexts();
        let _: datagram::SocketInfo<_, _, _> = datagram::close(core_ctx, bindings_ctx, id);
    }

    /// Gets unicast IP hop limit for ICMP sockets.
    pub fn get_unicast_hop_limit(&mut self, id: &SocketId<I>) -> NonZeroU8 {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_ip_hop_limits(core_ctx, bindings_ctx, *id).unicast
    }

    /// Gets multicast IP hop limit for ICMP sockets.
    pub fn get_multicast_hop_limit(&mut self, id: &SocketId<I>) -> NonZeroU8 {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_ip_hop_limits(core_ctx, bindings_ctx, *id).multicast
    }

    /// Sets unicast IP hop limit for ICMP sockets.
    pub fn set_unicast_hop_limit(&mut self, id: &SocketId<I>, hop_limit: Option<NonZeroU8>) {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::update_ip_hop_limit(
            core_ctx,
            bindings_ctx,
            *id,
            SocketHopLimits::set_unicast(hop_limit),
        )
    }

    /// Sets multicast IP hop limit for ICMP sockets.
    pub fn set_multicast_hop_limit(&mut self, id: &SocketId<I>, hop_limit: Option<NonZeroU8>) {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::update_ip_hop_limit(
            core_ctx,
            bindings_ctx,
            *id,
            SocketHopLimits::set_multicast(hop_limit),
        )
    }

    /// Sends an ICMP packet through a connection.
    ///
    /// The socket must be connected in order for the operation to succeed.
    pub fn send<B: BufferMut>(
        &mut self,
        id: &SocketId<I>,
        body: B,
    ) -> Result<(), datagram::SendError<packet_formats::error::ParseError>> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::send_conn::<_, _, _, Icmp, _>(core_ctx, bindings_ctx, *id, body)
    }

    /// Sends an ICMP packet with an remote address.
    ///
    /// The socket doesn't need to be connected.
    pub fn send_to<B: BufferMut>(
        &mut self,
        id: &SocketId<I>,
        remote_ip: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        body: B,
    ) -> Result<
        (),
        either::Either<LocalAddressError, datagram::SendToError<packet_formats::error::ParseError>>,
    > {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::send_to::<_, _, _, Icmp, _>(core_ctx, bindings_ctx, *id, remote_ip, (), body)
    }
}

/// Socket information about an ICMP socket.
#[derive(Debug, GenericOverIp, PartialEq, Eq)]
#[generic_over_ip(A, IpAddress)]
pub enum SocketInfo<A: IpAddress, D> {
    /// The socket is unbound.
    Unbound,
    /// The socket is bound.
    Bound {
        /// The bound local IP address.
        local_ip: Option<SpecifiedAddr<A>>,
        /// The ID field used for ICMP echoes.
        id: NonZeroU16,
        /// An optional device that is bound.
        device: Option<D>,
    },
    /// The socket is connected.
    Connected {
        /// The remote IP address this socket is connected to.
        remote_ip: SpecifiedAddr<A>,
        /// The bound local IP address.
        local_ip: SpecifiedAddr<A>,
        /// The ID field used for ICMP echoes.
        id: NonZeroU16,
        /// An optional device that is bound.
        device: Option<D>,
        /// Unused when sending/receiving packets, but will be reported back to
        /// user.
        remote_id: u16,
    },
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};
    use core::num::NonZeroU16;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::net_ip_v6;
    use net_types::{
        ip::{Ip, Ipv4, Ipv6, Mtu},
        Witness, ZonedAddr,
    };
    use packet::{Buf, Serializer};
    use packet_formats::icmp::{IcmpEchoRequest, IcmpUnusedCode};
    use test_case::test_case;

    use super::*;
    use crate::{
        device::loopback::{LoopbackCreationProperties, LoopbackDevice},
        ip::icmp::tests::FakeIcmpCtx,
        testutil::{handle_queued_rx_packets, TestIpExt, DEFAULT_INTERFACE_METRIC},
    };

    impl<I: Ip> SocketId<I> {
        pub(crate) fn new(id: usize) -> SocketId<I> {
            SocketId(id, IpVersionMarker::default())
        }
    }

    const REMOTE_ID: u16 = 1;

    enum IcmpConnectionType {
        Local,
        Remote,
    }

    enum IcmpSendType {
        Send,
        SendTo,
    }

    // TODO(https://fxbug.dev/42084713): Add test cases with local delivery and a
    // bound device once delivery of looped-back packets is corrected in the
    // socket map.
    #[ip_test]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::Send, true)]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::SendTo, true)]
    #[test_case(IcmpConnectionType::Local, IcmpSendType::Send, false)]
    #[test_case(IcmpConnectionType::Local, IcmpSendType::SendTo, false)]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::Send, false)]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::SendTo, false)]
    #[netstack3_macros::context_ip_bounds(I, crate::testutil::FakeBindingsCtx, crate)]
    fn test_icmp_connection<I: Ip + TestIpExt + datagram::IpExt + crate::marker::IpExt>(
        conn_type: IcmpConnectionType,
        send_type: IcmpSendType,
        bind_to_device: bool,
    ) {
        crate::testutil::set_logger_for_test();

        let config = I::FAKE_CONFIG;

        const LOCAL_CTX_NAME: &str = "alice";
        const REMOTE_CTX_NAME: &str = "bob";
        let (local, local_device_ids) = I::FAKE_CONFIG.into_builder().build();
        let (remote, remote_device_ids) = I::FAKE_CONFIG.swap().into_builder().build();
        let mut net = crate::context::testutil::new_simple_fake_network(
            LOCAL_CTX_NAME,
            local,
            local_device_ids[0].downgrade(),
            REMOTE_CTX_NAME,
            remote,
            remote_device_ids[0].downgrade(),
        );

        let icmp_id = 13;

        let (remote_addr, ctx_name_receiving_req) = match conn_type {
            IcmpConnectionType::Local => (config.local_ip, LOCAL_CTX_NAME),
            IcmpConnectionType::Remote => (config.remote_ip, REMOTE_CTX_NAME),
        };

        let loopback_device_id = net.with_context(LOCAL_CTX_NAME, |ctx| {
            ctx.core_api()
                .device::<LoopbackDevice>()
                .add_device_with_default_state(
                    LoopbackCreationProperties { mtu: Mtu::new(u16::MAX as u32) },
                    DEFAULT_INTERFACE_METRIC,
                )
                .into()
        });

        let echo_body = vec![1, 2, 3, 4];
        let buf = Buf::new(echo_body.clone(), ..)
            .encapsulate(IcmpPacketBuilder::<I, _>::new(
                *config.local_ip,
                *remote_addr,
                IcmpUnusedCode,
                IcmpEchoRequest::new(0, 1),
            ))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();
        let conn = net.with_context(LOCAL_CTX_NAME, |ctx| {
            crate::device::testutil::enable_device(ctx, &loopback_device_id);
            let mut socket_api = ctx.core_api().icmp_echo::<I>();
            let conn = socket_api.create();
            if bind_to_device {
                let device = local_device_ids[0].clone().into();
                socket_api.set_device(&conn, Some(&device)).expect("failed to set SO_BINDTODEVICE");
            }
            core::mem::drop((local_device_ids, remote_device_ids));
            socket_api.bind(&conn, None, NonZeroU16::new(icmp_id)).unwrap();
            match send_type {
                IcmpSendType::Send => {
                    socket_api
                        .connect(&conn, Some(ZonedAddr::Unzoned(remote_addr)), REMOTE_ID)
                        .unwrap();
                    socket_api.send(&conn, buf).unwrap();
                }
                IcmpSendType::SendTo => {
                    socket_api.send_to(&conn, Some(ZonedAddr::Unzoned(remote_addr)), buf).unwrap();
                }
            }
            handle_queued_rx_packets(ctx);

            conn
        });

        net.run_until_idle(crate::device::testutil::receive_frame, |ctx, _, id| {
            ctx.core_api().handle_timer(id);
            handle_queued_rx_packets(ctx);
        });

        assert_eq!(net.core_ctx(LOCAL_CTX_NAME).state.icmp_rx_counters::<I>().echo_reply.get(), 1);
        assert_eq!(
            net.core_ctx(ctx_name_receiving_req).state.icmp_rx_counters::<I>().echo_request.get(),
            1
        );
        let replies = net.bindings_ctx(LOCAL_CTX_NAME).take_icmp_replies(conn);
        let expected = Buf::new(echo_body, ..)
            .encapsulate(IcmpPacketBuilder::<I, _>::new(
                *config.local_ip,
                *remote_addr,
                IcmpUnusedCode,
                packet_formats::icmp::IcmpEchoReply::new(icmp_id, 1),
            ))
            .serialize_vec_outer()
            .unwrap()
            .into_inner()
            .into_inner();
        assert_matches!(&replies[..], [body] if *body == expected);
    }

    #[test]
    fn test_connect_dual_stack_fails() {
        // Verify that connecting to an ipv4-mapped-ipv6 address fails, as ICMP
        // sockets do not support dual-stack operations.
        let mut ctx = FakeIcmpCtx::<Ipv6>::default();
        let mut api = IcmpEchoSocketApi::<Ipv6, _>::new(ctx.as_mut());
        let conn = api.create();
        assert_eq!(
            api.connect(
                &conn,
                Some(ZonedAddr::Unzoned(
                    SpecifiedAddr::new(net_ip_v6!("::ffff:192.0.2.1")).unwrap(),
                )),
                REMOTE_ID,
            ),
            Err(datagram::ConnectError::RemoteUnexpectedlyMapped)
        );
    }

    #[ip_test]
    fn send_invalid_icmp_echo<I: Ip + TestIpExt + datagram::IpExt>() {
        let mut ctx = FakeIcmpCtx::<I>::default();
        let mut api = IcmpEchoSocketApi::<I, _>::new(ctx.as_mut());
        let conn = api.create();
        api.connect(&conn, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), REMOTE_ID).unwrap();

        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(IcmpPacketBuilder::<I, _>::new(
                I::FAKE_CONFIG.local_ip.get(),
                I::FAKE_CONFIG.remote_ip.get(),
                IcmpUnusedCode,
                packet_formats::icmp::IcmpEchoReply::new(0, 1),
            ))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();
        assert_matches!(
            api.send(&conn, buf),
            Err(datagram::SendError::SerializeError(
                packet_formats::error::ParseError::NotExpected
            ))
        );
    }

    #[ip_test]
    fn get_info<I: Ip + TestIpExt + datagram::IpExt>() {
        let mut ctx = FakeIcmpCtx::<I>::default();
        let mut api = IcmpEchoSocketApi::<I, _>::new(ctx.as_mut());
        const ICMP_ID: NonZeroU16 = const_unwrap::const_unwrap_option(NonZeroU16::new(1));

        let id = api.create();
        assert_eq!(api.get_info(&id), SocketInfo::Unbound);

        api.bind(&id, None, Some(ICMP_ID)).unwrap();
        assert_eq!(
            api.get_info(&id),
            SocketInfo::Bound { local_ip: None, id: ICMP_ID, device: None }
        );

        api.connect(&id, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), REMOTE_ID).unwrap();
        assert_eq!(
            api.get_info(&id),
            SocketInfo::Connected {
                local_ip: I::FAKE_CONFIG.local_ip,
                remote_ip: I::FAKE_CONFIG.remote_ip,
                id: ICMP_ID,
                device: None,
                remote_id: REMOTE_ID,
            }
        );
    }
}
