// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! ICMP Echo Sockets.

use alloc::vec::Vec;
use core::{
    borrow::Borrow,
    convert::Infallible as Never,
    fmt::Debug,
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU8},
};

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
    algorithm::{self, PortAllocImpl},
    context::{ContextPair, RngContext},
    data_structures::socketmap::IterShadows as _,
    device::{self, AnyDevice, DeviceIdContext},
    error::{LocalAddressError, SocketError},
    inspect::{Inspector, InspectorDeviceExt},
    ip::{
        icmp::{IcmpAddr, IcmpBindingsContext, IcmpIpExt, IcmpStateContext, InnerIcmpContext},
        socket::IpSock,
    },
    socket::{
        self,
        address::{ConnAddr, ConnInfoAddr, ConnIpAddr},
        datagram::{
            self, DatagramBoundStateContext, DatagramFlowId, DatagramSocketMapSpec,
            DatagramSocketSet, DatagramSocketSpec, DatagramStateContext, ExpectedUnboundError,
            SocketHopLimits,
        },
        AddrVec, IncompatibleError, InsertError, ListenerAddrInfo, MaybeDualStack, ShutdownType,
        SocketMapAddrSpec, SocketMapAddrStateSpec, SocketMapConflictPolicy, SocketMapStateSpec,
    },
    sync::{RemoveResourceResultWithContext, RwLock, StrongRc},
};

/// A marker trait for all IP extensions required by ICMP sockets.
pub trait IpExt: datagram::IpExt + IcmpIpExt {}
impl<O: datagram::IpExt + IcmpIpExt> IpExt for O {}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct IcmpSockets<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> {
    pub(crate) bound_and_id_allocator: RwLock<BoundSockets<I, D, BT>>,
    // Destroy all_sockets last so the strong references in the demux are
    // dropped before the primary references in the set.
    pub(crate) all_sockets: RwLock<IcmpSocketSet<I, D, BT>>,
}

/// An ICMP socket.
#[derive(GenericOverIp, Derivative)]
#[derivative(Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[generic_over_ip(I, Ip)]

pub struct IcmpSocketId<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes>(
    datagram::StrongRc<I, D, Icmp<BT>>,
);

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> Clone for IcmpSocketId<I, D, BT> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    fn clone(&self) -> Self {
        let Self(rc) = self;
        Self(StrongRc::clone(rc))
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes>
    From<datagram::StrongRc<I, D, Icmp<BT>>> for IcmpSocketId<I, D, BT>
{
    fn from(value: datagram::StrongRc<I, D, Icmp<BT>>) -> Self {
        Self(value)
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes>
    Borrow<datagram::StrongRc<I, D, Icmp<BT>>> for IcmpSocketId<I, D, BT>
{
    fn borrow(&self) -> &datagram::StrongRc<I, D, Icmp<BT>> {
        let Self(rc) = self;
        rc
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> PartialEq<WeakIcmpSocketId<I, D, BT>>
    for IcmpSocketId<I, D, BT>
{
    fn eq(&self, other: &WeakIcmpSocketId<I, D, BT>) -> bool {
        let Self(rc) = self;
        let WeakIcmpSocketId(weak) = other;
        StrongRc::weak_ptr_eq(rc, weak)
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> Debug for IcmpSocketId<I, D, BT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("IcmpSocketId").field(&StrongRc::debug_id(rc)).finish()
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> IcmpSocketId<I, D, BT> {
    /// Returns the inner state for this socket, to be used in conjunction with
    /// lock ordering mechanisms.
    pub(crate) fn state_for_locking(&self) -> &RwLock<IcmpSocketState<I, D, BT>> {
        let Self(rc) = self;
        &rc.state
    }

    /// Returns a means to debug outstanding references to this socket.
    pub fn debug_references(&self) -> impl Debug {
        let Self(rc) = self;
        StrongRc::debug_references(rc)
    }

    /// Downgrades this ID to a weak reference.
    pub fn downgrade(&self) -> WeakIcmpSocketId<I, D, BT> {
        let Self(rc) = self;
        WeakIcmpSocketId(StrongRc::downgrade(rc))
    }

    /// Returns external data associated with this socket.
    pub fn external_data(&self) -> &BT::ExternalData<I> {
        let Self(rc) = self;
        &rc.external_data
    }
}

/// A weak reference to an ICMP socket.
#[derive(GenericOverIp, Derivative)]
#[derivative(Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""), Clone(bound = ""))]
#[generic_over_ip(I, Ip)]
pub struct WeakIcmpSocketId<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes>(
    datagram::WeakRc<I, D, Icmp<BT>>,
);

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> PartialEq<IcmpSocketId<I, D, BT>>
    for WeakIcmpSocketId<I, D, BT>
{
    fn eq(&self, other: &IcmpSocketId<I, D, BT>) -> bool {
        PartialEq::eq(other, self)
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> Debug for WeakIcmpSocketId<I, D, BT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("WeakIcmpSocketId").field(&rc.debug_id()).finish()
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> WeakIcmpSocketId<I, D, BT> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    pub fn upgrade(&self) -> Option<IcmpSocketId<I, D, BT>> {
        let Self(rc) = self;
        rc.upgrade().map(IcmpSocketId)
    }
}

pub(crate) type IcmpSocketSet<I, D, BT> = DatagramSocketSet<I, D, Icmp<BT>>;
pub(crate) type IcmpSocketState<I, D, BT> = datagram::SocketState<I, D, Icmp<BT>>;

#[derive(Clone)]
pub(crate) struct IcmpConn<S> {
    icmp_id: u16,
    ip: S,
}

impl<'a, A: IpAddress, D> From<&'a IcmpConn<IpSock<A::Version, D>>> for IcmpAddr<A>
where
    A::Version: IpExt,
{
    fn from(conn: &'a IcmpConn<IpSock<A::Version, D>>) -> IcmpAddr<A> {
        IcmpAddr {
            local_addr: *conn.ip.local_ip(),
            remote_addr: *conn.ip.remote_ip(),
            icmp_id: conn.icmp_id,
        }
    }
}

/// The context required by the ICMP layer in order to deliver events related to
/// ICMP sockets.
pub trait IcmpEchoBindingsContext<I: IpExt, D: device::StrongId>: IcmpEchoBindingsTypes {
    /// Receives an ICMP echo reply.
    fn receive_icmp_echo_reply<B: BufferMut>(
        &mut self,
        conn: &IcmpSocketId<I, D::Weak, Self>,
        device_id: &D,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        id: u16,
        data: B,
    );
}

/// The bindings context providing external types to ICMP sockets.
///
/// # Discussion
///
/// We'd like this trait to take an `I` type parameter instead of using GAT to
/// get the IP version, however we end up with problems due to the shape of
/// [`DatagramSocketSpec`] and the underlying support for dual stack sockets.
///
/// This is completely fine for all known implementations, except for a rough
/// edge in fake tests bindings contexts that are already parameterized on I
/// themselves. This is still better than relying on `Box<dyn Any>` to keep the
/// external data in our references so we take the rough edge.
pub trait IcmpEchoBindingsTypes: Sized + 'static {
    /// Opaque bindings data held by core for a given IP version.
    type ExternalData<I: Ip>: Debug + Send + Sync + 'static;
}

/// A Context that provides access to the sockets' states.
pub trait StateContext<I: IcmpIpExt + IpExt, BC: IcmpBindingsContext<I, Self::DeviceId>>:
    DeviceIdContext<AnyDevice>
{
    type SocketStateCtx<'a>: InnerIcmpContext<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + IcmpStateContext;

    /// Calls the function with mutable access to the set with all ICMP
    /// sockets.
    fn with_all_sockets_mut<O, F: FnOnce(&mut IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with immutable access to the set with all ICMP
    /// sockets.
    fn with_all_sockets<O, F: FnOnce(&IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function without access to ICMP socket state.
    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with an immutable reference to the given socket's
    /// state.
    fn with_socket_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &IcmpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the given socket's state.
    fn with_socket_state_mut<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut IcmpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O;

    /// Call `f` with each socket's state.
    fn for_each_socket<
        F: FnMut(
            &mut Self::SocketStateCtx<'_>,
            &IcmpSocketId<I, Self::WeakDeviceId, BC>,
            &IcmpSocketState<I, Self::WeakDeviceId, BC>,
        ),
    >(
        &mut self,
        cb: F,
    );
}

/// Uninstantiatable type for implementing [`DatagramSocketSpec`].
pub struct Icmp<BT>(PhantomData<BT>, Never);

impl<BT: IcmpEchoBindingsTypes> DatagramSocketSpec for Icmp<BT> {
    const NAME: &'static str = "ICMP_ECHO";
    type AddrSpec = IcmpAddrSpec;

    type SocketId<I: datagram::IpExt, D: device::WeakId> = IcmpSocketId<I, D, BT>;

    type OtherStackIpOptions<I: datagram::IpExt, D: device::WeakId> = ();

    type SharingState = ();

    type SocketMapSpec<I: datagram::IpExt + datagram::DualStackIpExt, D: device::WeakId> =
        (Self, I, D);

    fn ip_proto<I: IpProtoExt>() -> I::Proto {
        I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
    }

    fn make_bound_socket_map_id<I: datagram::IpExt, D: device::WeakId>(
        s: &Self::SocketId<I, D>,
    ) -> <Self::SocketMapSpec<I, D> as datagram::DatagramSocketMapSpec<
        I,
        D,
        Self::AddrSpec,
    >>::BoundSocketId{
        s.clone()
    }

    type Serializer<I: datagram::IpExt, B: BufferMut> =
        packet::Nested<B, IcmpPacketBuilder<I, IcmpEchoRequest>>;
    type SerializeError = packet_formats::error::ParseError;

    type ExternalData<I: Ip> = BT::ExternalData<I>;

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

    fn try_alloc_listen_identifier<I: datagram::IpExt, D: device::WeakId>(
        bindings_ctx: &mut impl RngContext,
        is_available: impl Fn(
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        ) -> Result<(), datagram::InUseError>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
        let mut port = IcmpBoundSockets::<I, D, BT>::rand_ephemeral(&mut bindings_ctx.rng());
        for _ in IcmpBoundSockets::<I, D, BT>::EPHEMERAL_RANGE {
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

    type ConnState<I: datagram::IpExt, D: device::WeakId> = datagram::ConnState<I, I, D, Self>;
    // Store the remote port/id set by `connect`. This does not participate in
    // demuxing, so not part of the socketmap, but we need to store it so that
    // it can be reported later.
    type ConnStateExtra = u16;

    fn conn_info_from_state<I: IpExt, D: device::WeakId>(
        datagram::ConnState { addr: ConnAddr { ip, device }, extra, .. }: &Self::ConnState<I, D>,
    ) -> datagram::ConnInfo<I::Addr, D> {
        let ConnInfoAddr { local: (local_ip, local_identifier), remote: (remote_ip, ()) } =
            ip.clone().into();
        datagram::ConnInfo::new(local_ip, local_identifier, remote_ip, *extra, || {
            // The invariant that a zone is present if needed is upheld by connect.
            device.clone().expect("device must be bound for addresses that require zones")
        })
    }

    fn try_alloc_local_id<I: IpExt, D: device::WeakId, BC: RngContext>(
        bound: &IcmpBoundSockets<I, D, BT>,
        bindings_ctx: &mut BC,
        flow: datagram::DatagramFlowId<I::Addr, ()>,
    ) -> Option<NonZeroU16> {
        let mut rng = bindings_ctx.rng();
        algorithm::simple_randomized_port_alloc(&mut rng, &flow, bound, &())
            .map(|p| NonZeroU16::new(p).expect("ephemeral ports should be non-zero"))
    }
}

/// Uninstantiatable type for implementing [`SocketMapAddrSpec`].
pub enum IcmpAddrSpec {}

impl SocketMapAddrSpec for IcmpAddrSpec {
    type RemoteIdentifier = ();
    type LocalIdentifier = NonZeroU16;
}

type IcmpBoundSockets<I, D, BT> = datagram::BoundSockets<I, D, IcmpAddrSpec, (Icmp<BT>, I, D)>;

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> PortAllocImpl
    for IcmpBoundSockets<I, D, BT>
{
    const EPHEMERAL_RANGE: core::ops::RangeInclusive<u16> = 1..=u16::MAX;
    type Id = DatagramFlowId<I::Addr, ()>;
    type PortAvailableArg = ();

    fn is_port_available(&self, id: &Self::Id, port: u16, (): &()) -> bool {
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

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct BoundSockets<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> {
    pub(crate) socket_map: IcmpBoundSockets<I, D, BT>,
}

impl<I, BC, CC> datagram::NonDualStackDatagramBoundStateContext<I, BC, Icmp<BC>> for CC
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

impl<I, BC, CC> DatagramBoundStateContext<I, BC, Icmp<BC>> for CC
where
    I: IpExt + datagram::DualStackIpExt,
    BC: IcmpBindingsContext<I, Self::DeviceId>,
    CC: InnerIcmpContext<I, BC> + IcmpStateContext,
{
    type IpSocketsCtx<'a> = CC::IpSocketsCtx<'a>;

    // ICMP sockets doesn't support dual-stack operations.
    type DualStackContext = CC::DualStackContext;

    type NonDualStackContext = Self;

    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &IcmpBoundSockets<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(self, |ctx, BoundSockets { socket_map }| {
            cb(ctx, &socket_map)
        })
    }

    fn with_bound_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut IcmpBoundSockets<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(self, |ctx, BoundSockets { socket_map }| {
            cb(ctx, socket_map)
        })
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

impl<I, BC, CC> DatagramStateContext<I, BC, Icmp<BC>> for CC
where
    I: IpExt + datagram::DualStackIpExt,
    BC: IcmpBindingsContext<I, Self::DeviceId>,
    CC: StateContext<I, BC> + IcmpStateContext,
{
    type SocketsStateCtx<'a> = CC::SocketStateCtx<'a>;

    fn with_all_sockets_mut<O, F: FnOnce(&mut IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        StateContext::with_all_sockets_mut(self, cb)
    }

    fn with_all_sockets<O, F: FnOnce(&IcmpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        StateContext::with_all_sockets(self, cb)
    }

    fn with_socket_state<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &IcmpSocketState<I, Self::WeakDeviceId, BC>) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        StateContext::with_socket_state(self, id, cb)
    }

    fn with_socket_state_mut<
        O,
        F: FnOnce(
            &mut Self::SocketsStateCtx<'_>,
            &mut IcmpSocketState<I, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        id: &IcmpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        StateContext::with_socket_state_mut(self, id, cb)
    }

    fn for_each_socket<
        F: FnMut(
            &mut Self::SocketsStateCtx<'_>,
            &IcmpSocketId<I, Self::WeakDeviceId, BC>,
            &IcmpSocketState<I, Self::WeakDeviceId, BC>,
        ),
    >(
        &mut self,
        cb: F,
    ) {
        StateContext::for_each_socket(self, cb)
    }
}

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> SocketMapStateSpec
    for (Icmp<BT>, I, D)
{
    type ListenerId = IcmpSocketId<I, D, BT>;
    type ConnId = IcmpSocketId<I, D, BT>;

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

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes> SocketMapAddrStateSpec
    for IcmpSocketId<I, D, BT>
{
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

impl<I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes>
    DatagramSocketMapSpec<I, D, IcmpAddrSpec> for (Icmp<BT>, I, D)
{
    type BoundSocketId = IcmpSocketId<I, D, BT>;
}

impl<AA, I: IpExt, D: device::WeakId, BT: IcmpEchoBindingsTypes>
    SocketMapConflictPolicy<AA, (), I, D, IcmpAddrSpec> for (Icmp<BT>, I, D)
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

/// The ICMP Echo sockets API.
pub struct IcmpEchoSocketApi<I: Ip, C>(C, IpVersionMarker<I>);

impl<I: Ip, C> IcmpEchoSocketApi<I, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, IpVersionMarker::new())
    }
}

/// A local alias for [`IcmpSocketId`] for use in [`IcmpEchoSocketApi`].
///
/// TODO(https://github.com/rust-lang/rust/issues/8995): Make this an inherent
/// associated type.
type IcmpApiSocketId<I, C> = IcmpSocketId<
    I,
    <<C as ContextPair>::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
    <C as ContextPair>::BindingsContext,
>;

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

    /// Creates a new unbound ICMP socket with default external data.
    pub fn create(&mut self) -> IcmpApiSocketId<I, C>
    where
        <C::BindingsContext as IcmpEchoBindingsTypes>::ExternalData<I>: Default,
    {
        self.create_with(Default::default())
    }

    /// Creates a new unbound ICMP socket with provided external data.
    pub fn create_with(
        &mut self,
        external_data: <C::BindingsContext as IcmpEchoBindingsTypes>::ExternalData<I>,
    ) -> IcmpApiSocketId<I, C> {
        datagram::create(self.core_ctx(), external_data)
    }

    /// Connects an ICMP socket to remote IP.
    ///
    /// If the socket is never bound, an local ID will be allocated.
    pub fn connect(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
        remote_ip: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        remote_id: u16,
    ) -> Result<(), datagram::ConnectError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::connect(core_ctx, bindings_ctx, id, remote_ip, (), remote_id)
    }

    /// Binds an ICMP socket to a local IP address and a local ID.
    ///
    /// Both the IP and the ID are optional. When IP is missing, the "any" IP is
    /// assumed; When the ID is missing, it will be allocated.
    pub fn bind(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
        local_ip: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        icmp_id: Option<NonZeroU16>,
    ) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::listen(core_ctx, bindings_ctx, id, local_ip, icmp_id)
    }

    /// Gets the information about an ICMP socket.
    pub fn get_info(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
    ) -> datagram::SocketInfo<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId>
    {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_info(core_ctx, bindings_ctx, id)
    }

    /// Sets the bound device for a socket.
    ///
    /// Sets the device to be used for sending and receiving packets for a
    /// socket. If the socket is not currently bound to a local address and
    /// port, the device will be used when binding.
    pub fn set_device(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
        device_id: Option<&<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    ) -> Result<(), SocketError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::set_device(core_ctx, bindings_ctx, id, device_id)
    }

    /// Gets the device the specified socket is bound to.
    pub fn get_bound_device(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
    ) -> Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_bound_device(core_ctx, bindings_ctx, id)
    }

    /// Disconnects an ICMP socket.
    pub fn disconnect(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
    ) -> Result<(), datagram::ExpectedConnError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::disconnect_connected(core_ctx, bindings_ctx, id)
    }

    /// Shuts down an ICMP socket.
    pub fn shutdown(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
        shutdown_type: ShutdownType,
    ) -> Result<(), datagram::ExpectedConnError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::shutdown_connected(core_ctx, bindings_ctx, id, shutdown_type)
    }

    /// Gets the current shutdown state of an ICMP socket.
    pub fn get_shutdown(&mut self, id: &IcmpApiSocketId<I, C>) -> Option<ShutdownType> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_shutdown_connected(core_ctx, bindings_ctx, id)
    }

    /// Closes an ICMP socket.
    pub fn close(
        &mut self,
        id: IcmpApiSocketId<I, C>,
    ) -> RemoveResourceResultWithContext<
        <C::BindingsContext as IcmpEchoBindingsTypes>::ExternalData<I>,
        C::BindingsContext,
    > {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::close(core_ctx, bindings_ctx, id)
    }

    /// Gets unicast IP hop limit for ICMP sockets.
    pub fn get_unicast_hop_limit(&mut self, id: &IcmpApiSocketId<I, C>) -> NonZeroU8 {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_ip_hop_limits(core_ctx, bindings_ctx, id).unicast
    }

    /// Gets multicast IP hop limit for ICMP sockets.
    pub fn get_multicast_hop_limit(&mut self, id: &IcmpApiSocketId<I, C>) -> NonZeroU8 {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::get_ip_hop_limits(core_ctx, bindings_ctx, id).multicast
    }

    /// Sets unicast IP hop limit for ICMP sockets.
    pub fn set_unicast_hop_limit(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
        hop_limit: Option<NonZeroU8>,
    ) {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::update_ip_hop_limit(
            core_ctx,
            bindings_ctx,
            id,
            SocketHopLimits::set_unicast(hop_limit),
        )
    }

    /// Sets multicast IP hop limit for ICMP sockets.
    pub fn set_multicast_hop_limit(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
        hop_limit: Option<NonZeroU8>,
    ) {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::update_ip_hop_limit(
            core_ctx,
            bindings_ctx,
            id,
            SocketHopLimits::set_multicast(hop_limit),
        )
    }

    /// Sends an ICMP packet through a connection.
    ///
    /// The socket must be connected in order for the operation to succeed.
    pub fn send<B: BufferMut>(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
        body: B,
    ) -> Result<(), datagram::SendError<packet_formats::error::ParseError>> {
        let (core_ctx, bindings_ctx) = self.contexts();
        datagram::send_conn::<_, _, _, Icmp<C::BindingsContext>, _>(
            core_ctx,
            bindings_ctx,
            id,
            body,
        )
    }

    /// Sends an ICMP packet with an remote address.
    ///
    /// The socket doesn't need to be connected.
    pub fn send_to<B: BufferMut>(
        &mut self,
        id: &IcmpApiSocketId<I, C>,
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
        datagram::send_to::<_, _, _, Icmp<C::BindingsContext>, _>(
            core_ctx,
            bindings_ctx,
            id,
            remote_ip,
            (),
            body,
        )
    }

    /// Collects all currently opened sockets, returning a cloned reference for
    /// each one.
    pub fn collect_all_sockets(&mut self) -> Vec<IcmpApiSocketId<I, C>> {
        datagram::collect_all_sockets(self.core_ctx())
    }

    /// Provides inspect data for ICMP echo sockets.
    pub fn inspect<N>(&mut self, inspector: &mut N)
    where
        N: Inspector
            + InspectorDeviceExt<<C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId>,
    {
        DatagramStateContext::for_each_socket(self.core_ctx(), |_ctx, socket_id, socket_state| {
            socket_state.record_common_info(inspector, socket_id);
        });
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::net_ip_v6;
    use net_types::{
        ip::{Ipv4, Ipv6, Mtu},
        Witness,
    };
    use packet::Buf;
    use packet_formats::icmp::IcmpUnusedCode;
    use test_case::test_case;

    use super::*;
    use crate::{
        device::loopback::{LoopbackCreationProperties, LoopbackDevice},
        ip::icmp::tests::FakeIcmpCtx,
        socket::StrictlyZonedAddr,
        testutil::{CtxPairExt as _, TestIpExt, DEFAULT_INTERFACE_METRIC},
    };

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
        let mut net = crate::testutil::new_simple_fake_network(
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
            conn
        });

        net.run_until_idle();

        assert_eq!(
            net.context(LOCAL_CTX_NAME)
                .core_ctx
                .inner_icmp_state::<I>()
                .rx_counters
                .echo_reply
                .get(),
            1
        );
        assert_eq!(
            net.context(ctx_name_receiving_req)
                .core_ctx
                .inner_icmp_state::<I>()
                .rx_counters
                .echo_request
                .get(),
            1
        );
        let replies = net.context(LOCAL_CTX_NAME).bindings_ctx.take_icmp_replies(&conn);
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
        assert_eq!(api.get_info(&id), datagram::SocketInfo::Unbound);

        api.bind(&id, None, Some(ICMP_ID)).unwrap();
        assert_eq!(
            api.get_info(&id),
            datagram::SocketInfo::Listener(datagram::ListenerInfo {
                local_ip: None,
                local_identifier: ICMP_ID
            })
        );

        api.connect(&id, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), REMOTE_ID).unwrap();
        assert_eq!(
            api.get_info(&id),
            datagram::SocketInfo::Connected(datagram::ConnInfo {
                local_ip: StrictlyZonedAddr::new_unzoned_or_panic(I::FAKE_CONFIG.local_ip),
                local_identifier: ICMP_ID,
                remote_ip: StrictlyZonedAddr::new_unzoned_or_panic(I::FAKE_CONFIG.remote_ip),
                remote_identifier: REMOTE_ID,
            })
        );
    }
}
