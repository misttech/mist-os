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
    ip::{GenericOverIp, Ip, IpAddress, IpInvariant, IpVersionMarker},
    SpecifiedAddr,
};
use packet::{BufferMut, Serializer};
use packet_formats::{
    icmp::{IcmpEchoRequest, IcmpPacketBuilder},
    ip::{IpProtoExt, Ipv4Proto, Ipv6Proto},
};

use crate::{
    algorithm::{PortAlloc, PortAllocImpl},
    context::RngContext,
    data_structures::socketmap::IterShadows as _,
    device::{AnyDevice, DeviceId, DeviceIdContext, Id, WeakId},
    error::{LocalAddressError, SocketError},
    ip::{
        icmp::{IcmpAddr, IcmpBindingsContext, IcmpIpExt, IcmpStateContext, InnerIcmpContext},
        socket::IpSock,
        IpExt,
    },
    socket::{
        self,
        address::{ConnAddr, ConnIpAddr, ListenerAddr, ListenerIpAddr, SocketZonedIpAddr},
        datagram::{
            self, DatagramBoundStateContext, DatagramFlowId, DatagramSocketMapSpec,
            DatagramSocketSpec, DatagramStateContext, ExpectedUnboundError, SocketHopLimits,
        },
        AddrVec, IncompatibleError, InsertError, ListenerAddrInfo, MaybeDualStack, ShutdownType,
        SocketMapAddrSpec, SocketMapAddrStateSpec, SocketMapConflictPolicy, SocketMapStateSpec,
    },
    sync::RwLock,
    BindingsContext, CoreCtx, SyncCtx,
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
pub(crate) trait StateContext<I: IcmpIpExt + IpExt, BC: IcmpBindingsContext<I, Self::DeviceId>>:
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
        // TODO(https://fxbug.dev/47321): Instead of panic, make this trait
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

// TODO(https://fxbug.dev/133884): Remove the laziness by dropping `Option`.
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

/// A handler trait for ICMP sockets.
pub(crate) trait SocketHandler<I: datagram::IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Creates a new ICMP socket.
    fn create(&mut self) -> SocketId<I>;

    /// Connects an ICMP socket with a remote IP address.
    fn connect(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        remote_id: u16,
    ) -> Result<(), datagram::ConnectError>;

    /// Binds an ICMP socket to a local IP address and a local ID to send/recv
    /// ICMP echo replies on.
    fn bind(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        local_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        icmp_id: Option<NonZeroU16>,
    ) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>>;

    /// Gets the socket information of an ICMP socket.
    fn get_info(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId>;

    fn set_device(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        device_id: Option<&Self::DeviceId>,
    ) -> Result<(), SocketError>;

    fn get_bound_device(
        &mut self,
        bindings_ctx: &BC,
        id: &SocketId<I>,
    ) -> Option<Self::WeakDeviceId>;

    /// Disconnects an ICMP socket.
    fn disconnect(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
    ) -> Result<(), datagram::ExpectedConnError>;

    /// Shuts down an ICMP socket.
    fn shutdown(
        &mut self,
        bindings_ctx: &BC,
        id: &SocketId<I>,
        shutdown_type: ShutdownType,
    ) -> Result<(), datagram::ExpectedConnError>;

    /// Gets the current shutdown state of the ICMP socket.
    fn get_shutdown(&mut self, bindings_ctx: &BC, id: &SocketId<I>) -> Option<ShutdownType>;

    /// Closes the ICMP socket.
    fn close(&mut self, bindings_ctx: &mut BC, id: SocketId<I>);

    /// Gets the unicast hop limit.
    fn get_unicast_hop_limit(&mut self, bindings_ctx: &BC, id: &SocketId<I>) -> NonZeroU8;

    /// Gets the multicast hop limit.
    fn get_multicast_hop_limit(&mut self, bindings_ctx: &BC, id: &SocketId<I>) -> NonZeroU8;

    /// Sets the unicast hop limit.
    fn set_unicast_hop_limit(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        hop_limit: Option<NonZeroU8>,
    );

    /// Sets the multicast hop limit.
    fn set_multicast_hop_limit(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        hop_limit: Option<NonZeroU8>,
    );

    /// Send an ICMP echo reply on the socket.
    fn send<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        conn: &SocketId<I>,
        body: B,
    ) -> Result<(), datagram::SendError<packet_formats::error::ParseError>>;

    /// Send an ICMP echo reply to a remote address without connecting.
    fn send_to<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        body: B,
    ) -> Result<
        (),
        either::Either<LocalAddressError, datagram::SendToError<packet_formats::error::ParseError>>,
    >;
}

impl<
        I: datagram::IpExt,
        BC: IcmpBindingsContext<I, Self::DeviceId>,
        CC: StateContext<I, BC> + IcmpStateContext,
    > SocketHandler<I, BC> for CC
{
    fn create(&mut self) -> SocketId<I> {
        datagram::create(self)
    }
    fn connect(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        remote_id: u16,
    ) -> Result<(), datagram::ConnectError> {
        datagram::connect(self, bindings_ctx, id.clone(), remote_ip, (), remote_id)
    }

    fn bind(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        local_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        icmp_id: Option<NonZeroU16>,
    ) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
        datagram::listen(self, bindings_ctx, id.clone(), local_ip, icmp_id)
    }

    fn get_info(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId> {
        self.with_sockets_state(|_core_ctx, state| {
            match state.get(id.get_key_index()).expect("invalid socket ID") {
                datagram::SocketState::Unbound(_) => SocketInfo::Unbound,
                datagram::SocketState::Bound(
                    datagram::BoundSocketState{socket_type, original_bound_addr: _}
                ) => match socket_type {
                    datagram::BoundSocketStateType::Listener {
                        state,
                        sharing: _,
                    } => {
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
                    datagram::BoundSocketStateType::Connected {
                        state,
                        sharing: _,
                    } => {
                        let datagram::ConnState {
                            addr:
                                ConnAddr {
                                    ip: ConnIpAddr { local: (local_ip, id), remote: (remote_ip, ()) },
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
                }
            }
        })
    }

    fn set_device(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        device_id: Option<&Self::DeviceId>,
    ) -> Result<(), SocketError> {
        datagram::set_device(self, bindings_ctx, id.clone(), device_id)
    }

    fn get_bound_device(
        &mut self,
        bindings_ctx: &BC,
        id: &SocketId<I>,
    ) -> Option<Self::WeakDeviceId> {
        datagram::get_bound_device(self, bindings_ctx, id.clone())
    }

    fn disconnect(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
    ) -> Result<(), datagram::ExpectedConnError> {
        datagram::disconnect_connected(self, bindings_ctx, id.clone())
    }

    fn shutdown(
        &mut self,
        bindings_ctx: &BC,
        id: &SocketId<I>,
        shutdown_type: ShutdownType,
    ) -> Result<(), datagram::ExpectedConnError> {
        datagram::shutdown_connected(self, bindings_ctx, id.clone(), shutdown_type)
    }

    fn get_shutdown(&mut self, bindings_ctx: &BC, id: &SocketId<I>) -> Option<ShutdownType> {
        datagram::get_shutdown_connected(self, bindings_ctx, id.clone())
    }

    fn close(&mut self, bindings_ctx: &mut BC, id: SocketId<I>) {
        let _: datagram::SocketInfo<_, _, _> = datagram::close(self, bindings_ctx, id);
    }

    fn get_unicast_hop_limit(&mut self, bindings_ctx: &BC, id: &SocketId<I>) -> NonZeroU8 {
        datagram::get_ip_hop_limits(self, bindings_ctx, id.clone()).unicast
    }

    fn get_multicast_hop_limit(&mut self, bindings_ctx: &BC, id: &SocketId<I>) -> NonZeroU8 {
        datagram::get_ip_hop_limits(self, bindings_ctx, id.clone()).multicast
    }

    fn set_unicast_hop_limit(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        hop_limit: Option<NonZeroU8>,
    ) {
        datagram::update_ip_hop_limit(
            self,
            bindings_ctx,
            id.clone(),
            SocketHopLimits::set_unicast(hop_limit),
        )
    }

    fn set_multicast_hop_limit(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        hop_limit: Option<NonZeroU8>,
    ) {
        datagram::update_ip_hop_limit(
            self,
            bindings_ctx,
            id.clone(),
            SocketHopLimits::set_multicast(hop_limit),
        )
    }

    fn send<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        body: B,
    ) -> Result<(), datagram::SendError<packet_formats::error::ParseError>> {
        datagram::send_conn::<_, _, _, Icmp, _>(self, bindings_ctx, id.clone(), body)
    }

    fn send_to<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        id: &SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        body: B,
    ) -> Result<
        (),
        either::Either<LocalAddressError, datagram::SendToError<packet_formats::error::ParseError>>,
    > {
        datagram::send_to::<_, _, _, Icmp, _>(self, bindings_ctx, id.clone(), remote_ip, (), body)
    }
}

// TODO(https://fxbug.dev/42083910): The following freestanding functions are
// boilerplates that become too much in all socket modules, we need to reduce
// the need for them.

/// Creates a new unbound ICMP socket.
pub fn new_socket<I: Ip, BC: BindingsContext>(core_ctx: &SyncCtx<BC>) -> SocketId<I> {
    net_types::map_ip_twice!(I, IpInvariant(core_ctx), |IpInvariant(core_ctx)| {
        SocketHandler::<I, BC>::create(&mut CoreCtx::new_deprecated(core_ctx))
    },)
}

/// Connects an ICMP socket to remote IP.
///
/// If the socket is never bound, an local ID will be allocated.
pub fn connect<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, DeviceId<BC>>>,
    remote_id: u16,
) -> Result<(), datagram::ConnectError> {
    let IpInvariant(result) = net_types::map_ip_twice!(
        I,
        (IpInvariant((core_ctx, bindings_ctx, remote_id)), id, remote_ip),
        |(IpInvariant((core_ctx, bindings_ctx, remote_id)), id, remote_ip)| {
            IpInvariant(SocketHandler::<I, BC>::connect(
                &mut CoreCtx::new_deprecated(core_ctx),
                bindings_ctx,
                id,
                remote_ip,
                remote_id,
            ))
        }
    );
    result
}

/// Binds an ICMP socket to a local IP address and a local ID.
///
/// Both the IP and the ID are optional. When IP is missing, the "any" IP is
/// assumed; When the ID is missing, it will be allocated.
pub fn bind<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
    local_ip: Option<SocketZonedIpAddr<I::Addr, DeviceId<BC>>>,
    icmp_id: Option<NonZeroU16>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
    let IpInvariant(result) = net_types::map_ip_twice!(
        I,
        (IpInvariant((core_ctx, bindings_ctx, icmp_id)), id, local_ip),
        |(IpInvariant((core_ctx, bindings_ctx, icmp_id)), id, local_ip)| {
            IpInvariant(SocketHandler::<I, _>::bind(
                &mut CoreCtx::new_deprecated(core_ctx),
                bindings_ctx,
                id,
                local_ip,
                icmp_id,
            ))
        }
    );
    result
}

/// Sends an ICMP packet through a connection.
///
/// The socket must be connected in order for the operation to succeed.
pub fn send<I: Ip, B: BufferMut, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
    body: B,
) -> Result<(), datagram::SendError<packet_formats::error::ParseError>> {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx, body)), id), |(
        IpInvariant((core_ctx, bindings_ctx, body)),
        id,
    )| {
        SocketHandler::<I, BC>::send(&mut CoreCtx::new_deprecated(core_ctx), bindings_ctx, id, body)
    })
}

/// Sends an ICMP packet with an remote address.
///
/// The socket doesn't need to be connected.
pub fn send_to<I: Ip, B: BufferMut, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, DeviceId<BC>>>,
    body: B,
) -> Result<
    (),
    either::Either<LocalAddressError, datagram::SendToError<packet_formats::error::ParseError>>,
> {
    let IpInvariant(result) = net_types::map_ip_twice!(
        I,
        (IpInvariant((core_ctx, bindings_ctx, body)), (remote_ip, id)),
        |(IpInvariant((core_ctx, bindings_ctx, body)), (remote_ip, id))| {
            IpInvariant(SocketHandler::<I, BC>::send_to(
                &mut CoreCtx::new_deprecated(core_ctx),
                bindings_ctx,
                id,
                remote_ip,
                body,
            ))
        }
    );
    result
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

/// Gets the information about an ICMP socket.
pub fn get_info<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
) -> SocketInfo<I::Addr, crate::device::WeakDeviceId<BC>> {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx)), id), |(
        IpInvariant((core_ctx, bindings_ctx)),
        id,
    )| {
        SocketHandler::<I, BC>::get_info(&mut CoreCtx::new_deprecated(core_ctx), bindings_ctx, id)
    })
}

/// Sets the bound device for a socket.
///
/// Sets the device to be used for sending and receiving packets for a socket.
/// If the socket is not currently bound to a local address and port, the device
/// will be used when binding.
pub fn set_device<I: Ip, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
    device_id: Option<&DeviceId<BC>>,
) -> Result<(), SocketError> {
    let IpInvariant(result) = net_types::map_ip_twice!(
        I,
        (IpInvariant((core_ctx, bindings_ctx, device_id)), id),
        |(IpInvariant((core_ctx, bindings_ctx, device_id)), id)| {
            IpInvariant(SocketHandler::<I, _>::set_device(
                &mut CoreCtx::new_deprecated(core_ctx),
                bindings_ctx,
                id,
                device_id,
            ))
        },
    );
    result
}

/// Gets the device the specified socket is bound to.
pub fn get_bound_device<I: Ip, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &BC,
    id: &SocketId<I>,
) -> Option<crate::device::WeakDeviceId<BC>> {
    let IpInvariant(device) = net_types::map_ip_twice!(
        I,
        (IpInvariant((core_ctx, bindings_ctx)), id),
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            IpInvariant(SocketHandler::<I, _>::get_bound_device(
                &mut CoreCtx::new_deprecated(core_ctx),
                bindings_ctx,
                id,
            ))
        }
    );
    device
}

/// Disconnects an ICMP socket.
pub fn disconnect<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
) -> Result<(), datagram::ExpectedConnError> {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx)), id), |(
        IpInvariant((core_ctx, bindings_ctx)),
        id,
    )| {
        SocketHandler::<I, BC>::disconnect(&mut CoreCtx::new_deprecated(core_ctx), bindings_ctx, id)
    })
}

/// Shuts down an ICMP socket.
pub fn shutdown<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &BC,
    id: &SocketId<I>,
    shutdown_type: ShutdownType,
) -> Result<(), datagram::ExpectedConnError> {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx, shutdown_type)), id), |(
        IpInvariant((core_ctx, bindings_ctx, shutdown_type)),
        id,
    )| {
        SocketHandler::<I, BC>::shutdown(
            &mut CoreCtx::new_deprecated(core_ctx),
            bindings_ctx,
            id,
            shutdown_type,
        )
    })
}

/// Gets the current shutdown state of an ICMP socket.
pub fn get_shutdown<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &BC,
    id: &SocketId<I>,
) -> Option<ShutdownType> {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx)), id), |(
        IpInvariant((core_ctx, bindings_ctx)),
        id,
    )| {
        SocketHandler::<I, BC>::get_shutdown(
            &mut CoreCtx::new_deprecated(core_ctx),
            bindings_ctx,
            id,
        )
    })
}

/// closes an ICMP socket.
pub fn close<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: SocketId<I>,
) {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx)), id), |(
        IpInvariant((core_ctx, bindings_ctx)),
        id,
    )| {
        SocketHandler::<I, BC>::close(&mut CoreCtx::new_deprecated(core_ctx), bindings_ctx, id)
    })
}

/// Sets unicast IP hop limit for ICMP sockets.
pub fn set_unicast_hop_limit<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
    hop_limit: Option<NonZeroU8>,
) {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx, hop_limit)), id), |(
        IpInvariant((core_ctx, bindings_ctx, hop_limit)),
        id,
    )| {
        SocketHandler::<I, BC>::set_unicast_hop_limit(
            &mut CoreCtx::new_deprecated(core_ctx),
            bindings_ctx,
            id,
            hop_limit,
        )
    })
}

/// Sets multicast IP hop limit for ICMP sockets.
pub fn set_multicast_hop_limit<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &SocketId<I>,
    hop_limit: Option<NonZeroU8>,
) {
    net_types::map_ip_twice!(I, (IpInvariant((core_ctx, bindings_ctx, hop_limit)), id), |(
        IpInvariant((core_ctx, bindings_ctx, hop_limit)),
        id,
    )| {
        SocketHandler::<I, BC>::set_multicast_hop_limit(
            &mut CoreCtx::new_deprecated(core_ctx),
            bindings_ctx,
            id,
            hop_limit,
        )
    })
}

/// Gets unicast IP hop limit for ICMP sockets.
pub fn get_unicast_hop_limit<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &BC,
    id: &SocketId<I>,
) -> NonZeroU8 {
    let IpInvariant(hop_limit) = net_types::map_ip_twice!(
        I,
        (IpInvariant((core_ctx, bindings_ctx)), id),
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            IpInvariant(SocketHandler::<I, BC>::get_unicast_hop_limit(
                &mut CoreCtx::new_deprecated(core_ctx),
                bindings_ctx,
                id,
            ))
        }
    );
    hop_limit
}

/// Gets multicast IP hop limit for ICMP sockets.
pub fn get_multicast_hop_limit<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &BC,
    id: &SocketId<I>,
) -> NonZeroU8 {
    let IpInvariant(hop_limit) = net_types::map_ip_twice!(
        I,
        (IpInvariant((core_ctx, bindings_ctx)), id),
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            IpInvariant(SocketHandler::<I, BC>::get_multicast_hop_limit(
                &mut CoreCtx::new_deprecated(core_ctx),
                bindings_ctx,
                id,
            ))
        }
    );
    hop_limit
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
        context::testutil::FakeCtxWithCoreCtx,
        ip::icmp::tests::FakeIcmpCtx,
        testutil::{handle_queued_rx_packets, Ctx, TestIpExt, DEFAULT_INTERFACE_METRIC},
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

    // TODO(https://fxbug.dev/135041): Add test cases with local delivery and a
    // bound device once delivery of looped-back packets is corrected in the
    // socket map.
    #[ip_test]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::Send, true)]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::SendTo, true)]
    #[test_case(IcmpConnectionType::Local, IcmpSendType::Send, false)]
    #[test_case(IcmpConnectionType::Local, IcmpSendType::SendTo, false)]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::Send, false)]
    #[test_case(IcmpConnectionType::Remote, IcmpSendType::SendTo, false)]
    fn test_icmp_connection<I: Ip + TestIpExt + datagram::IpExt>(
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

        let loopback_device_id =
            net.with_context(LOCAL_CTX_NAME, |Ctx { core_ctx, bindings_ctx: _ }| {
                crate::device::add_loopback_device(
                    &&*core_ctx,
                    Mtu::new(u16::MAX as u32),
                    DEFAULT_INTERFACE_METRIC,
                )
                .expect("create the loopback interface")
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
        let conn = net.with_context(LOCAL_CTX_NAME, |Ctx { core_ctx, bindings_ctx }| {
            crate::device::testutil::enable_device(&&*core_ctx, bindings_ctx, &loopback_device_id);

            let conn = new_socket::<I, _>(core_ctx);
            if bind_to_device {
                let device = local_device_ids[0].clone().into();
                set_device(core_ctx, bindings_ctx, &conn, Some(&device))
                    .expect("failed to set SO_BINDTODEVICE");
            }
            core::mem::drop((local_device_ids, remote_device_ids));
            bind(core_ctx, bindings_ctx, &conn, None, NonZeroU16::new(icmp_id)).unwrap();
            match send_type {
                IcmpSendType::Send => {
                    connect(
                        core_ctx,
                        bindings_ctx,
                        &conn,
                        Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(remote_addr))),
                        REMOTE_ID,
                    )
                    .unwrap();
                    send(core_ctx, bindings_ctx, &conn, buf).unwrap();
                }
                IcmpSendType::SendTo => {
                    send_to(
                        core_ctx,
                        bindings_ctx,
                        &conn,
                        Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(remote_addr))),
                        buf,
                    )
                    .unwrap();
                }
            }
            handle_queued_rx_packets(core_ctx, bindings_ctx);

            conn
        });

        net.run_until_idle(
            crate::device::testutil::receive_frame,
            |Ctx { core_ctx, bindings_ctx }, _, id| {
                crate::handle_timer(&&*core_ctx, bindings_ctx, id);
                handle_queued_rx_packets(core_ctx, bindings_ctx);
            },
        );

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
        let FakeCtxWithCoreCtx { core_ctx, bindings_ctx } = &mut ctx;
        let conn = SocketHandler::<Ipv6, _>::create(core_ctx);
        assert_eq!(
            SocketHandler::<Ipv6, _>::connect(
                core_ctx,
                bindings_ctx,
                &conn,
                Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(
                    SpecifiedAddr::new(net_ip_v6!("::ffff:192.0.2.1")).unwrap(),
                ))),
                REMOTE_ID,
            ),
            Err(datagram::ConnectError::RemoteUnexpectedlyMapped)
        );
    }

    #[ip_test]
    fn send_invalid_icmp_echo<I: Ip + TestIpExt + datagram::IpExt>() {
        let mut ctx = FakeIcmpCtx::<I>::default();
        let FakeCtxWithCoreCtx { core_ctx, bindings_ctx } = &mut ctx;
        let conn = SocketHandler::<I, _>::create(core_ctx);
        SocketHandler::<I, _>::connect(
            core_ctx,
            bindings_ctx,
            &conn,
            Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip))),
            REMOTE_ID,
        )
        .unwrap();

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
            SocketHandler::<I, _>::send(core_ctx, bindings_ctx, &conn, buf,),
            Err(datagram::SendError::SerializeError(
                packet_formats::error::ParseError::NotExpected
            ))
        );
    }

    #[ip_test]
    fn get_info<I: Ip + TestIpExt + datagram::IpExt>() {
        let mut ctx = FakeIcmpCtx::<I>::default();
        let FakeCtxWithCoreCtx { core_ctx, bindings_ctx } = &mut ctx;
        const ICMP_ID: NonZeroU16 = const_unwrap::const_unwrap_option(NonZeroU16::new(1));

        let id = SocketHandler::<I, _>::create(core_ctx);
        assert_eq!(
            SocketHandler::<I, _>::get_info(core_ctx, bindings_ctx, &id),
            SocketInfo::Unbound
        );

        SocketHandler::<I, _>::bind(core_ctx, bindings_ctx, &id, None, Some(ICMP_ID)).unwrap();
        assert_eq!(
            SocketHandler::<I, _>::get_info(core_ctx, bindings_ctx, &id),
            SocketInfo::Bound { local_ip: None, id: ICMP_ID, device: None }
        );

        SocketHandler::<I, _>::connect(
            core_ctx,
            bindings_ctx,
            &id,
            Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip))),
            REMOTE_ID,
        )
        .unwrap();
        assert_eq!(
            SocketHandler::<I, _>::get_info(core_ctx, bindings_ctx, &id),
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
