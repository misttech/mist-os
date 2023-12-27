// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines how TCP state machines are used for TCP sockets.
//!
//! TCP state machine implemented in the parent module aims to only implement
//! RFC 793 which lacks posix semantics.
//!
//! To actually support posix-style sockets:
//! We would need two kinds of active sockets, listeners/connections (or
//! server sockets/client sockets; both are not very accurate terms, the key
//! difference is that the former has only local addresses but the later has
//! remote addresses in addition). [`Connection`]s are backed by a state
//! machine, however the state can be in any state. [`Listener`]s don't have
//! state machines, but they create [`Connection`]s that are backed by
//! [`State::Listen`] an incoming SYN and keep track of whether the connection
//! is established.

mod accept_queue;
pub(crate) mod demux;
pub(crate) mod isn;

use alloc::collections::{hash_map, HashMap};
use core::{
    convert::Infallible as Never,
    fmt::{self, Debug},
    marker::PhantomData,
    num::{NonZeroU16, NonZeroUsize},
    ops::{Deref, DerefMut, RangeInclusive},
};
use lock_order::Locked;

use assert_matches::assert_matches;
use derivative::Derivative;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddr, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    AddrAndZone, NonMappedAddr, SpecifiedAddr, ZonedAddr,
};
use packet_formats::ip::IpProto;
use rand::RngCore;
use smallvec::{smallvec, SmallVec};
use thiserror::Error;
use tracing::{debug, error, trace};

/// We use this trait to turn [`DualStackIpExt::DemuxSocketId`]s into a TimerId.
pub(crate) trait ToTimerId<D: device::WeakId, BT: TcpBindingsTypes> {
    fn to_timer_id(&self) -> TimerId<D, BT>;
}

impl<D: device::WeakId, BT: TcpBindingsTypes> ToTimerId<D, BT> for TcpSocketId<Ipv6, D, BT> {
    fn to_timer_id(&self) -> TimerId<D, BT> {
        TimerId::V6(self.downgrade())
    }
}

impl<D: device::WeakId, BT: TcpBindingsTypes> ToTimerId<D, BT>
    for EitherStack<TcpSocketId<Ipv4, D, BT>, TcpSocketId<Ipv6, D, BT>>
{
    fn to_timer_id(&self) -> TimerId<D, BT> {
        match self {
            EitherStack::ThisStack(v4) => TimerId::V4(v4.downgrade()),
            EitherStack::OtherStack(v6) => TimerId::V6(v6.downgrade()),
        }
    }
}

/// A dual stack IP extension trait for TCP.
pub trait DualStackIpExt: crate::socket::DualStackIpExt {
    /// For `Ipv4`, this is [`EitherStack<TcpSocketId<Ipv4, _, _>, TcpSocketId<Ipv6, _, _>>`],
    /// and for `Ipv6` it is just `TcpSocketId<Ipv6>`.
    // TODO(https://issues.fuchsia.dev/316412877): Use sealed traits so we can
    // remove this lint.
    #[allow(private_bounds)]
    type DemuxSocketId<D: device::WeakId, BT: TcpBindingsTypes>: SpecSocketId + ToTimerId<D, BT>;

    /// The type for a connection, for [`Ipv4`], this will be just the single
    /// stack version of the connection state and the connection address. For
    /// [`Ipv6`], this will be a `EitherStack`.
    type ConnectionAndAddr<D: device::WeakId, BT: TcpBindingsTypes>: Send + Sync + Debug;

    /// Determines which stack the demux socket ID belongs to and converts
    /// (by reference) to a dual stack TCP socket ID.
    fn as_dual_stack_ip_socket<D: device::WeakId, BT: TcpBindingsTypes>(
        id: &Self::DemuxSocketId<D, BT>,
    ) -> EitherStack<&TcpSocketId<Self, D, BT>, &TcpSocketId<Self::OtherVersion, D, BT>>;

    /// Determines which stack the demux socket ID belongs to and converts
    /// (by value) to a dual stack TCP socket ID.
    fn into_dual_stack_ip_socket<D: device::WeakId, BT: TcpBindingsTypes>(
        id: Self::DemuxSocketId<D, BT>,
    ) -> EitherStack<TcpSocketId<Self, D, BT>, TcpSocketId<Self::OtherVersion, D, BT>>;

    /// Turns a [`TcpSocketId`] of the current stack into the demuxer ID.
    fn into_demux_socket_id<D: device::WeakId, BT: TcpBindingsTypes>(
        id: TcpSocketId<Self, D, BT>,
    ) -> Self::DemuxSocketId<D, BT>;

    // TODO(https://issues.fuchsia.dev/316408184): Improve type safety and avoid
    // this method that is unreachable for Ipv4.
    /// Turns a [`TcpSocketId`] into the demuxer ID of the other stack.
    fn into_other_demux_socket_id<D: device::WeakId, BT: TcpBindingsTypes>(
        id: TcpSocketId<Self, D, BT>,
    ) -> <Self::OtherVersion as DualStackIpExt>::DemuxSocketId<D, BT>;
}

impl DualStackIpExt for Ipv4 {
    type DemuxSocketId<D: device::WeakId, BT: TcpBindingsTypes> =
        EitherStack<TcpSocketId<Ipv4, D, BT>, TcpSocketId<Ipv6, D, BT>>;
    type ConnectionAndAddr<D: device::WeakId, BT: TcpBindingsTypes> =
        (Connection<Ipv4, Ipv4, D, BT>, ConnAddr<ConnIpAddr<Ipv4Addr, NonZeroU16, NonZeroU16>, D>);

    fn as_dual_stack_ip_socket<D: device::WeakId, BT: TcpBindingsTypes>(
        id: &Self::DemuxSocketId<D, BT>,
    ) -> EitherStack<&TcpSocketId<Self, D, BT>, &TcpSocketId<Self::OtherVersion, D, BT>> {
        match id {
            EitherStack::ThisStack(id) => EitherStack::ThisStack(id),
            EitherStack::OtherStack(id) => EitherStack::OtherStack(id),
        }
    }
    fn into_dual_stack_ip_socket<D: device::WeakId, BT: TcpBindingsTypes>(
        id: Self::DemuxSocketId<D, BT>,
    ) -> EitherStack<TcpSocketId<Self, D, BT>, TcpSocketId<Self::OtherVersion, D, BT>> {
        id
    }
    fn into_demux_socket_id<D: device::WeakId, BT: TcpBindingsTypes>(
        id: TcpSocketId<Self, D, BT>,
    ) -> Self::DemuxSocketId<D, BT> {
        EitherStack::ThisStack(id)
    }

    fn into_other_demux_socket_id<D: device::WeakId, BT: TcpBindingsTypes>(
        _id: TcpSocketId<Self, D, BT>,
    ) -> <Self::OtherVersion as DualStackIpExt>::DemuxSocketId<D, BT> {
        // TODO(https://issues.fuchsia.dev/316408184): Improve type safety and avoid
        // this method that is unreachable for Ipv4.
        unreachable!()
    }
}

impl DualStackIpExt for Ipv6 {
    type DemuxSocketId<D: device::WeakId, BT: TcpBindingsTypes> = TcpSocketId<Ipv6, D, BT>;
    type ConnectionAndAddr<D: device::WeakId, BT: TcpBindingsTypes> = EitherStack<
        (Connection<Ipv6, Ipv6, D, BT>, ConnAddr<ConnIpAddr<Ipv6Addr, NonZeroU16, NonZeroU16>, D>),
        (Connection<Ipv6, Ipv4, D, BT>, ConnAddr<ConnIpAddr<Ipv4Addr, NonZeroU16, NonZeroU16>, D>),
    >;

    fn as_dual_stack_ip_socket<D: device::WeakId, BT: TcpBindingsTypes>(
        id: &Self::DemuxSocketId<D, BT>,
    ) -> EitherStack<&TcpSocketId<Self, D, BT>, &TcpSocketId<Self::OtherVersion, D, BT>> {
        EitherStack::ThisStack(id)
    }
    fn into_dual_stack_ip_socket<D: device::WeakId, BT: TcpBindingsTypes>(
        id: Self::DemuxSocketId<D, BT>,
    ) -> EitherStack<TcpSocketId<Self, D, BT>, TcpSocketId<Self::OtherVersion, D, BT>> {
        EitherStack::ThisStack(id)
    }

    fn into_demux_socket_id<D: device::WeakId, BT: TcpBindingsTypes>(
        id: TcpSocketId<Self, D, BT>,
    ) -> Self::DemuxSocketId<D, BT> {
        id
    }

    fn into_other_demux_socket_id<D: device::WeakId, BT: TcpBindingsTypes>(
        id: TcpSocketId<Self, D, BT>,
    ) -> <Self::OtherVersion as DualStackIpExt>::DemuxSocketId<D, BT> {
        EitherStack::OtherStack(id)
    }
}

use crate::{
    algorithm::{PortAlloc, PortAllocImpl},
    context::{InstantBindingsTypes, TimerContext, TracingContext},
    convert::{BidirectionalConverter as _, OwnedOrRefsBidirectionalConverter},
    data_structures::socketmap::{IterShadows as _, SocketMap},
    device::{self, AnyDevice, DeviceId, DeviceIdContext, WeakDeviceId, WeakId},
    error::{ExistsError, LocalAddressError, ZonedAddressError},
    ip::{
        icmp::IcmpErrorCode,
        socket::{
            DefaultSendOptions, DeviceIpSocketHandler, IpSock, IpSockCreationError, IpSocketHandler,
        },
        EitherDeviceId, TransportIpContext,
    },
    socket::{
        address::{
            dual_stack_remote_ip, ConnAddr, ConnIpAddr, DualStackRemoteIp, ListenerAddr,
            ListenerIpAddr, SocketIpAddr, SocketZonedIpAddr,
        },
        AddrVec, Bound, BoundSocketMap, EitherStack, IncompatibleError, InsertError, Inserter,
        ListenerAddrInfo, MaybeDualStack, RemoveResult, Shutdown, SocketMapAddrSpec,
        SocketMapAddrStateSpec, SocketMapAddrStateUpdateSharingSpec, SocketMapConflictPolicy,
        SocketMapStateSpec, SocketMapUpdateSharingPolicy, UpdateSharingError,
    },
    sync::RwLock,
    transport::tcp::{
        buffer::{IntoBuffers, ReceiveBuffer, SendBuffer},
        seqnum::SeqNum,
        socket::{accept_queue::AcceptQueue, demux::tcp_serialize_segment, isn::IsnGenerator},
        state::{CloseError, CloseReason, Closed, Initial, State, Takeable},
        BufferSizes, ConnectionError, Mss, OptionalBufferSizes, SocketOptions,
    },
    SyncCtx,
};

pub use accept_queue::ListenerNotifier;

/// Timer ID for TCP connections.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip()]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
#[allow(missing_docs)]
pub enum TimerId<D: device::WeakId, BT: TcpBindingsTypes> {
    V4(WeakTcpSocketId<Ipv4, D, BT>),
    V6(WeakTcpSocketId<Ipv6, D, BT>),
}

impl<I, D, BC> TimerContext<WeakTcpSocketId<I, D, BC>> for BC
where
    BC: TimerContext<TimerId<D, BC>> + TcpBindingsTypes,
    I: DualStackIpExt,
    D: device::WeakId,
{
    fn schedule_timer_instant(
        &mut self,
        time: Self::Instant,
        id: WeakTcpSocketId<I, D, BC>,
    ) -> Option<Self::Instant> {
        TimerContext::<TimerId<D, BC>>::schedule_timer_instant(self, time, id.into())
    }

    fn cancel_timer(&mut self, id: WeakTcpSocketId<I, D, BC>) -> Option<Self::Instant> {
        TimerContext::<TimerId<D, BC>>::cancel_timer(self, id.into())
    }

    fn cancel_timers_with<F: FnMut(&WeakTcpSocketId<I, D, BC>) -> bool>(&mut self, mut f: F) {
        TimerContext::<TimerId<D, BC>>::cancel_timers_with(self, |id| {
            id.as_inner::<I>().map(|i| f(i)).unwrap_or(false)
        })
    }

    fn scheduled_instant(&self, id: WeakTcpSocketId<I, D, BC>) -> Option<Self::Instant> {
        TimerContext::<TimerId<D, BC>>::scheduled_instant(self, id.into())
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> From<WeakTcpSocketId<I, D, BT>>
    for TimerId<D, BT>
{
    fn from(f: WeakTcpSocketId<I, D, BT>) -> Self {
        I::map_ip(f, TimerId::V4, TimerId::V6)
    }
}

impl<D: device::WeakId, BT: TcpBindingsTypes> TimerId<D, BT> {
    fn as_inner<I: DualStackIpExt>(&self) -> Option<&WeakTcpSocketId<I, D, BT>> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(id)| match id {
                TimerId::V4(i) => Some(i),
                _ => None,
            },
            |IpInvariant(id)| match id {
                TimerId::V6(i) => Some(i),
                _ => None,
            },
        )
    }
}

/// Bindings types for TCP.
///
/// The relationship between buffers  is as follows:
///
/// The Bindings will receive the `ReturnedBuffers` so that it can: 1. give the
/// application a handle to read/write data; 2. Observe whatever signal required
/// from the application so that it can inform Core. The peer end of returned
/// handle will be held by the state machine inside the netstack. Specialized
/// receive/send buffers will be derived from `ProvidedBuffers` from Bindings.
///
/// +-------------------------------+
/// |       +--------------+        |
/// |       |   returned   |        |
/// |       |    buffers   |        |
/// |       +------+-------+        |
/// |              |     application|
/// +--------------+----------------+
///                |
/// +--------------+----------------+
/// |              |        netstack|
/// |   +---+------+-------+---+    |
/// |   |   |  provided    |   |    |
/// |   | +-+-  buffers   -+-+ |    |
/// |   +-+-+--------------+-+-+    |
/// |     v                  v      |
/// |receive buffer     send buffer |
/// +-------------------------------+

pub trait TcpBindingsTypes: InstantBindingsTypes + 'static {
    /// Receive buffer used by TCP.
    type ReceiveBuffer: ReceiveBuffer + Send + Sync;
    /// Send buffer used by TCP.
    type SendBuffer: SendBuffer + Send + Sync;
    /// The object that will be returned by the state machine when a passive
    /// open connection becomes established. The bindings can use this object
    /// to read/write bytes from/into the created buffers.
    type ReturnedBuffers: Debug + Send + Sync;
    /// The extra information provided by the Bindings that implements platform
    /// dependent behaviors. It serves as a [`ListenerNotifier`] if the socket
    /// was used as a listener and it will be used to provide buffers if used
    /// to establish connections.
    type ListenerNotifierOrProvidedBuffers: Debug
        + Takeable
        + Clone
        + IntoBuffers<Self::ReceiveBuffer, Self::SendBuffer>
        + ListenerNotifier
        + Send
        + Sync;

    /// The buffer sizes to use when creating new sockets.
    fn default_buffer_sizes() -> BufferSizes;

    /// Creates new buffers and returns the object that Bindings need to
    /// read/write from/into the created buffers.
    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (Self::ReceiveBuffer, Self::SendBuffer, Self::ReturnedBuffers);
}

/// The bindings context for TCP.
///
/// TCP timers are scoped by weak device IDs.
pub trait TcpBindingsContext<D: device::WeakId>:
    Sized + TimerContext<TimerId<D, Self>> + TracingContext + TcpBindingsTypes
{
}

impl<D, O> TcpBindingsContext<D> for O
where
    O: TimerContext<TimerId<D, O>> + TracingContext + TcpBindingsTypes + Sized,
    D: device::WeakId,
{
}

pub(crate) trait TcpDemuxContext<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    /// Calls `f` with non-mutable access to the demux state.
    fn with_demux<O, F: FnOnce(&DemuxState<I, D, BT>) -> O>(&mut self, cb: F) -> O;

    /// Calls `f` with non-mutable access to the demux state.
    fn with_demux_mut<O, F: FnOnce(&mut DemuxState<I, D, BT>) -> O>(&mut self, cb: F) -> O;
}

// TODO(https://fxbug.dev/136316): This trait is useful when we don't have full
// support of dual-stack sockets, so we can take short-cuts for single-stack
// operations. Remove once TCP dual-stack is fully supported.
/// A helper trait that turns [`MaybeDualStack`] context into the current stack
/// context
pub trait AsSingleStack<T> {
    /// Get the single stack version of the context.
    fn as_single_stack(&mut self) -> &mut T;
}

impl<T> AsSingleStack<T> for T {
    fn as_single_stack(&mut self) -> &mut T {
        self
    }
}

impl<'a, SS: 'a, DS: AsSingleStack<SS> + 'a, SC, DC>
    MaybeDualStack<(&'a mut DS, DC), (&'a mut SS, SC)>
{
    fn into_single_stack(self) -> (&'a mut SS, MaybeDualStack<DC, SC>) {
        match self {
            MaybeDualStack::DualStack((ds, dc)) => {
                (ds.as_single_stack(), MaybeDualStack::DualStack(dc))
            }
            MaybeDualStack::NotDualStack((ss, sc)) => (ss, MaybeDualStack::NotDualStack(sc)),
        }
    }
}

/// Core context for TCP.
pub(crate) trait TcpContext<I: DualStackIpExt, BC: TcpBindingsTypes>:
    TcpDemuxContext<I, Self::WeakDeviceId, BC> + IpSocketHandler<I, BC>
{
    /// The sync context that will give access to this version of the IP layer.
    type SingleStackIpTransportAndDemuxCtx<'a>: TransportIpContext<I, BC, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + DeviceIpSocketHandler<I, BC>
        + TcpDemuxContext<I, Self::WeakDeviceId, BC>;

    /// A collection of type assertions that must be true in the single stack
    /// version, associated types and concrete types must unify and we can
    /// inspect types by converting them into the concrete types.
    type SingleStackConverter: OwnedOrRefsBidirectionalConverter<
        I::ConnectionAndAddr<Self::WeakDeviceId, BC>,
        (
            Connection<I, I, Self::WeakDeviceId, BC>,
            ConnAddr<ConnIpAddr<<I as Ip>::Addr, NonZeroU16, NonZeroU16>, Self::WeakDeviceId>,
        ),
    >;

    /// The sync context that will give access to both versions of the IP layer.
    type DualStackIpTransportAndDemuxCtx<'a>: TransportIpContext<I, BC, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + DeviceIpSocketHandler<I, BC>
        + TcpDemuxContext<I, Self::WeakDeviceId, BC>
        + TransportIpContext<
            I::OtherVersion,
            BC,
            DeviceId = Self::DeviceId,
            WeakDeviceId = Self::WeakDeviceId,
        > + DeviceIpSocketHandler<I::OtherVersion, BC>
        + TcpDemuxContext<I::OtherVersion, Self::WeakDeviceId, BC>
        + AsSingleStack<Self::SingleStackIpTransportAndDemuxCtx<'a>>;

    /// A collection of type assertions that must be true in the dual stack
    /// version, associated types and concrete types must unify and we can
    /// inspect types by converting them into the concrete types.
    type DualStackConverter: OwnedOrRefsBidirectionalConverter<
        I::ConnectionAndAddr<Self::WeakDeviceId, BC>,
        EitherStack<
            (
                Connection<I, I, Self::WeakDeviceId, BC>,
                ConnAddr<ConnIpAddr<<I as Ip>::Addr, NonZeroU16, NonZeroU16>, Self::WeakDeviceId>,
            ),
            (
                Connection<I, I::OtherVersion, Self::WeakDeviceId, BC>,
                ConnAddr<
                    ConnIpAddr<<I::OtherVersion as Ip>::Addr, NonZeroU16, NonZeroU16>,
                    Self::WeakDeviceId,
                >,
            ),
        >,
    >;

    /// Calls the function with mutable access to the set with all TCP sockets.
    fn with_all_sockets_mut<O, F: FnOnce(&mut TcpSocketSet<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Called whenever a socket has had its destruction deferred.
    ///
    /// Used for tests context implementations to forbid deferred destruction.
    fn socket_destruction_deferred(&mut self, socket: WeakTcpSocketId<I, Self::WeakDeviceId, BC>);

    /// Calls the callback once for each currently installed socket.
    fn for_each_socket<
        F: FnMut(
            &TcpSocketState<I, Self::WeakDeviceId, BC>,
            MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
        ),
    >(
        &mut self,
        cb: F,
    );

    /// Calls the function with access to the socket state, ISN generator, and
    /// Transport + Demux context.
    fn with_socket_mut_isn_transport_demux<
        O,
        F: for<'a> FnOnce(
            MaybeDualStack<
                (&'a mut Self::DualStackIpTransportAndDemuxCtx<'a>, Self::DualStackConverter),
                (&'a mut Self::SingleStackIpTransportAndDemuxCtx<'a>, Self::SingleStackConverter),
            >,
            &mut TcpSocketState<I, Self::WeakDeviceId, BC>,
            &IsnGenerator<BC::Instant>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O;

    /// Calls the function with immutable access to the socket state.
    fn with_socket<O, F: FnOnce(&TcpSocketState<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        self.with_socket_and_converter(id, |socket_state, _converter| cb(socket_state))
    }

    /// Calls the function with the immutable reference to the socket state and
    /// a converter to inspect.
    fn with_socket_and_converter<
        O,
        F: FnOnce(
            &TcpSocketState<I, Self::WeakDeviceId, BC>,
            MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O;

    /// Calls the function with access to the socket state and Transport + Demux
    /// context.
    fn with_socket_mut_transport_demux<
        O,
        F: for<'a> FnOnce(
            MaybeDualStack<
                (&'a mut Self::DualStackIpTransportAndDemuxCtx<'a>, Self::DualStackConverter),
                (&'a mut Self::SingleStackIpTransportAndDemuxCtx<'a>, Self::SingleStackConverter),
            >,
            &mut TcpSocketState<I, Self::WeakDeviceId, BC>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        self.with_socket_mut_isn_transport_demux(id, |ctx, socket_state, _isn| {
            cb(ctx, socket_state)
        })
    }

    /// Calls the function with mutable access to the socket state.
    fn with_socket_mut<O, F: FnOnce(&mut TcpSocketState<I, Self::WeakDeviceId, BC>) -> O>(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        self.with_socket_mut_isn_transport_demux(id, |_ctx, socket_state, _isn| cb(socket_state))
    }

    /// Calls the function with the mutable reference to the socket state and a
    /// converter to inspect.
    fn with_socket_mut_and_converter<
        O,
        F: FnOnce(
            &mut TcpSocketState<I, Self::WeakDeviceId, BC>,
            MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
        ) -> O,
    >(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        cb: F,
    ) -> O {
        self.with_socket_mut_isn_transport_demux(id, |ctx, socket_state, _isn| {
            let converter = match ctx {
                MaybeDualStack::NotDualStack((_core_ctx, converter)) => {
                    MaybeDualStack::NotDualStack(converter)
                }
                MaybeDualStack::DualStack((_core_ctx, converter)) => {
                    MaybeDualStack::DualStack(converter)
                }
            };
            cb(socket_state, converter)
        })
    }
}

/// Socket address includes the ip address and the port number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, GenericOverIp)]
#[generic_over_ip(A, IpAddress)]
pub struct SocketAddr<A: IpAddress, D> {
    /// The IP component of the address.
    pub ip: SocketZonedIpAddr<A, D>,
    /// The port component of the address.
    pub port: NonZeroU16,
}

impl<A: IpAddress, D> From<SocketAddr<A, D>>
    for IpAddr<SocketAddr<Ipv4Addr, D>, SocketAddr<Ipv6Addr, D>>
{
    fn from(addr: SocketAddr<A, D>) -> IpAddr<SocketAddr<Ipv4Addr, D>, SocketAddr<Ipv6Addr, D>> {
        let IpInvariant(addr) = <A::Version as Ip>::map_ip(
            addr,
            |i| IpInvariant(IpAddr::V4(i)),
            |i| IpInvariant(IpAddr::V6(i)),
        );
        addr
    }
}

impl<A: IpAddress, D> SocketAddr<A, D> {
    /// Maps the [`SocketAddr`]'s zone type.
    pub fn map_zone<Y>(self, f: impl FnOnce(D) -> Y) -> SocketAddr<A, Y> {
        let Self { ip, port } = self;
        SocketAddr { ip: ip.into_inner().map_zone(f).into(), port }
    }
}

impl<A: IpAddress, D: fmt::Display> fmt::Display for SocketAddr<A, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        let Self { ip, port } = self;
        let IpInvariant(result) = A::Version::map_ip(
            (ip, IpInvariant((f, port))),
            |(ip, IpInvariant((f, port)))| IpInvariant(write!(f, "{}:{}", ip.addr(), port)),
            |(ip, IpInvariant((f, port)))| match **ip {
                ZonedAddr::Unzoned(a) => IpInvariant(write!(f, "[{}]:{}", a, port)),
                ZonedAddr::Zoned(ref az) => IpInvariant(write!(f, "[{}]:{}", az, port)),
            },
        );
        result
    }
}

/// Uninstantiable type used to implement [`SocketMapAddrSpec`] for TCP
pub(crate) enum TcpPortSpec {}

impl SocketMapAddrSpec for TcpPortSpec {
    type RemoteIdentifier = NonZeroU16;
    type LocalIdentifier = NonZeroU16;
}

/// An implementation of [`IpTransportContext`] for TCP.
pub(crate) enum TcpIpTransportContext {}

/// This trait is only used as a marker for the identifier that
/// [`TcpSocketSpec`] keeps in the socket map. This is effectively only
/// implemented for [`TcpSocketId`] but defining a trait effectively reduces the
/// number of type parameters percolating down to the socket map types since
/// they only really care about the identifier's behavior.
pub(crate) trait SpecSocketId: Clone + Eq + PartialEq + Debug + 'static {}
impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> SpecSocketId
    for TcpSocketId<I, D, BT>
{
}

impl<A: SpecSocketId, B: SpecSocketId> SpecSocketId for EitherStack<A, B> {}

/// Uninstantiatable type for implementing [`SocketMapStateSpec`].
struct TcpSocketSpec<I, D, BT>(PhantomData<(I, D, BT)>, Never);

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> SocketMapStateSpec
    for TcpSocketSpec<I, D, BT>
{
    type ListenerId = TcpSocketId<I, D, BT>;
    type ConnId = I::DemuxSocketId<D, BT>;

    type ListenerSharingState = ListenerSharingState;
    type ConnSharingState = SharingState;
    type AddrVecTag = AddrVecTag;

    type ListenerAddrState = ListenerAddrState<Self::ListenerId>;
    type ConnAddrState = ConnAddrState<Self::ConnId>;

    fn listener_tag(
        ListenerAddrInfo { has_device, specified_addr: _ }: ListenerAddrInfo,
        state: &Self::ListenerAddrState,
    ) -> Self::AddrVecTag {
        let (sharing, state) = match state {
            ListenerAddrState::ExclusiveBound(_) => {
                (SharingState::Exclusive, SocketTagState::Bound)
            }
            ListenerAddrState::ExclusiveListener(_) => {
                (SharingState::Exclusive, SocketTagState::Listener)
            }
            ListenerAddrState::Shared { listener, bound: _ } => (
                SharingState::ReuseAddress,
                match listener {
                    Some(_) => SocketTagState::Listener,
                    None => SocketTagState::Bound,
                },
            ),
        };
        AddrVecTag { sharing, state, has_device }
    }

    fn connected_tag(has_device: bool, state: &Self::ConnAddrState) -> Self::AddrVecTag {
        let ConnAddrState { sharing, id: _ } = state;
        AddrVecTag { sharing: *sharing, has_device, state: SocketTagState::Conn }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AddrVecTag {
    sharing: SharingState,
    state: SocketTagState,
    has_device: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum SocketTagState {
    Conn,
    Listener,
    Bound,
}

#[derive(Debug)]
enum ListenerAddrState<S> {
    ExclusiveBound(S),
    ExclusiveListener(S),
    Shared { listener: Option<S>, bound: SmallVec<[S; 1]> },
}

#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct ListenerSharingState {
    pub(crate) sharing: SharingState,
    pub(crate) listening: bool,
}

enum ListenerAddrInserter<'a, S> {
    Listener(&'a mut Option<S>),
    Bound(&'a mut SmallVec<[S; 1]>),
}

impl<'a, S> Inserter<S> for ListenerAddrInserter<'a, S> {
    fn insert(self, id: S) {
        match self {
            Self::Listener(o) => *o = Some(id),
            Self::Bound(b) => b.push(id),
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum BoundSocketState<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    Listener(
        (
            MaybeListener<I, D, BT>,
            ListenerSharingState,
            ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>,
        ),
    ),
    Connected((I::ConnectionAndAddr<D, BT>, SharingState)),
}

impl<S: SpecSocketId> SocketMapAddrStateSpec for ListenerAddrState<S> {
    type SharingState = ListenerSharingState;
    type Id = S;
    type Inserter<'a> = ListenerAddrInserter<'a, S>;

    fn new(new_sharing_state: &Self::SharingState, id: Self::Id) -> Self {
        let ListenerSharingState { sharing, listening } = new_sharing_state;
        match sharing {
            SharingState::Exclusive => match listening {
                true => Self::ExclusiveListener(id),
                false => Self::ExclusiveBound(id),
            },
            SharingState::ReuseAddress => {
                let (listener, bound) =
                    if *listening { (Some(id), Default::default()) } else { (None, smallvec![id]) };
                Self::Shared { listener, bound }
            }
        }
    }

    fn contains_id(&self, id: &Self::Id) -> bool {
        match self {
            Self::ExclusiveBound(x) => id == x,
            Self::ExclusiveListener(x) => id == x,
            Self::Shared { listener, bound } => {
                listener.as_ref().map_or(false, |x| id == x) || bound.contains(id)
            }
        }
    }

    fn could_insert(
        &self,
        new_sharing_state: &Self::SharingState,
    ) -> Result<(), IncompatibleError> {
        match self {
            Self::ExclusiveBound(_) | Self::ExclusiveListener(_) => Err(IncompatibleError),
            Self::Shared { listener, bound: _ } => {
                let ListenerSharingState { listening: _, sharing } = new_sharing_state;
                match sharing {
                    SharingState::Exclusive => Err(IncompatibleError),
                    SharingState::ReuseAddress => match listener {
                        Some(_) => Err(IncompatibleError),
                        None => Ok(()),
                    },
                }
            }
        }
    }

    fn remove_by_id(&mut self, id: Self::Id) -> RemoveResult {
        match self {
            Self::ExclusiveBound(b) => {
                assert_eq!(*b, id);
                RemoveResult::IsLast
            }
            Self::ExclusiveListener(l) => {
                assert_eq!(*l, id);
                RemoveResult::IsLast
            }
            Self::Shared { listener, bound } => {
                match listener {
                    Some(l) if *l == id => {
                        *listener = None;
                    }
                    Some(_) | None => {
                        let index = bound.iter().position(|b| *b == id).expect("invalid socket ID");
                        let _: S = bound.swap_remove(index);
                    }
                };
                match (listener, bound.is_empty()) {
                    (Some(_), _) => RemoveResult::Success,
                    (None, false) => RemoveResult::Success,
                    (None, true) => RemoveResult::IsLast,
                }
            }
        }
    }

    fn try_get_inserter<'a, 'b>(
        &'b mut self,
        new_sharing_state: &'a Self::SharingState,
    ) -> Result<Self::Inserter<'b>, IncompatibleError> {
        match self {
            Self::ExclusiveBound(_) | Self::ExclusiveListener(_) => Err(IncompatibleError),
            Self::Shared { listener, bound } => {
                let ListenerSharingState { listening, sharing } = new_sharing_state;
                match sharing {
                    SharingState::Exclusive => Err(IncompatibleError),
                    SharingState::ReuseAddress => {
                        match listener {
                            Some(_) => {
                                // Always fail to insert if there is already a
                                // listening socket.
                                Err(IncompatibleError)
                            }
                            None => Ok(match listening {
                                true => ListenerAddrInserter::Listener(listener),
                                false => ListenerAddrInserter::Bound(bound),
                            }),
                        }
                    }
                }
            }
        }
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>
    SocketMapUpdateSharingPolicy<
        ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>,
        ListenerSharingState,
        I,
        D,
        TcpPortSpec,
    > for TcpSocketSpec<I, D, BT>
{
    fn allows_sharing_update(
        socketmap: &SocketMap<AddrVec<I, D, TcpPortSpec>, Bound<Self>>,
        addr: &ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>,
        ListenerSharingState{listening: old_listening, sharing: old_sharing}: &ListenerSharingState,
        ListenerSharingState{listening: new_listening, sharing: new_sharing}: &ListenerSharingState,
    ) -> Result<(), UpdateSharingError> {
        let ListenerAddr { device, ip: ListenerIpAddr { addr: _, identifier } } = addr;
        match (old_listening, new_listening) {
            (true, false) => (), // Changing a listener to bound is always okay.
            (true, true) | (false, false) => (), // No change
            (false, true) => {
                // Upgrading a bound socket to a listener requires no other
                // listeners on similar addresses. We can check that by checking
                // that there are no listeners shadowing the any-listener
                // address.
                if socketmap
                    .descendant_counts(
                        &ListenerAddr {
                            device: None,
                            ip: ListenerIpAddr { addr: None, identifier: *identifier },
                        }
                        .into(),
                    )
                    .any(
                        |(AddrVecTag { state, has_device: _, sharing: _ }, _): &(
                            _,
                            NonZeroUsize,
                        )| match state {
                            SocketTagState::Conn | SocketTagState::Bound => false,
                            SocketTagState::Listener => true,
                        },
                    )
                {
                    return Err(UpdateSharingError);
                }
            }
        }

        match (old_sharing, new_sharing) {
            (SharingState::Exclusive, SharingState::Exclusive)
            | (SharingState::ReuseAddress, SharingState::ReuseAddress) => (),
            (SharingState::Exclusive, SharingState::ReuseAddress) => (),
            (SharingState::ReuseAddress, SharingState::Exclusive) => {
                // Linux allows this, but it introduces inconsistent socket
                // state: if some sockets were allowed to bind because they all
                // had SO_REUSEADDR set, then allowing clearing SO_REUSEADDR on
                // one of them makes the state inconsistent. We only allow this
                // if it doesn't introduce inconsistencies.
                let root_addr = ListenerAddr {
                    device: None,
                    ip: ListenerIpAddr { addr: None, identifier: *identifier },
                };

                let conflicts = match device {
                    // If the socket doesn't have a device, it conflicts with
                    // any listeners that shadow it or that it shadows.
                    None => {
                        socketmap.descendant_counts(&addr.clone().into()).any(
                            |(AddrVecTag { has_device: _, sharing: _, state }, _)| match state {
                                SocketTagState::Conn => false,
                                SocketTagState::Bound | SocketTagState::Listener => true,
                            },
                        ) || (addr != &root_addr && socketmap.get(&root_addr.into()).is_some())
                    }
                    Some(_) => {
                        // If the socket has a device, it will indirectly
                        // conflict with a listener that doesn't have a device
                        // that is either on the same address or the unspecified
                        // address (on the same port).
                        socketmap.descendant_counts(&root_addr.into()).any(
                            |(AddrVecTag { has_device, sharing: _, state }, _)| match state {
                                SocketTagState::Conn => false,
                                SocketTagState::Bound | SocketTagState::Listener => !has_device,
                            },
                        )
                        // Detect a conflict with a shadower (which must also
                        // have a device) on the same address or on a specific
                        // address if this socket is on the unspecified address.
                        || socketmap.descendant_counts(&addr.clone().into()).any(
                            |(AddrVecTag { has_device: _, sharing: _, state }, _)| match state {
                                SocketTagState::Conn => false,
                                SocketTagState::Bound | SocketTagState::Listener => true,
                            },
                        )
                    }
                };

                if conflicts {
                    return Err(UpdateSharingError);
                }
            }
        }

        Ok(())
    }
}

impl<S: SpecSocketId> SocketMapAddrStateUpdateSharingSpec for ListenerAddrState<S> {
    fn try_update_sharing(
        &mut self,
        id: Self::Id,
        ListenerSharingState{listening: new_listening, sharing: new_sharing}: &Self::SharingState,
    ) -> Result<(), IncompatibleError> {
        match self {
            Self::ExclusiveBound(i) | Self::ExclusiveListener(i) => {
                assert_eq!(i, &id);
                *self = match new_sharing {
                    SharingState::Exclusive => match new_listening {
                        true => Self::ExclusiveListener(id),
                        false => Self::ExclusiveBound(id),
                    },
                    SharingState::ReuseAddress => {
                        let (listener, bound) = match new_listening {
                            true => (Some(id), Default::default()),
                            false => (None, smallvec![id]),
                        };
                        Self::Shared { listener, bound }
                    }
                };
                Ok(())
            }
            Self::Shared { listener, bound } => {
                if listener.as_ref() == Some(&id) {
                    match new_sharing {
                        SharingState::Exclusive => {
                            if bound.is_empty() {
                                *self = match new_listening {
                                    true => Self::ExclusiveListener(id),
                                    false => Self::ExclusiveBound(id),
                                };
                                Ok(())
                            } else {
                                Err(IncompatibleError)
                            }
                        }
                        SharingState::ReuseAddress => match new_listening {
                            true => Ok(()), // no-op
                            false => {
                                bound.push(id);
                                *listener = None;
                                Ok(())
                            }
                        },
                    }
                } else {
                    let index = bound
                        .iter()
                        .position(|b| b == &id)
                        .expect("ID is neither listener nor bound");
                    if *new_listening && listener.is_some() {
                        return Err(IncompatibleError);
                    }
                    match new_sharing {
                        SharingState::Exclusive => {
                            if bound.len() > 1 {
                                return Err(IncompatibleError);
                            } else {
                                *self = match new_listening {
                                    true => Self::ExclusiveListener(id),
                                    false => Self::ExclusiveBound(id),
                                };
                                Ok(())
                            }
                        }
                        SharingState::ReuseAddress => {
                            match new_listening {
                                false => Ok(()), // no-op
                                true => {
                                    let _: S = bound.swap_remove(index);
                                    *listener = Some(id);
                                    Ok(())
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SharingState {
    Exclusive,
    ReuseAddress,
}

impl Default for SharingState {
    fn default() -> Self {
        Self::Exclusive
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>
    SocketMapConflictPolicy<
        ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>,
        ListenerSharingState,
        I,
        D,
        TcpPortSpec,
    > for TcpSocketSpec<I, D, BT>
{
    fn check_insert_conflicts(
        sharing: &ListenerSharingState,
        addr: &ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>,
        socketmap: &SocketMap<AddrVec<I, D, TcpPortSpec>, Bound<Self>>,
    ) -> Result<(), InsertError> {
        let addr = AddrVec::Listen(addr.clone());
        let ListenerSharingState { listening: _, sharing } = sharing;
        // Check if any shadow address is present, specifically, if
        // there is an any-listener with the same port.
        for a in addr.iter_shadows() {
            if let Some(s) = socketmap.get(&a) {
                match s {
                    Bound::Conn(c) => unreachable!("found conn state {c:?} at listener addr {a:?}"),
                    Bound::Listen(l) => match l {
                        ListenerAddrState::ExclusiveListener(_)
                        | ListenerAddrState::ExclusiveBound(_) => {
                            return Err(InsertError::ShadowAddrExists)
                        }
                        ListenerAddrState::Shared { listener, bound: _ } => match sharing {
                            SharingState::Exclusive => return Err(InsertError::ShadowAddrExists),
                            SharingState::ReuseAddress => match listener {
                                Some(_) => return Err(InsertError::ShadowAddrExists),
                                None => (),
                            },
                        },
                    },
                }
            }
        }

        // Check if shadower exists. Note: Listeners do conflict with existing
        // connections, unless the listeners and connections have sharing
        // enabled.
        for (tag, _count) in socketmap.descendant_counts(&addr) {
            let AddrVecTag { sharing: tag_sharing, has_device: _, state: _ } = tag;
            match (tag_sharing, sharing) {
                (SharingState::Exclusive, SharingState::Exclusive | SharingState::ReuseAddress) => {
                    return Err(InsertError::ShadowerExists)
                }
                (SharingState::ReuseAddress, SharingState::Exclusive) => {
                    return Err(InsertError::ShadowerExists)
                }
                (SharingState::ReuseAddress, SharingState::ReuseAddress) => (),
            }
        }
        Ok(())
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>
    SocketMapConflictPolicy<
        ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, D>,
        SharingState,
        I,
        D,
        TcpPortSpec,
    > for TcpSocketSpec<I, D, BT>
{
    fn check_insert_conflicts(
        _sharing: &SharingState,
        addr: &ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, D>,
        socketmap: &SocketMap<AddrVec<I, D, TcpPortSpec>, Bound<Self>>,
    ) -> Result<(), InsertError> {
        // We need to make sure there are no present sockets that have the same
        // 4-tuple with the to-be-added socket.
        let addr = AddrVec::Conn(ConnAddr { device: None, ..*addr });
        if let Some(_) = socketmap.get(&addr) {
            return Err(InsertError::Exists);
        }
        // No shadower exists, i.e., no sockets with the same 4-tuple but with
        // a device bound.
        if socketmap.descendant_counts(&addr).len() > 0 {
            return Err(InsertError::ShadowerExists);
        }
        // Otherwise, connections don't conflict with existing listeners.
        Ok(())
    }
}

#[derive(Debug)]
struct ConnAddrState<S> {
    sharing: SharingState,
    id: S,
}

impl<S: SpecSocketId> ConnAddrState<S> {
    pub(crate) fn id(&self) -> S {
        self.id.clone()
    }
}

impl<S: SpecSocketId> SocketMapAddrStateSpec for ConnAddrState<S> {
    type Id = S;
    type Inserter<'a> = Never;
    type SharingState = SharingState;

    fn new(new_sharing_state: &Self::SharingState, id: Self::Id) -> Self {
        Self { sharing: *new_sharing_state, id }
    }

    fn contains_id(&self, id: &Self::Id) -> bool {
        &self.id == id
    }

    fn could_insert(
        &self,
        _new_sharing_state: &Self::SharingState,
    ) -> Result<(), IncompatibleError> {
        Err(IncompatibleError)
    }

    fn remove_by_id(&mut self, id: Self::Id) -> RemoveResult {
        let Self { sharing: _, id: existing_id } = self;
        assert_eq!(*existing_id, id);
        return RemoveResult::IsLast;
    }

    fn try_get_inserter<'a, 'b>(
        &'b mut self,
        _new_sharing_state: &'a Self::SharingState,
    ) -> Result<Self::Inserter<'b>, IncompatibleError> {
        Err(IncompatibleError)
    }
}

#[derive(Debug, Derivative, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct Unbound<D, Extra> {
    bound_device: Option<D>,
    buffer_sizes: BufferSizes,
    socket_options: SocketOptions,
    sharing: SharingState,
    socket_extra: Extra,
}

type ReferenceState<I, D, BT> = RwLock<TcpSocketState<I, D, BT>>;
type PrimaryRc<I, D, BT> = crate::sync::PrimaryRc<ReferenceState<I, D, BT>>;
type StrongRc<I, D, BT> = crate::sync::StrongRc<ReferenceState<I, D, BT>>;
type WeakRc<I, D, BT> = crate::sync::WeakRc<ReferenceState<I, D, BT>>;

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum TcpSocketSetEntry<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    /// The socket set is holding a primary reference.
    Primary(PrimaryRc<I, D, BT>),
    /// The socket set is holding a "dead on arrival" (DOA) entry for a strong
    /// reference.
    ///
    /// This mechanism guards against a subtle race between a connected socket
    /// created from a listener being added to the socket set and the same
    /// socket attempting to close itself before the listener has had a chance
    /// to add it to the set.
    ///
    /// See [`destroy_socket`] for the details handling this.
    DeadOnArrival,
}

/// A thin wrapper around a hash map that keeps a set of all the known TCP
/// sockets in the system.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct TcpSocketSet<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>(
    HashMap<TcpSocketId<I, D, BT>, TcpSocketSetEntry<I, D, BT>>,
);

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Deref for TcpSocketSet<I, D, BT> {
    type Target = HashMap<TcpSocketId<I, D, BT>, TcpSocketSetEntry<I, D, BT>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> DerefMut
    for TcpSocketSet<I, D, BT>
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// A custom drop impl for the entire set to make tests easier to handle.
///
/// Because [`TcpSocketId`] is not really RAII in respect to closing the socket,
/// tests might finish without closing them and it's easier to deal with that in
/// a single place.
impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Drop for TcpSocketSet<I, D, BT> {
    fn drop(&mut self) {
        // Listening sockets may hold references to other sockets so we walk
        // through all of the sockets looking for unclosed listeners and close
        // their accept queue so that dropping everything doesn't spring the
        // primary reference checks.
        //
        // Note that we don't pay attention to lock ordering here. Assuming that
        // when the set is dropped everything is going down and no locks are
        // held.
        let Self(map) = self;
        for TcpSocketId(rc) in map.keys() {
            let guard = rc.read();
            let accept_queue = match &*guard {
                TcpSocketState::Bound(BoundSocketState::Listener((
                    MaybeListener::Listener(Listener { accept_queue, .. }),
                    ..,
                ))) => accept_queue,
                _ => continue,
            };
            if !accept_queue.is_closed() {
                let (_pending_sockets_iterator, _): (_, BT::ListenerNotifierOrProvidedBuffers) =
                    accept_queue.close();
            }
        }
    }
}

pub(crate) struct DemuxState<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    port_alloc: PortAlloc<BoundSocketMap<I, D, TcpPortSpec, TcpSocketSpec<I, D, BT>>>,
    socketmap: BoundSocketMap<I, D, TcpPortSpec, TcpSocketSpec<I, D, BT>>,
}

/// Holds all the TCP socket states.
pub(crate) struct Sockets<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    pub(crate) demux: RwLock<DemuxState<I, D, BT>>,
    // Destroy all_sockets last so the strong references in the demux are
    // dropped before the primary references in the set.
    pub(crate) all_sockets: RwLock<TcpSocketSet<I, D, BT>>,
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum TcpSocketState<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    Unbound(Unbound<D, BT::ListenerNotifierOrProvidedBuffers>),
    Bound(BoundSocketState<I, D, BT>),
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> PortAllocImpl
    for BoundSocketMap<I, D, TcpPortSpec, TcpSocketSpec<I, D, BT>>
{
    const EPHEMERAL_RANGE: RangeInclusive<u16> = 49152..=65535;
    type Id = Option<SocketIpAddr<I::Addr>>;

    fn is_port_available(&self, addr: &Self::Id, port: u16) -> bool {
        // We can safely unwrap here, because the ports received in
        // `is_port_available` are guaranteed to be in `EPHEMERAL_RANGE`.
        let port = NonZeroU16::new(port).unwrap();
        let root_addr = AddrVec::from(ListenerAddr {
            ip: ListenerIpAddr { addr: *addr, identifier: port },
            device: None,
        });

        // A port is free if there are no sockets currently using it, and if
        // there are no sockets that are shadowing it.

        root_addr.iter_shadows().chain(core::iter::once(root_addr.clone())).all(|a| match &a {
            AddrVec::Listen(l) => self.listeners().get_by_addr(&l).is_none(),
            AddrVec::Conn(_c) => {
                unreachable!("no connection shall be included in an iteration from a listener")
            }
        }) && self.get_shadower_counts(&root_addr) == 0
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Sockets<I, D, BT> {
    pub(crate) fn new(rng: &mut impl RngCore) -> Self {
        Self {
            demux: RwLock::new(DemuxState {
                socketmap: Default::default(),
                port_alloc: PortAlloc::new(rng),
            }),
            all_sockets: Default::default(),
        }
    }
}

/// The Connection state.
///
/// Note: the `state` is not guaranteed to be [`State::Established`]. The
/// connection can be in any state as long as both the local and remote socket
/// addresses are specified.
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub struct Connection<
    SockI: DualStackIpExt,
    WireI: DualStackIpExt,
    D: device::WeakId,
    BT: TcpBindingsTypes,
> {
    accept_queue: Option<
        AcceptQueue<
            TcpSocketId<SockI, D, BT>,
            BT::ReturnedBuffers,
            BT::ListenerNotifierOrProvidedBuffers,
        >,
    >,
    state: State<
        BT::Instant,
        BT::ReceiveBuffer,
        BT::SendBuffer,
        BT::ListenerNotifierOrProvidedBuffers,
    >,
    ip_sock: IpSock<WireI, D, DefaultSendOptions>,
    /// The user has indicated that this connection will never be used again, we
    /// keep the connection in the socketmap to perform the shutdown but it will
    /// be auto removed once the state reaches Closed.
    defunct: bool,
    socket_options: SocketOptions,
    /// In contrast to a hard error, which will cause a connection to be closed,
    /// a soft error will not abort the connection, but it can be read by either
    /// calling `get_socket_error`, or after the connection times out.
    soft_error: Option<ConnectionError>,
    /// Whether the handshake has finished or aborted.
    handshake_status: HandshakeStatus,
}

fn make_connection<
    I: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
>(
    conn: Connection<I, I, CC::WeakDeviceId, BC>,
    addr: ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
    converter: &MaybeDualStack<CC::DualStackConverter, CC::SingleStackConverter>,
) -> I::ConnectionAndAddr<CC::WeakDeviceId, BC> {
    match converter {
        MaybeDualStack::NotDualStack(nds) => nds.convert_back((conn, addr)),
        MaybeDualStack::DualStack(ds) => ds.convert_back(EitherStack::ThisStack((conn, addr))),
    }
}

// TODO(https://fxbug.dev/136316): Remove this method once we support all dual
// stack operations.
fn try_into_this_stack_conn_mut<
    'a,
    I: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
>(
    conn: &'a mut I::ConnectionAndAddr<CC::WeakDeviceId, BC>,
    converter: &MaybeDualStack<CC::DualStackConverter, CC::SingleStackConverter>,
) -> Option<&'a mut (
    Connection<I, I, CC::WeakDeviceId, BC>,
    ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
)> {
    match converter {
        MaybeDualStack::NotDualStack(nds) => Some(nds.convert(conn)),
        MaybeDualStack::DualStack(ds) => match ds.convert(conn) {
            EitherStack::ThisStack(conn) => Some(conn),
            EitherStack::OtherStack(_conn) => None,
        },
    }
}

/// The Listener state.
///
/// State for sockets that participate in the passive open. Contrary to
/// [`Connection`], only the local address is specified.
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
#[cfg_attr(
    test,
    derivative(
        PartialEq(
            bound = "BT::ReturnedBuffers: PartialEq, BT::ListenerNotifierOrProvidedBuffers: PartialEq"
        ),
        Eq(bound = "BT::ReturnedBuffers: Eq, BT::ListenerNotifierOrProvidedBuffers: Eq"),
    )
)]
pub(crate) struct Listener<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    backlog: NonZeroUsize,
    accept_queue: AcceptQueue<
        TcpSocketId<I, D, BT>,
        BT::ReturnedBuffers,
        BT::ListenerNotifierOrProvidedBuffers,
    >,
    buffer_sizes: BufferSizes,
    socket_options: SocketOptions,
    // If ip sockets can be half-specified so that only the local address
    // is needed, we can construct an ip socket here to be reused.
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Listener<I, D, BT> {
    fn new(
        backlog: NonZeroUsize,
        buffer_sizes: BufferSizes,
        socket_options: SocketOptions,
        notifier: BT::ListenerNotifierOrProvidedBuffers,
    ) -> Self {
        Self { backlog, accept_queue: AcceptQueue::new(notifier), buffer_sizes, socket_options }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub(crate) struct BoundState<Extra> {
    buffer_sizes: BufferSizes,
    socket_options: SocketOptions,
    socket_extra: Extra,
}

/// Represents either a bound socket or a listener socket.
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
#[cfg_attr(
    test,
    derivative(
        Eq(bound = "BT::ReturnedBuffers: Eq, BT::ListenerNotifierOrProvidedBuffers: Eq"),
        PartialEq(
            bound = "BT::ReturnedBuffers: PartialEq, BT::ListenerNotifierOrProvidedBuffers: PartialEq"
        )
    )
)]
pub(crate) enum MaybeListener<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    Bound(BoundState<BT::ListenerNotifierOrProvidedBuffers>),
    Listener(Listener<I, D, BT>),
}

/// A TCP Socket ID.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
pub struct TcpSocketId<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>(
    StrongRc<I, D, BT>,
);

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Clone for TcpSocketId<I, D, BT> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    fn clone(&self) -> Self {
        let Self(rc) = self;
        Self(StrongRc::clone(rc))
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> TcpSocketId<I, D, BT> {
    pub(crate) fn new(state: TcpSocketState<I, D, BT>) -> (Self, PrimaryRc<I, D, BT>) {
        let primary = PrimaryRc::new(RwLock::new(state));
        let socket = Self(PrimaryRc::clone_strong(&primary));
        (socket, primary)
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Debug for TcpSocketId<I, D, BT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("TcpSocketId").field(&StrongRc::ptr_debug(rc)).finish()
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> TcpSocketId<I, D, BT> {
    pub(crate) fn downgrade(&self) -> WeakTcpSocketId<I, D, BT> {
        let Self(this) = self;
        WeakTcpSocketId(StrongRc::downgrade(this))
    }
}

/// A Weak TCP Socket ID.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
pub struct WeakTcpSocketId<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>(
    WeakRc<I, D, BT>,
);

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Debug
    for WeakTcpSocketId<I, D, BT>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("WeakTcpSocketId").field(&rc.ptr_debug()).finish()
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> PartialEq<TcpSocketId<I, D, BT>>
    for WeakTcpSocketId<I, D, BT>
{
    fn eq(&self, other: &TcpSocketId<I, D, BT>) -> bool {
        let Self(this) = self;
        let TcpSocketId(other) = other;
        StrongRc::weak_ptr_eq(other, this)
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> WeakTcpSocketId<I, D, BT> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    pub(crate) fn upgrade(&self) -> Option<TcpSocketId<I, D, BT>> {
        let Self(this) = self;
        this.upgrade().map(TcpSocketId)
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>
    lock_order::lock::RwLockFor<crate::lock_ordering::TcpSocketState<I>> for TcpSocketId<I, D, BT>
{
    type Data = TcpSocketState<I, D, BT>;

    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Self::Data>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Self::Data>
        where
            Self: 'l ;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        let Self(l) = self;
        l.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        let Self(l) = self;
        l.write()
    }
}

/// The status of a handshake.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum HandshakeStatus {
    /// The handshake is still pending.
    Pending,
    /// The handshake is aborted.
    Aborted,
    /// The handshake is completed.
    Completed {
        /// Whether it has been reported to the user yet.
        reported: bool,
    },
}

impl HandshakeStatus {
    fn update_if_pending(&mut self, new_status: Self) -> bool {
        if *self == HandshakeStatus::Pending {
            *self = new_status;
            true
        } else {
            false
        }
    }
}
pub(crate) trait SocketHandler<I: DualStackIpExt, BC: TcpBindingsContext<Self::WeakDeviceId>>:
    DeviceIdContext<AnyDevice>
{
    fn create_socket(
        &mut self,
        bindings_ctx: &mut BC,
        socket_extra: BC::ListenerNotifierOrProvidedBuffers,
    ) -> TcpSocketId<I, Self::WeakDeviceId, BC>;

    fn bind(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        local_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        port: Option<NonZeroU16>,
    ) -> Result<(), BindError>;

    fn listen(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        backlog: NonZeroUsize,
    ) -> Result<(), ListenError>;

    fn accept(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Result<
        (
            TcpSocketId<I, Self::WeakDeviceId, BC>,
            SocketAddr<I::Addr, Self::WeakDeviceId>,
            BC::ReturnedBuffers,
        ),
        AcceptError,
    >;

    fn connect(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        remote_port: NonZeroU16,
    ) -> Result<(), ConnectError>;

    fn close(&mut self, bindings_ctx: &mut BC, id: TcpSocketId<I, Self::WeakDeviceId, BC>);

    fn shutdown(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        shutdown: Shutdown,
    ) -> Result<bool, NoConnection>;

    fn get_info(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId>;

    fn do_send(&mut self, bindings_ctx: &mut BC, conn_id: &TcpSocketId<I, Self::WeakDeviceId, BC>);

    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        conn_id: WeakTcpSocketId<I, Self::WeakDeviceId, BC>,
    );

    fn with_socket_options_mut<R, F: FnOnce(&mut SocketOptions) -> R>(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        f: F,
    ) -> R;
    fn with_socket_options<R, F: FnOnce(&SocketOptions) -> R>(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        f: F,
    ) -> R;

    fn set_device(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        device: Option<Self::DeviceId>,
    ) -> Result<(), SetDeviceError>;

    fn set_send_buffer_size(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        size: usize,
    );
    fn send_buffer_size(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Option<usize>;
    fn set_receive_buffer_size(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        size: usize,
    );
    fn receive_buffer_size(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Option<usize>;

    fn set_reuseaddr(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        reuse: bool,
    ) -> Result<(), SetReuseAddrError>;
    fn reuseaddr(&mut self, id: &TcpSocketId<I, Self::WeakDeviceId, BC>) -> bool;

    /// Receives an ICMP error from the IP layer.
    fn on_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        orig_src_ip: SpecifiedAddr<I::Addr>,
        orig_dst_ip: SpecifiedAddr<I::Addr>,
        orig_src_port: NonZeroU16,
        orig_dst_port: NonZeroU16,
        seq: SeqNum,
        error: IcmpErrorCode,
    );

    fn get_socket_error(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Option<ConnectionError>;

    fn with_info<V: InfoVisitor<I, Self::WeakDeviceId>>(&mut self, cb: &mut V);
}

impl<I: DualStackIpExt, BC: TcpBindingsContext<CC::WeakDeviceId>, CC: TcpContext<I, BC>>
    SocketHandler<I, BC> for CC
{
    fn create_socket(
        &mut self,
        _bindings_ctx: &mut BC,
        socket_extra: BC::ListenerNotifierOrProvidedBuffers,
    ) -> TcpSocketId<I, Self::WeakDeviceId, BC> {
        self.with_all_sockets_mut(|all_sockets| {
            let (sock, primary) = TcpSocketId::new(TcpSocketState::Unbound(Unbound {
                buffer_sizes: BC::default_buffer_sizes(),
                bound_device: Default::default(),
                sharing: Default::default(),
                socket_options: Default::default(),
                socket_extra,
            }));
            assert_matches::assert_matches!(
                all_sockets.insert(sock.clone(), TcpSocketSetEntry::Primary(primary)),
                None
            );
            sock
        })
    }

    fn bind(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        addr: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        port: Option<NonZeroU16>,
    ) -> Result<(), BindError> {
        debug!("bind {id:?} to {addr:?}:{port:?}");
        // TODO(https://fxbug.dev/21198): Support dual-stack bind.
        if let Some(addr) = addr.as_ref() {
            match NonMappedAddr::new(addr.deref().addr()) {
                Some(_addr) => {}
                None => {
                    // Prevent binding to ipv4-mapped-ipv6 addrs for now.
                    return Err(BindError::LocalAddressError(
                        LocalAddressError::AddressUnexpectedlyMapped,
                    ));
                }
            }
        }

        // TODO(https://fxbug.dev/104300): Check if local_ip is a unicast address.
        self.with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let Unbound { bound_device, buffer_sizes, socket_options, sharing, socket_extra } =
                match socket_state {
                    TcpSocketState::Unbound(u) => u,
                    TcpSocketState::Bound(_) => return Err(BindError::AlreadyBound),
                };

            let bound_state = BoundState {
                buffer_sizes: buffer_sizes.clone(),
                socket_options: socket_options.clone(),
                socket_extra: socket_extra.clone(),
            };

            let (core_ctx, _converter) = core_ctx.into_single_stack();
            let (local_ip, device) = match addr {
                Some(addr) => {
                    // Extract the specified address and the device. The
                    // device is either the one from the address or the one
                    // to which the socket was previously bound.
                    let (addr, required_device) =
                        crate::transport::resolve_addr_with_device::<I::Addr, _, _, _>(
                            addr.into_inner(),
                            bound_device.clone(),
                        )
                        .map_err(LocalAddressError::Zone)?;

                    let mut assigned_to = <<CC as TcpContext<I, _>>::SingleStackIpTransportAndDemuxCtx<'_> as TransportIpContext<
                        I,
                        _,
                    >>::get_devices_with_assigned_addr(
                        core_ctx, addr
                    );
                    if !assigned_to.any(|d| {
                        required_device
                            .as_ref()
                            .map_or(true, |device| device == &EitherDeviceId::Strong(d))
                    }) {
                        return Err(LocalAddressError::AddressMismatch.into());
                    }

                    (Some(addr), required_device)
                }
                None => (None, bound_device.clone().map(EitherDeviceId::Weak)),
            };

            let weak_device = device.map(|d| d.as_weak(core_ctx).into_owned());
            let (addr, sharing) =
                core_ctx.with_demux_mut(move |DemuxState { socketmap, port_alloc }| {
                    let port = match port {
                        None => {
                            // TODO(https://fxbug.dev/132092): Delete conversion
                            // once `SocketZonedIpAddr` holds `SocketIpAddr`.
                            let addr = local_ip
                                .as_ref()
                                .map(|a| SocketIpAddr::new_from_specified_or_panic(*a));
                            match port_alloc.try_alloc(&addr, &socketmap) {
                                Some(port) => {
                                    NonZeroU16::new(port).expect("ephemeral ports must be non-zero")
                                }
                                None => {
                                    return Err(LocalAddressError::FailedToAllocateLocalPort);
                                }
                            }
                        }
                        Some(port) => port,
                    };

                    // TODO(https://fxbug.dev/132092): Delete conversion
                    // once `SocketZonedIpAddr` holds `SocketIpAddr`.
                    let local_ip = local_ip.map(SocketIpAddr::new_from_specified_or_panic);
                    let addr = ListenerAddr {
                        ip: ListenerIpAddr { addr: local_ip, identifier: port },
                        device: weak_device,
                    };
                    let sharing = ListenerSharingState { sharing: *sharing, listening: false };

                    let _inserted = socketmap
                        .listeners_mut()
                        .try_insert(addr.clone(), sharing.clone(), id.clone())
                        .map_err(|_: (InsertError, ListenerSharingState)| {
                            LocalAddressError::AddressInUse
                        })?;

                    Ok((addr, sharing))
                })?;

            *socket_state = TcpSocketState::Bound(BoundSocketState::Listener((
                MaybeListener::Bound(bound_state),
                sharing,
                addr,
            )));
            Ok(())
        })
    }

    fn listen(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        backlog: NonZeroUsize,
    ) -> Result<(), ListenError> {
        debug!("listen on {id:?} with backlog {backlog}");
        self.with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let (listener, listener_sharing, addr) = match socket_state {
                TcpSocketState::Bound(BoundSocketState::Listener((l, sharing, addr))) => match l {
                    MaybeListener::Listener(_) => return Err(ListenError::NotSupported),
                    MaybeListener::Bound(_) => (l, sharing, addr),
                },
                TcpSocketState::Bound(BoundSocketState::Connected(_))
                | TcpSocketState::Unbound(_) => return Err(ListenError::NotSupported),
            };

            let (core_ctx, _converter) = core_ctx.into_single_stack();
            core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                let entry =
                    socketmap.listeners_mut().entry(&id, &addr).expect("invalid listener id");
                let ListenerSharingState { sharing, listening } = listener_sharing;
                debug_assert!(!*listening, "invalid bound ID that has a listener socket");
                let sharing = *sharing;

                let new_sharing = ListenerSharingState { sharing, listening: true };
                match entry.try_update_sharing(&listener_sharing, new_sharing.clone()) {
                    Ok(()) => {
                        *listener_sharing = new_sharing;
                        Ok(())
                    }
                    Err(UpdateSharingError) => Err(ListenError::ListenerExists),
                }
            })?;

            match listener {
                MaybeListener::Bound(BoundState { buffer_sizes, socket_options, socket_extra }) => {
                    *listener = MaybeListener::Listener(Listener::new(
                        backlog,
                        buffer_sizes.clone(),
                        socket_options.clone(),
                        socket_extra.clone(),
                    ));
                }
                MaybeListener::Listener(_) => {
                    unreachable!("invalid bound id that points to a listener entry")
                }
            }
            Ok(())
        })
    }

    fn accept(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Result<
        (
            TcpSocketId<I, Self::WeakDeviceId, BC>,
            SocketAddr<I::Addr, Self::WeakDeviceId>,
            BC::ReturnedBuffers,
        ),
        AcceptError,
    > {
        let (conn_id, client_buffers) = self.with_socket_mut(id, |socket_state| {
            debug!("accept on {id:?}");
            let Listener { backlog: _, buffer_sizes: _, socket_options: _, accept_queue } =
                match socket_state {
                    TcpSocketState::Bound(BoundSocketState::Listener((
                        MaybeListener::Listener(l),
                        _sharing,
                        _addr,
                    ))) => l,
                    TcpSocketState::Unbound(_)
                    | TcpSocketState::Bound(BoundSocketState::Connected(_))
                    | TcpSocketState::Bound(BoundSocketState::Listener((
                        MaybeListener::Bound(_),
                        _,
                        _,
                    ))) => return Err(AcceptError::NotSupported),
                };
            let (conn_id, client_buffers) =
                accept_queue.pop_ready().ok_or(AcceptError::WouldBlock)?;

            Ok::<_, AcceptError>((conn_id, client_buffers))
        })?;

        let addr = self.with_socket_mut_and_converter(&conn_id, |socket_state, converter| {
            let (conn, conn_addr) = assert_matches!(
                socket_state,
                TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => try_into_this_stack_conn_mut::<I, BC, CC>(conn, &converter).expect("TODO(https://fxbug.dev/136316): dual stack listener not supported yet"),
                "invalid socket ID"
            );
            conn.accept_queue = None;
            let ConnAddr { ip: ConnIpAddr { local: _, remote }, device } = conn_addr;
            let (remote_ip, remote_port) = *remote;

            SocketAddr { ip: maybe_zoned(remote_ip.into(), device), port: remote_port }
        });
        Ok((conn_id, addr, client_buffers))
    }

    fn connect(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        remote_port: NonZeroU16,
    ) -> Result<(), ConnectError> {
        self.with_socket_mut_isn_transport_demux(id, |core_ctx, socket_state, isn| {
            debug!("connect on {id:?} to {remote_ip:?}:{remote_port}");
            let (
                local_ip,
                local_port,
                bound_device,
                listener_addr,
                sharing,
                socket_options,
                buffer_sizes,
                socket_extra,
            ) = match socket_state {
                TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => {
                    let handshake_status = match core_ctx {
                        MaybeDualStack::NotDualStack((_core_ctx, converter)) => {
                            let (conn, _addr) = converter.convert(conn);
                            &mut conn.handshake_status
                        }
                        MaybeDualStack::DualStack((_core_ctx, converter)) => {
                            match converter.convert(conn) {
                                EitherStack::ThisStack((conn, _addr)) => &mut conn.handshake_status,
                                EitherStack::OtherStack((conn, _addr)) => {
                                    &mut conn.handshake_status
                                }
                            }
                        }
                    };
                    match handshake_status {
                        HandshakeStatus::Pending => return Err(ConnectError::Pending),
                        HandshakeStatus::Aborted => return Err(ConnectError::Aborted),
                        HandshakeStatus::Completed { reported } => {
                            if *reported {
                                return Err(ConnectError::Completed);
                            } else {
                                *reported = true;
                                return Ok(());
                            }
                        }
                    }
                }
                TcpSocketState::Unbound(Unbound {
                    bound_device,
                    socket_extra,
                    buffer_sizes,
                    socket_options,
                    sharing,
                }) => (
                    None,
                    None,
                    bound_device.clone(),
                    None,
                    *sharing,
                    *socket_options,
                    *buffer_sizes,
                    socket_extra.clone(),
                ),
                TcpSocketState::Bound(BoundSocketState::Listener((
                    listener,
                    ListenerSharingState { sharing, listening: _ },
                    addr,
                ))) => match listener {
                    MaybeListener::Bound(BoundState {
                        buffer_sizes,
                        socket_options,
                        socket_extra,
                    }) => (
                        addr.ip.addr,
                        Some(addr.ip.identifier),
                        addr.device.clone(),
                        Some(addr.clone()),
                        *sharing,
                        *socket_options,
                        *buffer_sizes,
                        socket_extra.clone(),
                    ),
                    MaybeListener::Listener(_) => return Err(ConnectError::Listener),
                },
            };
            let remote_ip = dual_stack_remote_ip::<I, _>(remote_ip);
            match (remote_ip, core_ctx) {
                (
                    DualStackRemoteIp::ThisStack(remote_ip),
                    MaybeDualStack::NotDualStack((core_ctx, converter)),
                ) => {
                    let conn_and_addr = connect_inner(
                        core_ctx,
                        bindings_ctx,
                        &I::into_demux_socket_id(id.clone()),
                        isn,
                        listener_addr,
                        local_ip,
                        remote_ip,
                        bound_device,
                        local_port,
                        remote_port,
                        socket_extra,
                        buffer_sizes,
                        socket_options,
                        sharing,
                    )?;
                    *socket_state = TcpSocketState::Bound(BoundSocketState::Connected((
                        converter.convert_back(conn_and_addr),
                        sharing,
                    )));
                    Ok(())
                }
                (
                    DualStackRemoteIp::ThisStack(remote_ip),
                    MaybeDualStack::DualStack((core_ctx, converter)),
                ) => {
                    let conn_and_addr = connect_inner(
                        core_ctx,
                        bindings_ctx,
                        &I::into_demux_socket_id(id.clone()),
                        isn,
                        listener_addr,
                        local_ip,
                        remote_ip,
                        bound_device,
                        local_port,
                        remote_port,
                        socket_extra,
                        buffer_sizes,
                        socket_options,
                        sharing,
                    )?;
                    *socket_state = TcpSocketState::Bound(BoundSocketState::Connected((
                        converter.convert_back(EitherStack::ThisStack(conn_and_addr)),
                        sharing,
                    )));
                    Ok(())
                }
                (
                    DualStackRemoteIp::OtherStack(remote_ip),
                    MaybeDualStack::DualStack((core_ctx, converter)),
                ) => {
                    // TODO(https://fxbug.dev/136316): We don't have dual stack
                    // listeners yet, meaning we can't connect a socket that is
                    // bound in the other stack. So local_ip and listener_addr
                    // must be None for now.
                    assert_eq!(local_ip, None);
                    assert_eq!(listener_addr, None);
                    let (local_ip, listener_addr) = (None, None);
                    let conn_and_addr = connect_inner(
                        core_ctx,
                        bindings_ctx,
                        &I::into_other_demux_socket_id(id.clone()),
                        isn,
                        listener_addr,
                        local_ip,
                        remote_ip,
                        bound_device,
                        local_port,
                        remote_port,
                        socket_extra,
                        buffer_sizes,
                        socket_options,
                        sharing,
                    )?;
                    *socket_state = TcpSocketState::Bound(BoundSocketState::Connected((
                        converter.convert_back(EitherStack::OtherStack(conn_and_addr)),
                        sharing,
                    )));
                    Ok(())
                }
                (DualStackRemoteIp::OtherStack(_remote_ip), MaybeDualStack::NotDualStack(_nds)) => {
                    // [`MaybeDualStack::NotDualStack`] can only happen for IPv4
                    // sockets but [`DualStackRemoteIp::OtherStack`] can only
                    // happen for IPv6 sockets.
                    unreachable!("OtherStack socket in a non-dual-stack setting");
                }
            }
        })
    }

    fn close(&mut self, bindings_ctx: &mut BC, id: TcpSocketId<I, Self::WeakDeviceId, BC>) {
        let (destroy, pending) = self.with_socket_mut_transport_demux(
            &id,
            |core_ctx, socket_state| match socket_state {
                TcpSocketState::Unbound(_) => (true, None),
                TcpSocketState::Bound(BoundSocketState::Listener((
                    MaybeListener::Bound(_),
                    _sharing,
                    addr,
                ))) => {
                    let (core_ctx, _converter) = core_ctx.into_single_stack();
                    core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                        assert_matches!(socketmap.listeners_mut().remove(&id, &addr), Ok(()));
                    });
                    (true, None)
                }
                TcpSocketState::Bound(BoundSocketState::Listener((
                    MaybeListener::Listener(Listener {
                        accept_queue,
                        backlog: _,
                        buffer_sizes: _,
                        socket_options: _,
                    }),
                    _sharing,
                    addr,
                ))) => {
                    let (core_ctx, _converter) = core_ctx.into_single_stack();
                    core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                        assert_matches!(socketmap.listeners_mut().remove(&id, &addr), Ok(()));
                    });

                    let (pending, _socket_extra) = accept_queue.close();
                    (true, Some(pending))
                }
                TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => {
                    fn do_close<SockI, WireI, CC, BC>(
                        core_ctx: &mut CC,
                        bindings_ctx: &mut BC,
                        id: &TcpSocketId<SockI, CC::WeakDeviceId, BC>,
                        demux_id: &WireI::DemuxSocketId<CC::WeakDeviceId, BC>,
                        conn: &mut Connection<SockI, WireI, CC::WeakDeviceId, BC>,
                        addr: &ConnAddr<
                            ConnIpAddr<<WireI as Ip>::Addr, NonZeroU16, NonZeroU16>,
                            CC::WeakDeviceId,
                        >,
                    ) -> bool
                    where
                        SockI: DualStackIpExt,
                        WireI: DualStackIpExt,
                        BC: TcpBindingsContext<CC::WeakDeviceId>,
                        CC: TransportIpContext<WireI, BC>
                            + TcpDemuxContext<WireI, CC::WeakDeviceId, BC>,
                    {
                        conn.defunct = true;
                        let already_closed = match conn.state.close(
                            CloseReason::Close { now: bindings_ctx.now() },
                            &conn.socket_options,
                        ) {
                            Err(CloseError::NoConnection) => true,
                            Err(CloseError::Closing) => false,
                            Ok(()) => matches!(conn.state, State::Closed(_)),
                        };
                        if already_closed {
                            core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                                assert_matches!(
                                    socketmap.conns_mut().remove(demux_id, addr),
                                    Ok(())
                                );
                            });
                            let _: Option<_> = bindings_ctx.cancel_timer(id.downgrade().into());
                        } else {
                            do_send_inner(&id, conn, &addr, core_ctx, bindings_ctx);
                        };
                        already_closed
                    }
                    let already_closed = match core_ctx {
                        MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                            let (conn, addr) = converter.convert(conn);
                            do_close(
                                core_ctx,
                                bindings_ctx,
                                &id,
                                &I::into_demux_socket_id(id.clone()),
                                conn,
                                addr,
                            )
                        }
                        MaybeDualStack::DualStack((core_ctx, converter)) => {
                            match converter.convert(conn) {
                                EitherStack::ThisStack((conn, addr)) => do_close(
                                    core_ctx,
                                    bindings_ctx,
                                    &id,
                                    &I::into_demux_socket_id(id.clone()),
                                    conn,
                                    addr,
                                ),
                                EitherStack::OtherStack((conn, addr)) => do_close(
                                    core_ctx,
                                    bindings_ctx,
                                    &id,
                                    &I::into_other_demux_socket_id(id.clone()),
                                    conn,
                                    addr,
                                ),
                            }
                        }
                    };
                    (already_closed, None)
                }
            },
        );

        close_pending_sockets(self, bindings_ctx, pending.into_iter().flatten());

        if destroy {
            destroy_socket(self, bindings_ctx, id);
        }
    }

    fn shutdown(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        shutdown: Shutdown,
    ) -> Result<bool, NoConnection> {
        let (result, pending) = self.with_socket_mut_transport_demux(
            id,
            |core_ctx, socket_state| {
                match socket_state {
                TcpSocketState::Unbound(_) => Err(NoConnection),
                TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => {
                    if !shutdown.send {
                        return Ok((true, None));
                    }
                    fn do_shutdown<SockI, WireI, CC, BC>(
                        core_ctx: &mut CC,
                        bindings_ctx: &mut BC,
                        id: &TcpSocketId<SockI, CC::WeakDeviceId, BC>,
                        conn: &mut Connection<SockI, WireI, CC::WeakDeviceId, BC>,
                        addr: &ConnAddr<
                            ConnIpAddr<<WireI as Ip>::Addr, NonZeroU16, NonZeroU16>,
                            CC::WeakDeviceId,
                        >,
                    ) -> Result<(), NoConnection>
                    where
                        SockI: DualStackIpExt,
                        WireI: DualStackIpExt,
                        BC: TcpBindingsContext<CC::WeakDeviceId>,
                        CC: TransportIpContext<WireI, BC>
                            + TcpDemuxContext<WireI, CC::WeakDeviceId, BC>,
                    {
                        match conn.state.close(CloseReason::Shutdown, &conn.socket_options) {
                            Ok(()) => {
                                do_send_inner(id, conn, addr, core_ctx, bindings_ctx);
                                Ok(())
                            }
                            Err(CloseError::NoConnection) => Err(NoConnection),
                            Err(CloseError::Closing) => Ok(()),
                        }
                    }
                    match core_ctx {
                        MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                            let (conn, addr) = converter.convert(conn);
                            do_shutdown(core_ctx, bindings_ctx, id, conn, addr)?
                        },
                        MaybeDualStack::DualStack((core_ctx, converter)) => match converter.convert(conn) {
                            EitherStack::ThisStack((conn, addr)) => do_shutdown(core_ctx, bindings_ctx, id, conn, addr)?,
                            EitherStack::OtherStack((conn, addr)) => do_shutdown(core_ctx, bindings_ctx, id, conn, addr)?,
                        }
                    };
                    Ok((true, None))
                }
                TcpSocketState::Bound(BoundSocketState::Listener((
                    maybe_listener,
                    sharing,
                    addr,
                ))) => {
                    let (core_ctx, _converter) = core_ctx.into_single_stack();
                    if !shutdown.receive {
                        return Ok((false, None));
                    }
                    match maybe_listener {
                        MaybeListener::Bound(_) => return Err(NoConnection),
                        MaybeListener::Listener(_) => {}
                    }

                    let new_sharing =
                        core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                            let entry = socketmap
                                .listeners_mut()
                                .entry(id, addr)
                                .expect("invalid listener ID");
                            let ListenerSharingState { sharing: _, listening } = sharing;
                            assert!(*listening, "listener {id:?} is not listening");
                            let new_sharing =
                                ListenerSharingState { listening: false, ..sharing.clone() };
                            match entry.try_update_sharing(sharing, new_sharing.clone()) {
                                Ok(()) => new_sharing,
                                Err(e) => {
                                    unreachable!(
                                        "downgrading a TCP listener to bound should not fail, got {e:?}"
                                    )
                                }
                            }
                        });

                    let queued_items = replace_with::replace_with_and(
                        maybe_listener,
                        |maybe_listener| {
                        let Listener {
                            backlog: _,
                            accept_queue,
                            buffer_sizes,
                            socket_options
                        } = assert_matches!(maybe_listener,
                            MaybeListener::Listener(l) => l, "must be a listener");
                        let (pending, socket_extra) = accept_queue.close();
                        let bound_state = BoundState { buffer_sizes, socket_options, socket_extra };
                        (MaybeListener::Bound(bound_state), pending)
                    });
                    *sharing = new_sharing;

                    Ok((false, Some(queued_items)))
                }
                }
            },
        )?;

        close_pending_sockets(self, bindings_ctx, pending.into_iter().flatten());

        Ok(result)
    }

    fn set_device(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        new_device: Option<Self::DeviceId>,
    ) -> Result<(), SetDeviceError> {
        let weak_device = new_device.as_ref().map(|d| self.downgrade_device_id(d));
        self.with_socket_mut_transport_demux(id, move |core_ctx, socket_state| {
            debug!("set device on {id:?} to {new_device:?}");
            let (core_ctx, converter) = core_ctx.into_single_stack();
            match socket_state {
                TcpSocketState::Unbound(unbound) => {
                    unbound.bound_device = weak_device;
                    Ok(())
                }
                TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => {
                    let (conn, addr) = try_into_this_stack_conn_mut::<I, BC, CC>(conn, &converter)
                        .ok_or(SetDeviceError::DualStackNotSupported)?;
                    let ConnAddr {
                        device: old_device,
                        ip: ConnIpAddr { local: (local_ip, _), remote: (remote_ip, _) },
                    } = addr;

                    if !crate::socket::can_device_change(
                        Some(local_ip.as_ref()),
                        Some(remote_ip.as_ref()),
                        old_device.as_ref(),
                        new_device.as_ref(),
                    ) {
                        return Err(SetDeviceError::ZoneChange);
                    }

                    let new_socket = core_ctx
                        .new_ip_socket(
                            bindings_ctx,
                            new_device.as_ref().map(EitherDeviceId::Strong),
                            Some(*local_ip),
                            *remote_ip,
                            IpProto::Tcp.into(),
                            Default::default(),
                        )
                        .map_err(|_: (IpSockCreationError, DefaultSendOptions)| {
                            SetDeviceError::Unroutable
                        })?;

                    core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                        let entry = socketmap
                            .conns_mut()
                            .entry(&I::into_demux_socket_id(id.clone()), addr)
                            .unwrap_or_else(|| panic!("invalid listener ID {:?}", id));
                        match entry.try_update_addr(ConnAddr {
                            device: new_socket.device().cloned(),
                            ..addr.clone()
                        }) {
                            Ok(entry) => {
                                *addr = entry.get_addr().clone();
                                let Connection {
                                    ip_sock,
                                    accept_queue: _,
                                    state: _,
                                    defunct: _,
                                    socket_options: _,
                                    soft_error: _,
                                    handshake_status: _,
                                } = conn;
                                *ip_sock = new_socket;
                                Ok(())
                            }
                            Err((ExistsError, _entry)) => Err(SetDeviceError::Conflict),
                        }
                    })
                }
                TcpSocketState::Bound(BoundSocketState::Listener((_listener, _sharing, addr))) => {
                    core_ctx.with_demux_mut(move |DemuxState { socketmap, .. }| {
                        let entry = socketmap.listeners_mut().entry(&id, addr).expect("invalid ID");
                        let ListenerAddr { device: old_device, ip: ip_addr } = addr;
                        let ListenerIpAddr { identifier: _, addr: ip } = ip_addr;

                        if !crate::socket::can_device_change(
                            ip.as_ref().map(|a| a.as_ref()), /* local_ip */
                            None,                            /* remote_ip */
                            old_device.as_ref(),
                            new_device.as_ref(),
                        ) {
                            return Err(SetDeviceError::ZoneChange);
                        }

                        let ip = *ip_addr;
                        match entry.try_update_addr(ListenerAddr { device: weak_device, ip }) {
                            Ok(entry) => {
                                *addr = entry.get_addr().clone();
                                Ok(())
                            }
                            Err((ExistsError, _entry)) => Err(SetDeviceError::Conflict),
                        }
                    })
                }
            }
        })
    }

    fn get_info(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> SocketInfo<I::Addr, CC::WeakDeviceId> {
        self.with_socket_and_converter(id, |socket_state, _converter| match socket_state {
            TcpSocketState::Unbound(unbound) => SocketInfo::Unbound(unbound.into()),
            TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => {
                #[derive(GenericOverIp)]
                #[generic_over_ip(I, Ip)]
                struct WrapIn<'a, I: DualStackIpExt, D: WeakId, BT: TcpBindingsTypes>(
                    &'a I::ConnectionAndAddr<D, BT>,
                );
                #[derive(GenericOverIp)]
                #[generic_over_ip(A, IpAddress)]
                struct WrapOut<A: IpAddress, D>(ConnectionInfo<A, D>);
                let WrapOut(conn_info) = I::map_ip(
                    WrapIn(conn),
                    |WrapIn(single_stack_state)| {
                        let (_, v4_addr) = single_stack_state;
                        WrapOut(v4_addr.clone().into())
                    },
                    |WrapIn(dual_stack_state)| match dual_stack_state {
                        EitherStack::ThisStack((_, v6_addr)) => WrapOut(v6_addr.clone().into()),
                        EitherStack::OtherStack((_, v4_addr)) => {
                            let ConnAddr {
                                ip:
                                    ConnIpAddr {
                                        local: (local_ip, local_port),
                                        remote: (remote_ip, remote_port),
                                    },
                                device,
                            } = v4_addr;
                            WrapOut(ConnectionInfo {
                                local_addr: SocketAddr {
                                    ip: maybe_zoned(local_ip.addr().to_ipv6_mapped(), device),
                                    port: *local_port,
                                },
                                remote_addr: SocketAddr {
                                    ip: maybe_zoned(remote_ip.addr().to_ipv6_mapped(), device),
                                    port: *remote_port,
                                },
                                device: device.clone(),
                            })
                        }
                    },
                );
                SocketInfo::Connection(conn_info)
            }
            TcpSocketState::Bound(BoundSocketState::Listener((_listener, _sharing, addr))) => {
                SocketInfo::Bound(addr.clone().into())
            }
        })
    }

    fn do_send(&mut self, bindings_ctx: &mut BC, conn_id: &TcpSocketId<I, Self::WeakDeviceId, BC>) {
        self.with_socket_mut_transport_demux(conn_id, |core_ctx, socket_state| {
            let (conn, _sharing) = assert_matches!(
                socket_state,
                TcpSocketState::Bound(BoundSocketState::Connected(c)) => c
            );
            match core_ctx {
                MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                    let (conn, addr) = converter.convert(conn);
                    do_send_inner(conn_id, conn, addr, core_ctx, bindings_ctx);
                }
                MaybeDualStack::DualStack((core_ctx, converter)) => match converter.convert(conn) {
                    EitherStack::ThisStack((conn, addr)) => {
                        do_send_inner(conn_id, conn, addr, core_ctx, bindings_ctx)
                    }
                    EitherStack::OtherStack((conn, addr)) => {
                        do_send_inner(conn_id, conn, addr, core_ctx, bindings_ctx)
                    }
                },
            }
        })
    }

    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        weak_id: WeakTcpSocketId<I, Self::WeakDeviceId, BC>,
    ) {
        let id = match weak_id.upgrade() {
            Some(c) => c,
            None => return,
        };
        // Alias refs so we can move weak_id to the closure.
        let id_alias = &id;
        let ctx_alias = &mut *bindings_ctx;
        let defunct = self.with_socket_mut_transport_demux(&id, move |core_ctx, socket_state| {
            let id = id_alias;
            let ctx = ctx_alias;
            let (conn, _sharing) = assert_matches!(
                socket_state,
                TcpSocketState::Bound(BoundSocketState::Connected(conn)) => conn
            );
            fn do_handle_timer<SockI, WireI, CC, BC>(
                core_ctx: &mut CC,
                bindings_ctx: &mut BC,
                weak_id: WeakTcpSocketId<SockI, CC::WeakDeviceId, BC>,
                id: &TcpSocketId<SockI, CC::WeakDeviceId, BC>,
                demux_id: &WireI::DemuxSocketId<CC::WeakDeviceId, BC>,
                conn: &mut Connection<SockI, WireI, CC::WeakDeviceId, BC>,
                addr: &ConnAddr<
                    ConnIpAddr<<WireI as Ip>::Addr, NonZeroU16, NonZeroU16>,
                    CC::WeakDeviceId,
                >,
            ) -> bool
            where
                SockI: DualStackIpExt,
                WireI: DualStackIpExt,
                BC: TcpBindingsContext<CC::WeakDeviceId>,
                CC: TransportIpContext<WireI, BC> + TcpDemuxContext<WireI, CC::WeakDeviceId, BC>,
            {
                do_send_inner(id, conn, addr, core_ctx, bindings_ctx);
                if conn.defunct && matches!(conn.state, State::Closed(_)) {
                    core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                        assert_matches!(socketmap.conns_mut().remove(demux_id, addr), Ok(()));
                    });
                    let _: Option<_> = bindings_ctx.cancel_timer(weak_id.into());
                    true
                } else {
                    false
                }
            }
            match core_ctx {
                MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                    let (conn, addr) = converter.convert(conn);
                    do_handle_timer(
                        core_ctx,
                        ctx,
                        weak_id,
                        id,
                        &I::into_demux_socket_id(id.clone()),
                        conn,
                        addr,
                    )
                }
                MaybeDualStack::DualStack((core_ctx, converter)) => match converter.convert(conn) {
                    EitherStack::ThisStack((conn, addr)) => do_handle_timer(
                        core_ctx,
                        ctx,
                        weak_id,
                        id,
                        &I::into_demux_socket_id(id.clone()),
                        conn,
                        addr,
                    ),
                    EitherStack::OtherStack((conn, addr)) => do_handle_timer(
                        core_ctx,
                        ctx,
                        weak_id,
                        id,
                        &I::into_other_demux_socket_id(id.clone()),
                        conn,
                        addr,
                    ),
                },
            }
        });
        if defunct {
            // Remove the entry from the primary map and drop primary.
            destroy_socket(self, bindings_ctx, id);
        }
    }

    fn with_socket_options_mut<R, F: FnOnce(&mut SocketOptions) -> R>(
        &mut self,
        bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        f: F,
    ) -> R {
        self.with_socket_mut_transport_demux(id, |core_ctx, socket_state| match socket_state {
            TcpSocketState::Unbound(unbound) => f(&mut unbound.socket_options),
            TcpSocketState::Bound(BoundSocketState::Listener((
                MaybeListener::Bound(bound),
                _,
                _,
            ))) => f(&mut bound.socket_options),
            TcpSocketState::Bound(BoundSocketState::Listener((
                MaybeListener::Listener(listener),
                _,
                _,
            ))) => f(&mut listener.socket_options),
            TcpSocketState::Bound(BoundSocketState::Connected((conn, _))) => match core_ctx {
                MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                    let (conn, addr) = converter.convert(conn);
                    let old = conn.socket_options;
                    let result = f(&mut conn.socket_options);
                    if old != conn.socket_options {
                        do_send_inner(id, conn, &*addr, core_ctx, bindings_ctx);
                    }
                    result
                }
                MaybeDualStack::DualStack((core_ctx, converter)) => match converter.convert(conn) {
                    EitherStack::ThisStack((conn, addr)) => {
                        let old = conn.socket_options;
                        let result = f(&mut conn.socket_options);
                        if old != conn.socket_options {
                            do_send_inner(id, conn, &*addr, core_ctx, bindings_ctx);
                        }
                        result
                    }
                    EitherStack::OtherStack((conn, addr)) => {
                        let old = conn.socket_options;
                        let result = f(&mut conn.socket_options);
                        if old != conn.socket_options {
                            do_send_inner(id, conn, &*addr, core_ctx, bindings_ctx);
                        }
                        result
                    }
                },
            },
        })
    }

    fn with_socket_options<R, F: FnOnce(&SocketOptions) -> R>(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        f: F,
    ) -> R {
        self.with_socket_and_converter(id, |socket_state, converter| match socket_state {
            TcpSocketState::Unbound(unbound) => f(&unbound.socket_options),
            TcpSocketState::Bound(BoundSocketState::Listener((
                MaybeListener::Bound(bound),
                _,
                _,
            ))) => f(&bound.socket_options),
            TcpSocketState::Bound(BoundSocketState::Listener((
                MaybeListener::Listener(listener),
                _,
                _,
            ))) => f(&listener.socket_options),
            TcpSocketState::Bound(BoundSocketState::Connected((conn, _))) => {
                let socket_options = match converter {
                    MaybeDualStack::NotDualStack(converter) => {
                        let (conn, _addr) = converter.convert(conn);
                        &conn.socket_options
                    }
                    MaybeDualStack::DualStack(converter) => match converter.convert(conn) {
                        EitherStack::ThisStack((conn, _addr)) => &conn.socket_options,
                        EitherStack::OtherStack((conn, _addr)) => &conn.socket_options,
                    },
                };
                f(socket_options)
            }
        })
    }

    fn set_send_buffer_size(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        size: usize,
    ) {
        set_buffer_size::<SendBufferSize, I, BC, CC>(self, id, size)
    }

    fn send_buffer_size(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Option<usize> {
        get_buffer_size::<SendBufferSize, I, BC, CC>(self, id)
    }

    fn set_receive_buffer_size(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        size: usize,
    ) {
        set_buffer_size::<ReceiveBufferSize, I, BC, CC>(self, id, size)
    }

    fn receive_buffer_size(
        &mut self,
        _bindings_ctx: &mut BC,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Option<usize> {
        get_buffer_size::<ReceiveBufferSize, I, BC, CC>(self, id)
    }

    fn set_reuseaddr(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
        reuse: bool,
    ) -> Result<(), SetReuseAddrError> {
        let new_sharing = match reuse {
            true => SharingState::ReuseAddress,
            false => SharingState::Exclusive,
        };
        self.with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let (core_ctx, _converter) = core_ctx.into_single_stack();
            match socket_state {
                TcpSocketState::Unbound(unbound) => {
                    unbound.sharing = new_sharing;
                    Ok(())
                }
                TcpSocketState::Bound(BoundSocketState::Listener((
                    _listener,
                    old_sharing,
                    addr,
                ))) => core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                    let ListenerSharingState { listening, sharing } = *old_sharing;
                    let entry =
                        socketmap.listeners_mut().entry(&id, addr).expect("invalid socket ID");
                    if new_sharing == sharing {
                        return Ok(());
                    }
                    let new_sharing = ListenerSharingState { listening, sharing: new_sharing };
                    match entry.try_update_sharing(old_sharing, new_sharing.clone()) {
                        Ok(()) => {
                            *old_sharing = new_sharing;
                            Ok(())
                        }
                        Err(UpdateSharingError) => Err(SetReuseAddrError::AddrInUse),
                    }
                }),
                TcpSocketState::Bound(BoundSocketState::Connected(_)) => {
                    // TODO(https://fxbug.dev/97823): Support setting the option
                    // for connection sockets.
                    Err(SetReuseAddrError::NotSupported)
                }
            }
        })
    }

    fn reuseaddr(&mut self, id: &TcpSocketId<I, Self::WeakDeviceId, BC>) -> bool {
        get_reuseaddr(self, id)
    }

    fn on_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        orig_src_ip: SpecifiedAddr<I::Addr>,
        orig_dst_ip: SpecifiedAddr<I::Addr>,
        orig_src_port: NonZeroU16,
        orig_dst_port: NonZeroU16,
        seq: SeqNum,
        error: IcmpErrorCode,
    ) {
        let id = self.with_demux(|DemuxState { socketmap, .. }| {
            // TODO(https://fxbug.dev/132092): Remove panic opportunities once
            // `SocketHandler` functions take `SocketIpAddr`.
            let orig_src_ip = SocketIpAddr::new_from_specified_or_panic(orig_src_ip);
            let orig_dst_ip = SocketIpAddr::new_from_specified_or_panic(orig_dst_ip);
            socketmap
                .conns()
                .get_by_addr(&ConnAddr {
                    ip: ConnIpAddr {
                        local: (orig_src_ip, orig_src_port),
                        remote: (orig_dst_ip, orig_dst_port),
                    },
                    device: None,
                })
                .map(|ConnAddrState { sharing: _, id }| id.clone())
        });

        let id = match id {
            Some(id) => id,
            None => return,
        };

        let id = match I::into_dual_stack_ip_socket(id) {
            EitherStack::ThisStack(id) => id,
            // TODO(https://fxbug.dev/136316): Handle ICMP error for the other
            // stack.
            EitherStack::OtherStack(_) => return,
        };
        let destroy = self.with_socket_mut_transport_demux(&id, |core_ctx, socket_state| {
            let (core_ctx, converter) = core_ctx.into_single_stack();
            let Some((
                Connection {
                    accept_queue,
                    state,
                    ip_sock: _,
                    defunct: _,
                    socket_options: _,
                    soft_error,
                    handshake_status,
                },
                addr,
            )) = assert_matches!(
                socket_state,
                TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => try_into_this_stack_conn_mut::<I, BC, CC>(conn, &converter),
                "invalid socket ID"
            ) else {
                // TODO(https://fxbug.dev/136316): Destroy the other stack
                // socket once we allow that.
                return false;
            };
            *soft_error = soft_error.or(state.on_icmp_error(error, seq));

            if let State::Closed(Closed { reason }) = state {
                debug!("handshake_status: {handshake_status:?}");
                let _: bool = handshake_status.update_if_pending(HandshakeStatus::Aborted);
                match accept_queue {
                    Some(accept_queue) => {
                        accept_queue.remove(&id);
                        // Remove the socket from demux and destroy it if not
                        // held by the user.
                        core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                            assert_matches!(socketmap.conns_mut().remove(&I::into_demux_socket_id(id.clone()), addr), Ok(()));
                        });
                        return true;
                    }
                    None => {
                        if let Some(err) = reason {
                            if *err == ConnectionError::TimedOut {
                                *err = soft_error.unwrap_or(ConnectionError::TimedOut);
                            }
                        }
                    }
                }
            }

            false
        });

        if destroy {
            destroy_socket(self, bindings_ctx, id);
        }
    }

    fn get_socket_error(
        &mut self,
        id: &TcpSocketId<I, Self::WeakDeviceId, BC>,
    ) -> Option<ConnectionError> {
        self.with_socket_mut_and_converter(id, |socket_state, converter| match socket_state {
            TcpSocketState::Unbound(_) | TcpSocketState::Bound(BoundSocketState::Listener(_)) => {
                None
            }
            TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => {
                let (state, soft_error) = match converter {
                    MaybeDualStack::NotDualStack(converter) => {
                        let (conn, _addr) = converter.convert(conn);
                        (&conn.state, &mut conn.soft_error)
                    }
                    MaybeDualStack::DualStack(converter) => match converter.convert(conn) {
                        EitherStack::ThisStack((conn, _addr)) => {
                            (&conn.state, &mut conn.soft_error)
                        }
                        EitherStack::OtherStack((conn, _addr)) => {
                            (&conn.state, &mut conn.soft_error)
                        }
                    },
                };
                let hard_error = if let State::Closed(Closed { reason: hard_error }) = state {
                    hard_error.clone()
                } else {
                    None
                };
                hard_error.or_else(|| soft_error.take())
            }
        })
    }

    fn with_info<V: InfoVisitor<I, Self::WeakDeviceId>>(&mut self, visitor: &mut V) {
        self.for_each_socket(|socket_state, converter| {
            let stats: Option<SocketStats<I, _>> = match socket_state {
                TcpSocketState::Unbound(_) => Some(SocketStats { local: None, remote: None }),
                TcpSocketState::Bound(BoundSocketState::Listener((_state, _sharing, addr))) => {
                    let ListenerAddr { ip: ListenerIpAddr { identifier, addr }, ref device } =
                        *addr;
                    let local = addr.map(|addr| SocketAddr {
                        ip: maybe_zoned(*addr.as_ref(), device),
                        port: identifier,
                    });
                    Some(SocketStats { local, remote: None })
                }
                TcpSocketState::Bound(BoundSocketState::Connected((state, _sharing))) => {
                    let (defunct, addr) = match converter {
                        MaybeDualStack::NotDualStack(converter) => {
                            let (conn, addr) = converter.convert(state);
                            (conn.defunct, addr)
                        }
                        MaybeDualStack::DualStack(converter) => match converter.convert(state) {
                            EitherStack::ThisStack((conn, addr)) => (conn.defunct, addr),
                            // TODO(https://fxbug.dev/136316): Support dual stack.
                            EitherStack::OtherStack((_conn, _addr)) => return,
                        },
                    };
                    (!defunct).then(|| {
                        let ConnAddr {
                            ip:
                                ConnIpAddr {
                                    local: (local_ip, local_port),
                                    remote: (remote_ip, remote_port),
                                },
                            ref device,
                        } = *addr;
                        let local = Some(SocketAddr {
                            ip: maybe_zoned(*local_ip.as_ref(), device),
                            port: local_port,
                        });
                        let remote = Some(SocketAddr {
                            ip: maybe_zoned(*remote_ip.as_ref(), device),
                            port: remote_port,
                        });
                        SocketStats { local, remote }
                    })
                }
            };
            if let Some(stats) = stats {
                visitor.visit(stats);
            }
        });
    }
}

/// Destroys the socket with `id`.
fn destroy_socket<
    I: DualStackIpExt,
    CC: TcpContext<I, BC>,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: TcpSocketId<I, CC::WeakDeviceId, BC>,
) {
    let deferred = core_ctx.with_all_sockets_mut(move |all_sockets| {
        let entry = all_sockets.entry(id);
        let (id, primary) = match entry {
            hash_map::Entry::Occupied(o) => match o.get() {
                TcpSocketSetEntry::DeadOnArrival => {
                    let id = o.key();
                    let TcpSocketId(rc) = id;
                    debug!(
                        "{id:?} destruction skipped, socket is DOA. References={:?}",
                        StrongRc::debug_references(rc)
                    );
                    // Destruction is deferred.
                    return Some(id.downgrade());
                }
                TcpSocketSetEntry::Primary(_) => {
                    assert_matches!(o.remove_entry(), (k, TcpSocketSetEntry::Primary(p)) => (k, p))
                }
            },
            hash_map::Entry::Vacant(v) => {
                let id = v.key();
                let TcpSocketId(rc) = id;
                let refs = StrongRc::debug_references(rc);
                let weak = id.downgrade();
                if StrongRc::marked_for_destruction(rc) {
                    // Socket is already marked for destruction, we've raced
                    // this removal with the addition to the socket set. Mark
                    // the entry as DOA.
                    debug!(
                        "{id:?} raced with insertion, marking socket as DOA. References={refs:?}",
                    );
                    let _: &mut _ = v.insert(TcpSocketSetEntry::DeadOnArrival);
                } else {
                    debug!("{id:?} destruction is already deferred. References={refs:?}");
                }
                return Some(weak);
            }
        };
        struct LoggingNotifier<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>(
            WeakTcpSocketId<I, D, BT>,
        );

        impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>
            crate::sync::RcNotifier<ReferenceState<I, D, BT>> for LoggingNotifier<I, D, BT>
        {
            fn notify(&mut self, data: ReferenceState<I, D, BT>) {
                let state = data.into_inner();
                let Self(weak) = self;
                debug!("delayed-dropping {weak:?}: {state:?}");
            }
        }

        // Keep a weak ref around so we can have nice debug logs.
        let weak = id.downgrade();
        core::mem::drop(id);
        match PrimaryRc::unwrap_or_notify_with(primary, || (LoggingNotifier(weak.clone()), ())) {
            Ok(state) => {
                debug!("destroyed {weak:?} {state:?}");
                None
            }
            Err(()) => {
                let WeakTcpSocketId(rc) = &weak;
                debug!(
                    "{weak:?} has strong references left. References={:?}",
                    rc.debug_references()
                );
                Some(weak)
            }
        }
    });

    if let Some(deferred) = deferred {
        // Any situation where this is called where the socket is not actually
        // destroyed is notified back to the context so tests can assert on
        // correct behavior.
        core_ctx.socket_destruction_deferred(deferred);
    }
}

fn get_reuseaddr<
    I: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
>(
    core_ctx: &mut CC,
    id: &TcpSocketId<I, CC::WeakDeviceId, BC>,
) -> bool {
    core_ctx.with_socket(id, |socket_state| match socket_state {
        TcpSocketState::Unbound(Unbound { sharing, .. })
        | TcpSocketState::Bound(
            BoundSocketState::Connected((_, sharing))
            | BoundSocketState::Listener((_, ListenerSharingState { sharing, .. }, _)),
        ) => match sharing {
            SharingState::Exclusive => false,
            SharingState::ReuseAddress => true,
        },
    })
}

/// Closes all sockets in `pending`.
///
/// Used to cleanup all pending sockets in the accept queue when a listener
/// socket is shutdown or closed.
fn close_pending_sockets<I, CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    pending: impl Iterator<Item = TcpSocketId<I, CC::WeakDeviceId, BC>>,
) where
    I: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
{
    for conn_id in pending {
        let _: Option<BC::Instant> = bindings_ctx.cancel_timer(conn_id.downgrade().into());
        core_ctx.with_socket_mut_transport_demux(&conn_id, |core_ctx, socket_state| {
            let (core_ctx, converter)= core_ctx.into_single_stack();
            let (conn, conn_addr) = assert_matches!(
                socket_state,
                TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => match converter {
                    MaybeDualStack::NotDualStack(nds) => nds.convert(conn),
                    // TODO(https://fxbug.dev/136316): The assertion is fine
                    // because we don't have dual stack listeners.
                    MaybeDualStack::DualStack(ds) => assert_matches!(ds.convert(conn), EitherStack::ThisStack(conn) => conn),
                }
            );
            core_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                assert_matches!(
                    socketmap
                        .conns_mut()
                        .remove(&I::into_demux_socket_id(conn_id.clone()), &conn_addr),
                    Ok(())
                );
            });

            if let Some(reset) = conn.state.abort() {
                let ConnAddr { ip, device: _ } = conn_addr;
                let ser = tcp_serialize_segment(reset, *ip);
                core_ctx.send_ip_packet(bindings_ctx, &conn.ip_sock, ser, None).unwrap_or_else(
                    |(body, err)| {
                        debug!(
                            "failed to reset connection to {:?}, body: {:?}, err: {:?}",
                            ip, body, err
                        )
                    },
                );
            }
        });
        destroy_socket(core_ctx, bindings_ctx, conn_id);
    }
}

fn do_send_inner<SockI, WireI, CC, BC>(
    conn_id: &TcpSocketId<SockI, CC::WeakDeviceId, BC>,
    conn: &mut Connection<SockI, WireI, CC::WeakDeviceId, BC>,
    addr: &ConnAddr<ConnIpAddr<WireI::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
    ip_transport_ctx: &mut CC,
    bindings_ctx: &mut BC,
) where
    SockI: DualStackIpExt,
    WireI: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TransportIpContext<WireI, BC>,
{
    while let Some(seg) = conn.state.poll_send(u32::MAX, bindings_ctx.now(), &conn.socket_options) {
        let ser = tcp_serialize_segment(seg, addr.ip.clone());
        ip_transport_ctx.send_ip_packet(bindings_ctx, &conn.ip_sock, ser, None).unwrap_or_else(
            |(body, err)| {
                // Currently there are a few call sites to `do_send_inner` and they
                // don't really care about the error, with Rust's strict
                // `unused_result` lint, not returning an error that no one
                // would care makes the code less cumbersome to write. So We do
                // not return the error to caller but just log it instead. If
                // we find a case where the caller is interested in the error,
                // then we can always come back and change this.
                debug!(
                    "failed to send an ip packet on {:?}, body: {:?}, err: {:?}",
                    conn_id, body, err
                )
            },
        );
    }

    if let Some(instant) = conn.state.poll_send_at() {
        let _: Option<_> = bindings_ctx.schedule_timer_instant(instant, conn_id.downgrade().into());
    }
}

enum SendBufferSize {}
enum ReceiveBufferSize {}

trait AccessBufferSize {
    fn set_unconnected_size(sizes: &mut BufferSizes, new_size: usize);
    fn set_connected_size<
        Instant: crate::Instant + 'static,
        S: SendBuffer,
        R: ReceiveBuffer,
        P: Debug,
    >(
        state: &mut State<Instant, R, S, P>,
        new_size: usize,
    );
    fn get_buffer_size(sizes: &OptionalBufferSizes) -> Option<usize>;
}

impl AccessBufferSize for SendBufferSize {
    fn set_unconnected_size(sizes: &mut BufferSizes, new_size: usize) {
        let BufferSizes { send, receive: _ } = sizes;
        *send = new_size
    }

    fn set_connected_size<
        Instant: crate::Instant + 'static,
        S: SendBuffer,
        R: ReceiveBuffer,
        P: Debug,
    >(
        state: &mut State<Instant, R, S, P>,
        new_size: usize,
    ) {
        state.set_send_buffer_size(new_size)
    }

    fn get_buffer_size(sizes: &OptionalBufferSizes) -> Option<usize> {
        let OptionalBufferSizes { send, receive: _ } = sizes;
        *send
    }
}

impl AccessBufferSize for ReceiveBufferSize {
    fn set_unconnected_size(sizes: &mut BufferSizes, new_size: usize) {
        let BufferSizes { send: _, receive } = sizes;
        *receive = new_size
    }

    fn set_connected_size<
        Instant: crate::Instant + 'static,
        S: SendBuffer,
        R: ReceiveBuffer,
        P: Debug,
    >(
        state: &mut State<Instant, R, S, P>,
        new_size: usize,
    ) {
        state.set_receive_buffer_size(new_size)
    }

    fn get_buffer_size(sizes: &OptionalBufferSizes) -> Option<usize> {
        let OptionalBufferSizes { send: _, receive } = sizes;
        *receive
    }
}

fn set_buffer_size<
    Which: AccessBufferSize,
    I: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
>(
    core_ctx: &mut CC,
    id: &TcpSocketId<I, CC::WeakDeviceId, BC>,
    size: usize,
) {
    core_ctx.with_socket_mut_and_converter(id, |socket_state, converter| match socket_state {
        TcpSocketState::Unbound(Unbound { buffer_sizes, .. }) => {
            Which::set_unconnected_size(buffer_sizes, size)
        }
        TcpSocketState::Bound(BoundSocketState::Connected((conn, _))) => {
            let state = match converter {
                MaybeDualStack::NotDualStack(converter) => {
                    let (conn, _addr) = converter.convert(conn);
                    &mut conn.state
                }
                MaybeDualStack::DualStack(converter) => match converter.convert(conn) {
                    EitherStack::ThisStack((conn, _addr)) => &mut conn.state,
                    EitherStack::OtherStack((conn, _addr)) => &mut conn.state,
                },
            };
            Which::set_connected_size(state, size)
        }
        TcpSocketState::Bound(BoundSocketState::Listener((
            MaybeListener::Listener(Listener { buffer_sizes, .. })
            | MaybeListener::Bound(BoundState { buffer_sizes, .. }),
            _,
            _,
        ))) => Which::set_unconnected_size(buffer_sizes, size),
    })
}

fn get_buffer_size<
    Which: AccessBufferSize,
    I: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
>(
    core_ctx: &mut CC,
    id: &TcpSocketId<I, CC::WeakDeviceId, BC>,
) -> Option<usize> {
    core_ctx.with_socket_and_converter(id, |socket, converter| {
        let sizes = match socket {
            TcpSocketState::Unbound(Unbound { buffer_sizes, .. }) => buffer_sizes.into_optional(),
            TcpSocketState::Bound(BoundSocketState::Connected((conn, _))) => {
                let state = match converter {
                    MaybeDualStack::NotDualStack(converter) => {
                        let (conn, _addr) = converter.convert(conn);
                        &conn.state
                    }
                    MaybeDualStack::DualStack(converter) => match converter.convert(conn) {
                        EitherStack::ThisStack((conn, _addr)) => &conn.state,
                        EitherStack::OtherStack((conn, _addr)) => &conn.state,
                    },
                };
                state.target_buffer_sizes()
            }
            TcpSocketState::Bound(BoundSocketState::Listener((maybe_listener, _, _))) => {
                match maybe_listener {
                    MaybeListener::Bound(BoundState { buffer_sizes, .. })
                    | MaybeListener::Listener(Listener { buffer_sizes, .. }) => {
                        buffer_sizes.into_optional()
                    }
                }
            }
        };
        Which::get_buffer_size(&sizes)
    })
}

/// Creates a new socket in unbound state.
pub fn create_socket<I, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    socket_extra: BC::ListenerNotifierOrProvidedBuffers,
) -> TcpSocketId<I, WeakDeviceId<BC>, BC>
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        IpInvariant((&mut core_ctx, bindings_ctx, socket_extra)),
        |IpInvariant((core_ctx, bindings_ctx, socket_extra))| {
            SocketHandler::create_socket(core_ctx, bindings_ctx, socket_extra)
        },
        |IpInvariant((core_ctx, bindings_ctx, socket_extra))| {
            SocketHandler::create_socket(core_ctx, bindings_ctx, socket_extra)
        },
    )
}

/// Error returned when failing to set the bound device for a socket.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip()]
pub enum SetDeviceError {
    /// The socket would conflict with another socket.
    Conflict,
    /// The socket would become unroutable.
    Unroutable,
    /// The socket has an address with a different zone.
    ZoneChange,
    // TODO(https://fxbug.dev/136316): Remove this variant.
    /// Setting device not supported for dual stack.
    DualStackNotSupported,
}

/// Sets the device on a socket.
///
/// Passing `None` clears the bound device.
pub fn set_device<I, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    device: Option<DeviceId<BC>>,
) -> Result<(), SetDeviceError>
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx, device)), id),
        |(IpInvariant((core_ctx, bindings_ctx, device)), id)| {
            SocketHandler::set_device(core_ctx, bindings_ctx, id, device)
        },
        |(IpInvariant((core_ctx, bindings_ctx, device)), id)| {
            SocketHandler::set_device(core_ctx, bindings_ctx, id, device)
        },
    )
}

/// Binds an unbound socket to a local socket address.
///
/// Requests that the given socket be bound to the local address, if one is
/// provided; otherwise to all addresses. If `port` is specified (is `Some`),
/// the socket will be bound to that port. Otherwise a port will be selected to
/// not conflict with existing bound or connected sockets.
pub fn bind<I, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    local_ip: Option<SocketZonedIpAddr<I::Addr, DeviceId<BC>>>,
    port: Option<NonZeroU16>,
) -> Result<(), BindError>
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx, port)), id, local_ip),
        |(IpInvariant((core_ctx, bindings_ctx, port)), id, local_ip)| {
            SocketHandler::bind(core_ctx, bindings_ctx, id, local_ip, port)
        },
        |(IpInvariant((core_ctx, bindings_ctx, port)), id, local_ip)| {
            SocketHandler::bind(core_ctx, bindings_ctx, id, local_ip, port)
        },
    )
}

/// Listens on an already bound socket.
pub fn listen<I, BC>(
    core_ctx: &SyncCtx<BC>,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    backlog: NonZeroUsize,
) -> Result<(), ListenError>
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, backlog)), id),
        |(IpInvariant((core_ctx, backlog)), id)| SocketHandler::listen(core_ctx, id, backlog),
        |(IpInvariant((core_ctx, backlog)), id)| SocketHandler::listen(core_ctx, id, backlog),
    )
}

/// Possible errors for accept operation.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip()]
pub enum AcceptError {
    /// There is no established socket currently.
    WouldBlock,
    /// Cannot accept on this socket.
    NotSupported,
}

/// Errors for the listen operation.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip()]
pub enum ListenError {
    /// There would be a conflict with another listening socket.
    ListenerExists,
    /// Cannot listen on such socket.
    NotSupported,
}

/// Possible error for calling `shutdown` on a not-yet connected socket.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip()]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct NoConnection;

/// Error returned when attempting to set the ReuseAddress option.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip()]
pub enum SetReuseAddrError {
    /// Cannot share the address because it is already used.
    AddrInUse,
    /// Cannot set ReuseAddr on a connected socket.
    NotSupported,
}

/// Accepts an established socket from the queue of a listener socket.
pub fn accept<I: DualStackIpExt, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
) -> Result<
    (
        TcpSocketId<I, WeakDeviceId<BC>, BC>,
        SocketAddr<I::Addr, WeakDeviceId<BC>>,
        BC::ReturnedBuffers,
    ),
    AcceptError,
>
where
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut core_ctx, bindings_ctx)), id),
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            SocketHandler::accept(core_ctx, bindings_ctx, id)
                .map(|(a, b, c)| (a, b, IpInvariant(c)))
        },
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            SocketHandler::accept(core_ctx, bindings_ctx, id)
                .map(|(a, b, c)| (a, b, IpInvariant(c)))
        },
    )
    .map(|(a, b, IpInvariant(c))| (a, b, c))
}

/// Possible errors when connecting a socket.
#[derive(Debug, Error, GenericOverIp)]
#[generic_over_ip()]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum ConnectError {
    /// Cannot allocate a local port for the connection.
    #[error("Unable to allocate a port")]
    NoPort,
    /// Cannot find a route to the remote host.
    #[error("No route to remote host")]
    NoRoute,
    /// There was a problem with the provided address relating to its zone.
    #[error("{}", _0)]
    Zone(#[from] ZonedAddressError),
    /// There is an existing connection with the same 4-tuple.
    #[error("There is already a connection at the address requested")]
    ConnectionExists,
    /// Doesn't support `connect` for a listener.
    #[error("Called connect on a listener")]
    Listener,
    /// The handshake is still going on.
    #[error("The handshake has already started")]
    Pending,
    /// Cannot call connect on a connection that is already established.
    #[error("The handshake is completed")]
    Completed,
    /// The handshake is refused by the remote host.
    #[error("The handshake is aborted")]
    Aborted,
}

/// Possible errors when connecting a socket.
#[derive(Debug, Error, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub enum BindError {
    /// The socket was already bound.
    #[error("The socket was already bound")]
    AlreadyBound,
    /// The socekt cannot bind to the local address.
    #[error(transparent)]
    LocalAddressError(#[from] LocalAddressError),
}

/// Connects a socket to a remote address.
///
/// When the method returns, the connection is not guaranteed to be established.
/// It is up to the caller (Bindings) to determine when the connection has been
/// established. Bindings are free to use anything available on the platform to
/// check, for instance, signals.
pub fn connect<I, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, DeviceId<BC>>>,
    remote_port: NonZeroU16,
) -> Result<(), ConnectError>
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx, remote_port)), id, remote_ip),
        |(IpInvariant((core_ctx, bindings_ctx, remote_port)), id, remote_ip)| {
            SocketHandler::connect(core_ctx, bindings_ctx, id, remote_ip, remote_port)
        },
        |(IpInvariant((core_ctx, bindings_ctx, remote_port)), id, remote_ip)| {
            SocketHandler::connect(core_ctx, bindings_ctx, id, remote_ip, remote_port)
        },
    )
}

fn connect_inner<CC, BC, SockI, WireI>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: &WireI::DemuxSocketId<CC::WeakDeviceId, BC>,
    isn: &IsnGenerator<BC::Instant>,
    listener_addr: Option<ListenerAddr<ListenerIpAddr<WireI::Addr, NonZeroU16>, CC::WeakDeviceId>>,
    local_ip: Option<SocketIpAddr<WireI::Addr>>,
    remote_ip: ZonedAddr<SocketIpAddr<WireI::Addr>, CC::DeviceId>,
    bound_device: Option<CC::WeakDeviceId>,
    local_port: Option<NonZeroU16>,
    remote_port: NonZeroU16,
    netstack_buffers: BC::ListenerNotifierOrProvidedBuffers,
    buffer_sizes: BufferSizes,
    socket_options: SocketOptions,
    sharing: SharingState,
) -> Result<
    (
        Connection<SockI, WireI, CC::WeakDeviceId, BC>,
        ConnAddr<ConnIpAddr<WireI::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
    ),
    ConnectError,
>
where
    SockI: DualStackIpExt,
    WireI: DualStackIpExt,
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TransportIpContext<WireI, BC>
        + DeviceIpSocketHandler<WireI, BC>
        + TcpDemuxContext<WireI, CC::WeakDeviceId, BC>,
{
    let (remote_ip, device) = crate::transport::resolve_addr_with_device::<WireI::Addr, _, _, _>(
        remote_ip,
        bound_device,
    )?;

    let ip_sock = core_ctx
        .new_ip_socket(
            bindings_ctx,
            device.as_ref().map(|d| d.as_ref()),
            local_ip.map(SocketIpAddr::into),
            remote_ip,
            IpProto::Tcp.into(),
            DefaultSendOptions,
        )
        .map_err(|(err, DefaultSendOptions {})| match err {
            IpSockCreationError::Route(_) => ConnectError::NoRoute,
        })?;

    let device_mms =
        core_ctx.get_mms(bindings_ctx, &ip_sock).map_err(|_err: crate::ip::socket::MmsError| {
            // We either cannot find the route, or the device for
            // the route cannot handle the smallest TCP/IP packet.
            ConnectError::NoRoute
        })?;
    let conn_addr = core_ctx.with_demux_mut(|demux| {
        let DemuxState { port_alloc, socketmap } = demux;

        let local_port = local_port.map_or_else(
            || match port_alloc.try_alloc(&Some(*ip_sock.local_ip()), socketmap) {
                Some(port) => Ok(NonZeroU16::new(port).expect("ephemeral ports must be non-zero")),
                None => Err(ConnectError::NoPort),
            },
            Ok,
        )?;

        let conn_addr = ConnAddr {
            ip: ConnIpAddr {
                local: (*ip_sock.local_ip(), local_port),
                remote: (*ip_sock.remote_ip(), remote_port),
            },
            device: ip_sock.device().cloned(),
        };

        let _entry = socketmap
            .conns_mut()
            .try_insert(conn_addr.clone(), sharing, id.clone())
            .map_err(|(err, _sharing)| match err {
                // The connection will conflict with an existing one.
                InsertError::Exists | InsertError::ShadowerExists => ConnectError::ConnectionExists,
                // Connections don't conflict with listeners, and we should not
                // observe the following errors.
                InsertError::ShadowAddrExists | InsertError::IndirectConflict => {
                    panic!("failed to insert connection: {:?}", err)
                }
            })?;

        // If we managed to install ourselves in the demux, we need to remove
        // the previous listening form if any.
        if let Some(listener_addr) = listener_addr {
            // TODO(https://fxbug.dev/136316): This assertion is OK because we
            // don't have dual stack listeners.
            let id = assert_matches!(WireI::as_dual_stack_ip_socket(id), EitherStack::ThisStack(id) => id);
            socketmap
                .listeners_mut()
                .remove(id, &listener_addr)
                .expect("failed to remove a bound socket");
        }

        Result::<_, ConnectError>::Ok(conn_addr)
    })?;

    let isn = isn.generate::<SocketIpAddr<WireI::Addr>, NonZeroU16>(
        bindings_ctx.now(),
        conn_addr.ip.local,
        conn_addr.ip.remote,
    );

    let now = bindings_ctx.now();
    let (syn_sent, syn) = Closed::<Initial>::connect(
        isn,
        now,
        netstack_buffers,
        buffer_sizes,
        Mss::from_mms::<WireI>(device_mms).ok_or(ConnectError::NoRoute)?,
        Mss::default::<WireI>(),
        &socket_options,
    );
    let state = State::<_, BC::ReceiveBuffer, BC::SendBuffer, _>::SynSent(syn_sent);
    let poll_send_at = state.poll_send_at().expect("no retrans timer");

    // Send first SYN packet.
    core_ctx
        .send_ip_packet(bindings_ctx, &ip_sock, tcp_serialize_segment(syn, conn_addr.ip), None)
        .unwrap_or_else(|(body, err)| {
            trace!("tcp: failed to send ip packet {:?}: {:?}", body, err);
        });

    assert_eq!(bindings_ctx.schedule_timer_instant(poll_send_at, id.to_timer_id()), None);

    Ok((
        Connection {
            accept_queue: None,
            state,
            ip_sock,
            defunct: false,
            socket_options,
            soft_error: None,
            handshake_status: HandshakeStatus::Pending,
        },
        conn_addr,
    ))
}

/// Shuts down a socket.
///
/// For a connection, calling this function signals the other side of the
/// connection that we will not be sending anything over the connection; The
/// connection will still stay in the socketmap even after reaching `Closed`
/// state.
///
/// For a Listener, calling this function brings it back to bound state and
/// shutdowns all the connection that is currently ready to be accepted.
///
/// Returns Err(NoConnection) if the shutdown option does not apply. Otherwise,
/// Whether a connection has been shutdown is returned, i.e., if the socket was
/// a listener, the operation will succeed but false will be returned.
pub fn shutdown<I, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    shutdown: Shutdown,
) -> Result<bool, NoConnection>
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx, shutdown)), id),
        |(IpInvariant((core_ctx, bindings_ctx, shutdown)), id)| {
            SocketHandler::shutdown(core_ctx, bindings_ctx, id, shutdown)
        },
        |(IpInvariant((core_ctx, bindings_ctx, shutdown)), id)| {
            SocketHandler::shutdown(core_ctx, bindings_ctx, id, shutdown)
        },
    )
}

/// Closes a socket.
pub fn close<I, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: TcpSocketId<I, WeakDeviceId<BC>, BC>,
) where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx)), id),
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            SocketHandler::close(core_ctx, bindings_ctx, id)
        },
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            SocketHandler::close(core_ctx, bindings_ctx, id)
        },
    )
}

/// Sets the POSIX SO_REUSEADDR socket option on a socket.
pub fn set_reuseaddr<I, BC>(
    core_ctx: &SyncCtx<BC>,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    reuse: bool,
) -> Result<(), SetReuseAddrError>
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, reuse)), id),
        |(IpInvariant((core_ctx, reuse)), id)| SocketHandler::set_reuseaddr(core_ctx, id, reuse),
        |(IpInvariant((core_ctx, reuse)), id)| SocketHandler::set_reuseaddr(core_ctx, id, reuse),
    )
}

/// Gets the POSIX SO_REUSEADDR socket option on a socket.
pub fn reuseaddr<I, BC>(core_ctx: &SyncCtx<BC>, id: &TcpSocketId<I, WeakDeviceId<BC>, BC>) -> bool
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant(&mut core_ctx), id),
        |(IpInvariant(core_ctx), id)| SocketHandler::reuseaddr(core_ctx, id),
        |(IpInvariant(core_ctx), id)| SocketHandler::reuseaddr(core_ctx, id),
    )
}

/// Statistics about an individual socket.
pub struct SocketStats<I: Ip, D> {
    /// The local address of the socket.
    pub local: Option<SocketAddr<I::Addr, D>>,
    /// The remote address of the socket.
    pub remote: Option<SocketAddr<I::Addr, D>>,
}

/// Visitor for socket state.
pub trait InfoVisitor<I: Ip, D> {
    /// Called once for each TCP socket in the system.
    fn visit(&mut self, state: SocketStats<I, D>);
}

/// Provides access to shared and per-socket TCP stats via a visitor.
pub fn with_info<I, BC, V>(core_ctx: &SyncCtx<BC>, cb: &mut V)
where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
    V: InfoVisitor<I, device::WeakDeviceId<BC>>
        + InfoVisitor<Ipv4, device::WeakDeviceId<BC>>
        + InfoVisitor<Ipv6, device::WeakDeviceId<BC>>,
{
    let mut core_ctx = Locked::new(core_ctx);
    let IpInvariant(r) = I::map_ip(
        IpInvariant((&mut core_ctx, cb)),
        |IpInvariant((core_ctx, cb))| {
            IpInvariant(SocketHandler::<Ipv4, _>::with_info(core_ctx, cb))
        },
        |IpInvariant((core_ctx, cb))| {
            IpInvariant(SocketHandler::<Ipv6, _>::with_info(core_ctx, cb))
        },
    );
    r
}

/// Information about a socket.
#[derive(Clone, Debug, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip(A, IpAddress)]
pub enum SocketInfo<A: IpAddress, D> {
    /// Unbound socket info.
    Unbound(UnboundInfo<D>),
    /// Bound or listener socket info.
    Bound(BoundInfo<A, D>),
    /// Connection socket info.
    Connection(ConnectionInfo<A, D>),
}

/// Information about an unbound socket.
#[derive(Clone, Debug, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip()]
pub struct UnboundInfo<D> {
    /// The device the socket will be bound to.
    pub device: Option<D>,
}

/// Information about a bound socket's address.
#[derive(Clone, Debug, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip(A, IpAddress)]
pub struct BoundInfo<A: IpAddress, D> {
    /// The IP address the socket is bound to, or `None` for all local IPs.
    pub addr: Option<SocketZonedIpAddr<A, D>>,
    /// The port number the socket is bound to.
    pub port: NonZeroU16,
    /// The device the socket is bound to.
    pub device: Option<D>,
}

/// Information about a connected socket's address.
#[derive(Clone, Debug, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip(A, IpAddress)]
pub struct ConnectionInfo<A: IpAddress, D> {
    /// The local address the socket is bound to.
    pub local_addr: SocketAddr<A, D>,
    /// The remote address the socket is connected to.
    pub remote_addr: SocketAddr<A, D>,
    /// The device the socket is bound to.
    pub device: Option<D>,
}

impl<D: Clone, Extra> From<&'_ Unbound<D, Extra>> for UnboundInfo<D> {
    fn from(unbound: &Unbound<D, Extra>) -> Self {
        let Unbound {
            bound_device: device,
            buffer_sizes: _,
            socket_options: _,
            sharing: _,
            socket_extra: _,
        } = unbound;
        Self { device: device.clone() }
    }
}

fn maybe_zoned<A: IpAddress, D: Clone>(
    ip: SpecifiedAddr<A>,
    device: &Option<D>,
) -> SocketZonedIpAddr<A, D> {
    device
        .as_ref()
        .and_then(|device| {
            AddrAndZone::new(ip, device).map(|az| ZonedAddr::Zoned(az.map_zone(Clone::clone)))
        })
        .unwrap_or(ZonedAddr::Unzoned(ip))
        .into()
}

impl<A: IpAddress, D: Clone> From<ListenerAddr<ListenerIpAddr<A, NonZeroU16>, D>>
    for BoundInfo<A, D>
{
    fn from(addr: ListenerAddr<ListenerIpAddr<A, NonZeroU16>, D>) -> Self {
        let ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device } = addr;
        let addr = addr.map(|ip| maybe_zoned(ip.into(), &device));
        BoundInfo { addr, port: identifier, device }
    }
}

impl<A: IpAddress, D: Clone> From<ConnAddr<ConnIpAddr<A, NonZeroU16, NonZeroU16>, D>>
    for ConnectionInfo<A, D>
{
    fn from(addr: ConnAddr<ConnIpAddr<A, NonZeroU16, NonZeroU16>, D>) -> Self {
        let ConnAddr { ip: ConnIpAddr { local, remote }, device } = addr;
        let convert = |(ip, port): (SocketIpAddr<A>, NonZeroU16)| SocketAddr {
            ip: maybe_zoned(ip.into(), &device),
            port,
        };
        Self { local_addr: convert(local), remote_addr: convert(remote), device }
    }
}

/// Get information for a TCP socket.
pub fn get_info<I: DualStackIpExt, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
) -> SocketInfo<I::Addr, WeakDeviceId<BC>> {
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant(&mut core_ctx), id),
        |(IpInvariant(core_ctx), id)| SocketHandler::get_info(core_ctx, id),
        |(IpInvariant(core_ctx), id)| SocketHandler::get_info(core_ctx, id),
    )
}

/// Access options mutably for a TCP socket.
pub fn with_socket_options_mut<
    I: DualStackIpExt,
    BC: crate::BindingsContext,
    R,
    F: FnOnce(&mut SocketOptions) -> R,
>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    f: F,
) -> R {
    let mut core_ctx = Locked::new(core_ctx);
    let IpInvariant(r) = I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx, f)), id),
        |(IpInvariant((core_ctx, bindings_ctx, f)), id)| {
            IpInvariant(SocketHandler::with_socket_options_mut(core_ctx, bindings_ctx, id, f))
        },
        |(IpInvariant((core_ctx, bindings_ctx, f)), id)| {
            IpInvariant(SocketHandler::with_socket_options_mut(core_ctx, bindings_ctx, id, f))
        },
    );
    r
}

/// Access socket options immutably for a TCP socket.
pub fn with_socket_options<
    I: DualStackIpExt,
    BC: crate::BindingsContext,
    R,
    F: FnOnce(&SocketOptions) -> R,
>(
    core_ctx: &SyncCtx<BC>,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    f: F,
) -> R {
    let mut core_ctx = Locked::new(core_ctx);
    let IpInvariant(r) = I::map_ip(
        (IpInvariant((&mut core_ctx, f)), id),
        |(IpInvariant((core_ctx, f)), id)| {
            IpInvariant(SocketHandler::with_socket_options(core_ctx, id, f))
        },
        |(IpInvariant((core_ctx, f)), id)| {
            IpInvariant(SocketHandler::with_socket_options(core_ctx, id, f))
        },
    );
    r
}

/// Set the size of the send buffer for this socket and future derived sockets.
pub fn set_send_buffer_size<I: DualStackIpExt, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    size: usize,
) {
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx, size)), id),
        |(IpInvariant((core_ctx, bindings_ctx, size)), id)| {
            SocketHandler::set_send_buffer_size(core_ctx, bindings_ctx, id, size)
        },
        |(IpInvariant((core_ctx, bindings_ctx, size)), id)| {
            SocketHandler::set_send_buffer_size(core_ctx, bindings_ctx, id, size)
        },
    )
}

/// Get the size of the send buffer for this socket and future derived sockets.
pub fn send_buffer_size<I: DualStackIpExt, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
) -> Option<usize> {
    let mut core_ctx = Locked::new(core_ctx);
    let IpInvariant(size) = I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx)), id),
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            IpInvariant(SocketHandler::send_buffer_size(core_ctx, bindings_ctx, id))
        },
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            IpInvariant(SocketHandler::send_buffer_size(core_ctx, bindings_ctx, id))
        },
    );
    size
}

/// Set the size of the send buffer for this socket and future derived sockets.
pub fn set_receive_buffer_size<I: DualStackIpExt, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
    size: usize,
) {
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx, size)), id),
        |(IpInvariant((core_ctx, bindings_ctx, size)), id)| {
            SocketHandler::set_receive_buffer_size(core_ctx, bindings_ctx, id, size)
        },
        |(IpInvariant((core_ctx, bindings_ctx, size)), id)| {
            SocketHandler::set_receive_buffer_size(core_ctx, bindings_ctx, id, size)
        },
    )
}

/// Get the size of the receive buffer for this socket and future derived
/// sockets.
pub fn receive_buffer_size<I: DualStackIpExt, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
) -> Option<usize> {
    let mut core_ctx = Locked::new(core_ctx);
    let IpInvariant(size) = I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx)), id),
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            IpInvariant(SocketHandler::receive_buffer_size(core_ctx, bindings_ctx, id))
        },
        |(IpInvariant((core_ctx, bindings_ctx)), id)| {
            IpInvariant(SocketHandler::receive_buffer_size(core_ctx, bindings_ctx, id))
        },
    );
    size
}

/// Gets the last error on the connection.
pub fn get_socket_error<I: DualStackIpExt, BC: crate::BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
) -> Option<ConnectionError> {
    let mut core_ctx = Locked::new(core_ctx);
    let IpInvariant(err) = I::map_ip(
        (IpInvariant(&mut core_ctx), id),
        |(IpInvariant(core_ctx), id)| IpInvariant(SocketHandler::get_socket_error(core_ctx, id)),
        |(IpInvariant(core_ctx), id)| IpInvariant(SocketHandler::get_socket_error(core_ctx, id)),
    );
    err
}

/// Call this function whenever a socket can push out more data. That means either:
///
/// - A retransmission timer fires.
/// - An ack received from peer so that our send window is enlarged.
/// - The user puts data into the buffer and we are notified.
pub fn do_send<I, BC>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    conn_id: &TcpSocketId<I, WeakDeviceId<BC>, BC>,
) where
    I: DualStackIpExt,
    BC: crate::BindingsContext,
{
    let mut core_ctx = Locked::new(core_ctx);
    I::map_ip(
        (IpInvariant((&mut core_ctx, bindings_ctx)), conn_id),
        |(IpInvariant((core_ctx, bindings_ctx)), conn_id)| {
            SocketHandler::do_send(core_ctx, bindings_ctx, conn_id)
        },
        |(IpInvariant((core_ctx, bindings_ctx)), conn_id)| {
            SocketHandler::do_send(core_ctx, bindings_ctx, conn_id)
        },
    )
}

pub(crate) fn handle_timer<CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    timer_id: TimerId<CC::WeakDeviceId, BC>,
) where
    BC: TcpBindingsContext<CC::WeakDeviceId>,
    CC: TcpContext<Ipv4, BC> + TcpContext<Ipv6, BC>,
{
    match timer_id {
        TimerId::V4(conn_id) => {
            SocketHandler::<Ipv4, _>::handle_timer(core_ctx, bindings_ctx, conn_id)
        }
        TimerId::V6(conn_id) => {
            SocketHandler::<Ipv6, _>::handle_timer(core_ctx, bindings_ctx, conn_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, rc::Rc, string::String, sync::Arc, vec, vec::Vec};
    use core::{
        ffi::CStr,
        num::NonZeroU16,
        ops::{Deref, DerefMut},
        time::Duration,
    };

    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use net_declare::net_ip_v6;
    use net_types::{
        ip::{AddrSubnet, Ip, IpVersion, Ipv4, Ipv6, Ipv6SourceAddr, Mtu},
        AddrAndZone, LinkLocalAddr, Witness,
    };
    use packet::{Buf, BufferMut, ParseBuffer as _, Serializer};
    use packet_formats::{
        icmp::{Icmpv4DestUnreachableCode, Icmpv6DestUnreachableCode},
        tcp::{TcpParseArgs, TcpSegment},
    };
    use rand::Rng as _;
    use rand_xorshift::XorShiftRng;
    use test_case::test_case;

    use crate::{
        context::{
            testutil::{
                FakeFrameCtx, FakeInstant, FakeInstantCtx, FakeLinkResolutionNotifier, FakeNetwork,
                FakeNetworkContext, FakeTimerCtx, InstantAndData, PendingFrameData, StepResult,
                WithFakeFrameContext, WithFakeTimerContext, WrappedFakeCoreCtx,
            },
            InstantContext as _, SendFrameContext,
        },
        device::{
            link::LinkDevice,
            socket::DeviceSocketTypes,
            testutil::{FakeDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
            DeviceLayerStateTypes,
        },
        ip::{
            device::{
                nud::LinkResolutionContext,
                state::{
                    DualStackIpDeviceState, IpDeviceStateIpExt, Ipv4AddrConfig, Ipv4AddressEntry,
                    Ipv4DeviceState, Ipv6AddrConfig, Ipv6AddressEntry, Ipv6DadState,
                    Ipv6DeviceState,
                },
            },
            icmp::{IcmpIpExt, Icmpv4ErrorCode, Icmpv6ErrorCode},
            socket::testutil::FakeDualStackIpSocketCtx,
            socket::{
                testutil::FakeDeviceConfig, IpSocketBindingsContext, IpSocketContext, MmsError,
                SendOptions,
            },
            testutil::DualStackSendIpPacketMeta,
            types::ResolvedRoute,
            HopLimits, IpTransportContext, ResolveRouteError, SendIpPacketMeta, TransportIpContext,
        },
        sync::Mutex,
        testutil::ContextPair,
        testutil::{
            new_rng, run_with_many_seeds, set_logger_for_test, FakeCryptoRng, MonotonicIdentifier,
            TestIpExt,
        },
        transport::tcp::{
            buffer::{
                testutil::{
                    ClientBuffers, ProvidedBuffers, TestSendBuffer, WriteBackClientBuffers,
                },
                RingBuffer,
            },
            state::{TimeWait, MSL},
            ConnectionError, Mms, DEFAULT_FIN_WAIT2_TIMEOUT,
        },
        Instant as _,
    };

    use super::*;

    trait TcpTestIpExt: DualStackIpExt + TestIpExt + IpDeviceStateIpExt + DualStackIpExt {
        type SingleStackConverter: OwnedOrRefsBidirectionalConverter<
            Self::ConnectionAndAddr<FakeWeakDeviceId<FakeDeviceId>, TcpBindingsCtx<FakeDeviceId>>,
            (
                Connection<
                    Self,
                    Self,
                    FakeWeakDeviceId<FakeDeviceId>,
                    TcpBindingsCtx<FakeDeviceId>,
                >,
                ConnAddr<
                    ConnIpAddr<<Self as Ip>::Addr, NonZeroU16, NonZeroU16>,
                    FakeWeakDeviceId<FakeDeviceId>,
                >,
            ),
        >;

        type DualStackConverter: OwnedOrRefsBidirectionalConverter<
            Self::ConnectionAndAddr<FakeWeakDeviceId<FakeDeviceId>, TcpBindingsCtx<FakeDeviceId>>,
            EitherStack<
                (
                    Connection<
                        Self,
                        Self,
                        FakeWeakDeviceId<FakeDeviceId>,
                        TcpBindingsCtx<FakeDeviceId>,
                    >,
                    ConnAddr<
                        ConnIpAddr<<Self as Ip>::Addr, NonZeroU16, NonZeroU16>,
                        FakeWeakDeviceId<FakeDeviceId>,
                    >,
                ),
                (
                    Connection<
                        Self,
                        Self::OtherVersion,
                        FakeWeakDeviceId<FakeDeviceId>,
                        TcpBindingsCtx<FakeDeviceId>,
                    >,
                    ConnAddr<
                        ConnIpAddr<<Self::OtherVersion as Ip>::Addr, NonZeroU16, NonZeroU16>,
                        FakeWeakDeviceId<FakeDeviceId>,
                    >,
                ),
            >,
        >;
        fn recv_src_addr(addr: Self::Addr) -> Self::RecvSrcAddr;

        fn new_device_state(
            addrs: impl IntoIterator<Item = Self::Addr>,
            prefix: u8,
        ) -> DualStackIpDeviceState<FakeInstant>;

        fn converter() -> MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>;
    }

    struct FakeTcpState<I: TcpTestIpExt, D: FakeStrongDeviceId, BT: TcpBindingsTypes> {
        isn_generator: Rc<IsnGenerator<BT::Instant>>,
        demux: DemuxState<I, D::Weak, BT>,
        // Always destroy all sockets last so the strong references in the demux
        // are gone.
        all_sockets: TcpSocketSet<I, D::Weak, BT>,
    }

    impl<I, D, BT> Default for FakeTcpState<I, D, BT>
    where
        I: TcpTestIpExt,
        D: FakeStrongDeviceId,
        BT: TcpBindingsTypes,
        BT::Instant: Default,
    {
        fn default() -> Self {
            Self {
                isn_generator: Default::default(),
                all_sockets: Default::default(),
                demux: DemuxState {
                    socketmap: Default::default(),
                    port_alloc: PortAlloc::new(&mut FakeCryptoRng::new_xorshift(0)),
                },
            }
        }
    }

    struct FakeDualStackTcpState<D: FakeStrongDeviceId, BT: TcpBindingsTypes> {
        v4: FakeTcpState<Ipv4, D, BT>,
        v6: FakeTcpState<Ipv6, D, BT>,
    }

    impl<D, BT> Default for FakeDualStackTcpState<D, BT>
    where
        D: FakeStrongDeviceId,
        BT: TcpBindingsTypes,
        BT::Instant: Default,
    {
        fn default() -> Self {
            Self { v4: Default::default(), v6: Default::default() }
        }
    }

    type TcpCoreCtx<D, BT> = WrappedFakeCoreCtx<
        FakeDualStackTcpState<D, BT>,
        FakeDualStackIpSocketCtx<D>,
        DualStackSendIpPacketMeta<D>,
        D,
    >;

    type TcpCtx<D> = ContextPair<TcpCoreCtx<D, TcpBindingsCtx<D>>, TcpBindingsCtx<D>>;

    /// Delegate implementation to internal thing.
    impl<D: FakeStrongDeviceId> WithFakeFrameContext<DualStackSendIpPacketMeta<D>> for TcpCtx<D> {
        fn with_fake_frame_ctx_mut<
            O,
            F: FnOnce(&mut FakeFrameCtx<DualStackSendIpPacketMeta<D>>) -> O,
        >(
            &mut self,
            f: F,
        ) -> O {
            f(&mut self.core_ctx.inner.as_mut())
        }
    }

    impl<D: FakeStrongDeviceId> FakeNetworkContext for TcpCtx<D> {
        type TimerId = TimerId<D::Weak, TcpBindingsCtx<D>>;
        type SendMeta = DualStackSendIpPacketMeta<D>;
    }

    impl<D: FakeStrongDeviceId> WithFakeTimerContext<TimerId<D::Weak, TcpBindingsCtx<D>>>
        for TcpCtx<D>
    {
        fn with_fake_timer_ctx<
            O,
            F: FnOnce(&FakeTimerCtx<TimerId<D::Weak, TcpBindingsCtx<D>>>) -> O,
        >(
            &self,
            f: F,
        ) -> O {
            let Self { core_ctx: _, bindings_ctx } = self;
            f(&bindings_ctx.timers)
        }

        fn with_fake_timer_ctx_mut<
            O,
            F: FnOnce(&mut FakeTimerCtx<TimerId<D::Weak, TcpBindingsCtx<D>>>) -> O,
        >(
            &mut self,
            f: F,
        ) -> O {
            let Self { core_ctx: _, bindings_ctx } = self;
            f(&mut bindings_ctx.timers)
        }
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct TcpBindingsCtx<D: FakeStrongDeviceId> {
        rng: FakeCryptoRng<XorShiftRng>,
        timers: FakeTimerCtx<TimerId<D::Weak, Self>>,
    }

    impl<D: LinkDevice + FakeStrongDeviceId> LinkResolutionContext<D> for TcpBindingsCtx<D> {
        type Notifier = FakeLinkResolutionNotifier<D>;
    }

    /// Delegate implementation to internal thing.
    impl<D: FakeStrongDeviceId> TimerContext<TimerId<D::Weak, Self>> for TcpBindingsCtx<D> {
        fn schedule_timer_instant(
            &mut self,
            time: FakeInstant,
            id: TimerId<D::Weak, Self>,
        ) -> Option<FakeInstant> {
            self.timers.schedule_timer_instant(time, id)
        }

        fn cancel_timer(&mut self, id: TimerId<D::Weak, Self>) -> Option<FakeInstant> {
            self.timers.cancel_timer(id)
        }

        fn cancel_timers_with<F: FnMut(&TimerId<D::Weak, Self>) -> bool>(&mut self, f: F) {
            self.timers.cancel_timers_with(f);
        }

        fn scheduled_instant(&self, id: TimerId<D::Weak, Self>) -> Option<FakeInstant> {
            self.timers.scheduled_instant(id)
        }
    }

    impl<D: FakeStrongDeviceId> AsRef<FakeInstantCtx> for TcpBindingsCtx<D> {
        fn as_ref(&self) -> &FakeInstantCtx {
            &self.timers.instant
        }
    }

    impl<D: FakeStrongDeviceId> AsRef<FakeCryptoRng<XorShiftRng>> for TcpBindingsCtx<D> {
        fn as_ref(&self) -> &FakeCryptoRng<XorShiftRng> {
            &self.rng
        }
    }

    impl<D: FakeStrongDeviceId> DeviceSocketTypes for TcpBindingsCtx<D> {
        type SocketState = ();
    }

    impl<D: FakeStrongDeviceId> DeviceLayerStateTypes for TcpBindingsCtx<D> {
        type LoopbackDeviceState = ();
        type EthernetDeviceState = ();
        type DeviceIdentifier = MonotonicIdentifier;
    }

    impl<D: FakeStrongDeviceId> TracingContext for TcpBindingsCtx<D> {
        type DurationScope = ();

        fn duration(&self, _: &'static CStr) {}
    }

    impl<D: FakeStrongDeviceId> TcpBindingsTypes for TcpBindingsCtx<D> {
        type ReceiveBuffer = Arc<Mutex<RingBuffer>>;
        type SendBuffer = TestSendBuffer;
        type ReturnedBuffers = ClientBuffers;
        type ListenerNotifierOrProvidedBuffers = ProvidedBuffers;

        fn new_passive_open_buffers(
            buffer_sizes: BufferSizes,
        ) -> (Self::ReceiveBuffer, Self::SendBuffer, Self::ReturnedBuffers) {
            let client = ClientBuffers::new(buffer_sizes);
            (
                Arc::clone(&client.receive),
                TestSendBuffer::new(Arc::clone(&client.send), RingBuffer::default()),
                client,
            )
        }

        fn default_buffer_sizes() -> BufferSizes {
            BufferSizes::default()
        }
    }

    impl<I, D, BC> DeviceIpSocketHandler<I, BC> for TcpCoreCtx<D, BC>
    where
        I: TcpTestIpExt,
        D: FakeStrongDeviceId,
        BC: TcpBindingsTypes + IpSocketBindingsContext,
    {
        fn get_mms<O: SendOptions<I>>(
            &mut self,
            _bindings_ctx: &mut BC,
            _ip_sock: &IpSock<I, Self::WeakDeviceId, O>,
        ) -> Result<Mms, MmsError> {
            Ok(Mms::from_mtu::<I>(Mtu::new(1500), 0).unwrap())
        }
    }

    /// Delegate implementation to inner context.
    impl<I, D, BC> TransportIpContext<I, BC> for TcpCoreCtx<D, BC>
    where
        I: TcpTestIpExt,
        D: FakeStrongDeviceId,
        BC: TcpBindingsTypes + IpSocketBindingsContext,
        FakeDualStackIpSocketCtx<D>: TransportIpContext<I, BC, DeviceId = Self::DeviceId>,
    {
        type DevicesWithAddrIter<'a> = <FakeDualStackIpSocketCtx<D> as TransportIpContext<I, BC>>::DevicesWithAddrIter<'a>
            where Self: 'a;

        fn get_devices_with_assigned_addr(
            &mut self,
            addr: SpecifiedAddr<I::Addr>,
        ) -> Self::DevicesWithAddrIter<'_> {
            TransportIpContext::<I, BC>::get_devices_with_assigned_addr(self.inner.get_mut(), addr)
        }

        fn get_default_hop_limits(&mut self, device: Option<&Self::DeviceId>) -> HopLimits {
            TransportIpContext::<I, BC>::get_default_hop_limits(self.inner.get_mut(), device)
        }

        fn confirm_reachable_with_destination(
            &mut self,
            bindings_ctx: &mut BC,
            dst: SpecifiedAddr<I::Addr>,
            device: Option<&Self::DeviceId>,
        ) {
            TransportIpContext::<I, BC>::confirm_reachable_with_destination(
                self.inner.get_mut(),
                bindings_ctx,
                dst,
                device,
            )
        }
    }

    /// Delegate implementation to inner context.
    impl<
            I: TcpTestIpExt,
            D: FakeStrongDeviceId,
            BC: TcpBindingsTypes + IpSocketBindingsContext,
        > IpSocketContext<I, BC> for TcpCoreCtx<D, BC>
    {
        fn lookup_route(
            &mut self,
            bindings_ctx: &mut BC,
            device: Option<&Self::DeviceId>,
            local_ip: Option<SpecifiedAddr<I::Addr>>,
            addr: SpecifiedAddr<I::Addr>,
        ) -> Result<ResolvedRoute<I, Self::DeviceId>, ResolveRouteError> {
            self.inner.get_mut().lookup_route(bindings_ctx, device, local_ip, addr)
        }

        fn send_ip_packet<SS>(
            &mut self,
            bindings_ctx: &mut BC,
            SendIpPacketMeta {  device, src_ip, dst_ip, next_hop, proto, ttl, mtu }: SendIpPacketMeta<I, &Self::DeviceId, SpecifiedAddr<I::Addr>>,
            body: SS,
        ) -> Result<(), SS>
        where
            SS: Serializer,
            SS::Buffer: BufferMut,
        {
            let meta = SendIpPacketMeta::<I, _, _> {
                device: device.clone(),
                src_ip,
                dst_ip,
                next_hop,
                proto,
                ttl,
                mtu,
            }
            .into();
            self.inner.frames.send_frame(bindings_ctx, meta, body)
        }
    }

    impl<D, BC> TcpDemuxContext<Ipv4, D::Weak, BC> for TcpCoreCtx<D, BC>
    where
        D: FakeStrongDeviceId,
        BC: TcpBindingsTypes + IpSocketBindingsContext,
    {
        fn with_demux<O, F: FnOnce(&DemuxState<Ipv4, D::Weak, BC>) -> O>(&mut self, cb: F) -> O {
            cb(&self.outer.v4.demux)
        }

        fn with_demux_mut<O, F: FnOnce(&mut DemuxState<Ipv4, D::Weak, BC>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.outer.v4.demux)
        }
    }

    impl<D, BC> TcpDemuxContext<Ipv6, D::Weak, BC> for TcpCoreCtx<D, BC>
    where
        D: FakeStrongDeviceId,
        BC: TcpBindingsTypes + IpSocketBindingsContext,
    {
        fn with_demux<O, F: FnOnce(&DemuxState<Ipv6, D::Weak, BC>) -> O>(&mut self, cb: F) -> O {
            cb(&self.outer.v6.demux)
        }

        fn with_demux_mut<O, F: FnOnce(&mut DemuxState<Ipv6, D::Weak, BC>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.outer.v6.demux)
        }
    }

    impl<D: FakeStrongDeviceId, BC: TcpBindingsTypes + IpSocketBindingsContext> TcpContext<Ipv6, BC>
        for TcpCoreCtx<D, BC>
    {
        type SingleStackIpTransportAndDemuxCtx<'a> = Self;
        type SingleStackConverter = crate::convert::UninstantiableConverter;
        type DualStackIpTransportAndDemuxCtx<'a> = Self;
        type DualStackConverter = ();
        fn with_all_sockets_mut<
            O,
            F: FnOnce(&mut TcpSocketSet<Ipv6, Self::WeakDeviceId, BC>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.outer.v6.all_sockets)
        }

        fn socket_destruction_deferred(
            &mut self,
            socket: WeakTcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
        ) {
            let WeakTcpSocketId(rc) = &socket;
            panic!(
                "deferred socket {socket:?} destruction, references = {:?}",
                rc.debug_references()
            );
        }

        fn for_each_socket<
            F: FnMut(
                &TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
                MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
            ),
        >(
            &mut self,
            _cb: F,
        ) {
            unimplemented!()
        }

        fn with_socket_mut_isn_transport_demux<
            O,
            F: for<'a> FnOnce(
                MaybeDualStack<
                    (&'a mut Self::DualStackIpTransportAndDemuxCtx<'a>, Self::DualStackConverter),
                    (
                        &'a mut Self::SingleStackIpTransportAndDemuxCtx<'a>,
                        Self::SingleStackConverter,
                    ),
                >,
                &mut TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
                &IsnGenerator<BC::Instant>,
            ) -> O,
        >(
            &mut self,
            id: &TcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
            cb: F,
        ) -> O {
            let isn = Rc::clone(&self.outer.v6.isn_generator);
            cb(MaybeDualStack::DualStack((self, ())), id.get_mut().deref_mut(), isn.deref())
        }

        fn with_socket_and_converter<
            O,
            F: FnOnce(
                &TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
                MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
            ) -> O,
        >(
            &mut self,
            id: &TcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
            cb: F,
        ) -> O {
            cb(id.get_mut().deref_mut(), MaybeDualStack::DualStack(()))
        }
    }

    impl<D: FakeStrongDeviceId, BC: TcpBindingsTypes + IpSocketBindingsContext> TcpContext<Ipv4, BC>
        for TcpCoreCtx<D, BC>
    {
        type SingleStackIpTransportAndDemuxCtx<'a> = Self;
        type SingleStackConverter = ();
        type DualStackIpTransportAndDemuxCtx<'a> = Self;
        type DualStackConverter = crate::convert::UninstantiableConverter;
        fn with_all_sockets_mut<
            O,
            F: FnOnce(&mut TcpSocketSet<Ipv4, Self::WeakDeviceId, BC>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.outer.v4.all_sockets)
        }

        fn socket_destruction_deferred(
            &mut self,
            socket: WeakTcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
        ) {
            let WeakTcpSocketId(rc) = &socket;
            panic!(
                "deferred socket {socket:?} destruction, references = {:?}",
                rc.debug_references()
            );
        }

        fn for_each_socket<
            F: FnMut(
                &TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
                MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
            ),
        >(
            &mut self,
            _cb: F,
        ) {
            unimplemented!()
        }

        fn with_socket_mut_isn_transport_demux<
            O,
            F: for<'a> FnOnce(
                MaybeDualStack<
                    (&'a mut Self::DualStackIpTransportAndDemuxCtx<'a>, Self::DualStackConverter),
                    (
                        &'a mut Self::SingleStackIpTransportAndDemuxCtx<'a>,
                        Self::SingleStackConverter,
                    ),
                >,
                &mut TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
                &IsnGenerator<BC::Instant>,
            ) -> O,
        >(
            &mut self,
            id: &TcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
            cb: F,
        ) -> O {
            let isn: Rc<IsnGenerator<<BC as InstantBindingsTypes>::Instant>> =
                Rc::clone(&self.outer.v4.isn_generator);
            cb(MaybeDualStack::NotDualStack((self, ())), id.get_mut().deref_mut(), isn.deref())
        }

        fn with_socket_and_converter<
            O,
            F: FnOnce(
                &TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
                MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>,
            ) -> O,
        >(
            &mut self,
            id: &TcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
            cb: F,
        ) -> O {
            cb(id.get_mut().deref_mut(), MaybeDualStack::NotDualStack(()))
        }
    }

    impl TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>> {
        fn new<I: TcpTestIpExt>(
            addr: SpecifiedAddr<I::Addr>,
            peer: SpecifiedAddr<I::Addr>,
            _prefix: u8,
        ) -> Self {
            Self::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new(core::iter::once(FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![addr],
                    remote_ips: vec![peer],
                })),
                FakeDualStackTcpState::default(),
            )
        }
    }

    impl TcpCoreCtx<MultipleDevicesId, TcpBindingsCtx<MultipleDevicesId>> {
        fn new_multiple_devices() -> Self {
            Self::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new(core::iter::empty::<
                    FakeDeviceConfig<MultipleDevicesId, SpecifiedAddr<IpAddr>>,
                >()),
                Default::default(),
            )
        }
    }

    const LOCAL: &'static str = "local";
    const REMOTE: &'static str = "remote";
    const PORT_1: NonZeroU16 = const_unwrap_option(NonZeroU16::new(42));
    const PORT_2: NonZeroU16 = const_unwrap_option(NonZeroU16::new(43));

    impl TcpTestIpExt for Ipv4 {
        type SingleStackConverter = ();
        type DualStackConverter = crate::convert::UninstantiableConverter;
        fn converter() -> MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter> {
            MaybeDualStack::NotDualStack(())
        }
        fn recv_src_addr(addr: Self::Addr) -> Self::RecvSrcAddr {
            addr
        }

        fn new_device_state(
            addrs: impl IntoIterator<Item = Self::Addr>,
            prefix: u8,
        ) -> DualStackIpDeviceState<FakeInstant> {
            let ipv4 = Ipv4DeviceState::default();
            for addr in addrs {
                let _addr_id = ipv4
                    .ip_state
                    .addrs
                    .write()
                    .add(Ipv4AddressEntry::new(
                        AddrSubnet::new(addr, prefix).unwrap(),
                        Ipv4AddrConfig::default(),
                    ))
                    .expect("failed to add address");
            }
            DualStackIpDeviceState { ipv4, ipv6: Default::default() }
        }
    }

    impl TcpTestIpExt for Ipv6 {
        type SingleStackConverter = crate::convert::UninstantiableConverter;
        type DualStackConverter = ();
        fn converter() -> MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter> {
            MaybeDualStack::DualStack(())
        }
        fn recv_src_addr(addr: Self::Addr) -> Self::RecvSrcAddr {
            Ipv6SourceAddr::new(addr).unwrap()
        }

        fn new_device_state(
            addrs: impl IntoIterator<Item = Self::Addr>,
            prefix: u8,
        ) -> DualStackIpDeviceState<FakeInstant> {
            let ipv6 = Ipv6DeviceState::default();
            for addr in addrs {
                let _addr_id = ipv6
                    .ip_state
                    .addrs
                    .write()
                    .add(Ipv6AddressEntry::new(
                        AddrSubnet::new(addr, prefix).unwrap(),
                        Ipv6DadState::Assigned,
                        Ipv6AddrConfig::default(),
                    ))
                    .expect("failed to add address");
            }
            DualStackIpDeviceState { ipv4: Default::default(), ipv6 }
        }
    }

    type TcpTestNetwork = FakeNetwork<
        &'static str,
        DualStackSendIpPacketMeta<FakeDeviceId>,
        TcpCtx<FakeDeviceId>,
        fn(
            &'static str,
            DualStackSendIpPacketMeta<FakeDeviceId>,
        ) -> Vec<(
            &'static str,
            DualStackSendIpPacketMeta<FakeDeviceId>,
            Option<core::time::Duration>,
        )>,
    >;

    fn new_test_net<I: TcpTestIpExt>() -> TcpTestNetwork {
        FakeNetwork::new(
            [
                (
                    LOCAL,
                    TcpCtx {
                        core_ctx: TcpCoreCtx::new::<I>(
                            I::FAKE_CONFIG.local_ip,
                            I::FAKE_CONFIG.remote_ip,
                            I::FAKE_CONFIG.subnet.prefix(),
                        ),
                        bindings_ctx: TcpBindingsCtx::default(),
                    },
                ),
                (
                    REMOTE,
                    TcpCtx {
                        core_ctx: TcpCoreCtx::new::<I>(
                            I::FAKE_CONFIG.remote_ip,
                            I::FAKE_CONFIG.local_ip,
                            I::FAKE_CONFIG.subnet.prefix(),
                        ),
                        bindings_ctx: TcpBindingsCtx::default(),
                    },
                ),
            ],
            move |net, meta: DualStackSendIpPacketMeta<_>| {
                if net == LOCAL {
                    alloc::vec![(REMOTE, meta, None)]
                } else {
                    alloc::vec![(LOCAL, meta, None)]
                }
            },
        )
    }

    /// Utilities for accessing locked internal state in tests.
    impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> TcpSocketId<I, D, BT> {
        fn get(&self) -> impl Deref<Target = TcpSocketState<I, D, BT>> + '_ {
            let Self(rc) = self;
            rc.read()
        }

        fn get_mut(&self) -> impl DerefMut<Target = TcpSocketState<I, D, BT>> + '_ {
            let Self(rc) = self;
            rc.write()
        }
    }

    fn handle_frame(
        TcpCtx { core_ctx, bindings_ctx }: &mut TcpCtx<FakeDeviceId>,
        meta: DualStackSendIpPacketMeta<FakeDeviceId>,
        buffer: Buf<Vec<u8>>,
    ) {
        match meta {
            DualStackSendIpPacketMeta::V4(meta) => {
                <TcpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    core_ctx,
                    bindings_ctx,
                    &FakeDeviceId,
                    Ipv4::recv_src_addr(*meta.src_ip),
                    meta.dst_ip,
                    buffer,
                )
                .expect("failed to deliver bytes");
            }
            DualStackSendIpPacketMeta::V6(meta) => {
                <TcpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    core_ctx,
                    bindings_ctx,
                    &FakeDeviceId,
                    Ipv6::recv_src_addr(*meta.src_ip),
                    meta.dst_ip,
                    buffer,
                )
                .expect("failed to deliver bytes");
            }
        }
    }

    fn handle_timer<D: FakeStrongDeviceId>(
        TcpCtx { core_ctx, bindings_ctx }: &mut TcpCtx<D>,
        _: &mut (),
        timer_id: TimerId<D::Weak, TcpBindingsCtx<D>>,
    ) {
        match timer_id {
            TimerId::V4(id) => SocketHandler::<Ipv4, _>::handle_timer(core_ctx, bindings_ctx, id),
            TimerId::V6(id) => SocketHandler::<Ipv6, _>::handle_timer(core_ctx, bindings_ctx, id),
        }
    }

    fn assert_this_stack_conn<
        'a,
        I: DualStackIpExt,
        BC: TcpBindingsContext<CC::WeakDeviceId>,
        CC: TcpContext<I, BC>,
    >(
        conn: &'a I::ConnectionAndAddr<CC::WeakDeviceId, BC>,
        converter: &MaybeDualStack<CC::DualStackConverter, CC::SingleStackConverter>,
    ) -> &'a (
        Connection<I, I, CC::WeakDeviceId, BC>,
        ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
    ) {
        match converter {
            MaybeDualStack::NotDualStack(nds) => nds.convert(conn),
            MaybeDualStack::DualStack(ds) => {
                assert_matches!(ds.convert(conn), EitherStack::ThisStack(conn) => conn)
            }
        }
    }

    /// How to bind the client socket in `bind_listen_connect_accept_inner`.
    struct BindConfig {
        /// Which port to bind the client to.
        client_port: Option<NonZeroU16>,
        /// Which port to bind the server to.
        server_port: NonZeroU16,
        /// Whether to set REUSE_ADDR for the client.
        client_reuse_addr: bool,
    }

    /// The following test sets up two connected testing context - one as the
    /// server and the other as the client. Tests if a connection can be
    /// established using `bind`, `listen`, `connect` and `accept`.
    ///
    /// # Arguments
    ///
    /// * `listen_addr` - The address to listen on.
    /// * `bind_config` - Specifics about how to bind the client socket.
    ///
    /// # Returns
    ///
    /// Returns a tuple of
    ///   - the created test network.
    ///   - the client socket from local.
    ///   - the send end of the client socket.
    ///   - the accepted socket from remote.
    fn bind_listen_connect_accept_inner<I: Ip + TcpTestIpExt>(
        listen_addr: I::Addr,
        BindConfig { client_port, server_port, client_reuse_addr }: BindConfig,
        seed: u128,
        drop_rate: f64,
    ) -> (
        TcpTestNetwork,
        TcpSocketId<I, FakeWeakDeviceId<FakeDeviceId>, TcpBindingsCtx<FakeDeviceId>>,
        Arc<Mutex<Vec<u8>>>,
        TcpSocketId<I, FakeWeakDeviceId<FakeDeviceId>, TcpBindingsCtx<FakeDeviceId>>,
    )
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        let mut net = new_test_net::<I>();
        let mut rng = new_rng(seed);

        let mut maybe_drop_frame =
            |ctx: &mut TcpCtx<_>, meta: DualStackSendIpPacketMeta<_>, buffer: Buf<Vec<u8>>| {
                let x: f64 = rng.gen();
                if x > drop_rate {
                    handle_frame(ctx, meta, buffer);
                }
            };

        let backlog = NonZeroUsize::new(1).unwrap();
        let server = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let server =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &server,
                SpecifiedAddr::new(listen_addr).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(server_port),
            )
            .expect("failed to bind the server socket");
            SocketHandler::listen(core_ctx, &server, backlog).expect("can listen");
            server
        });

        let client_ends = WriteBackClientBuffers::default();
        let client = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket = SocketHandler::<I, _>::create_socket(
                core_ctx,
                bindings_ctx,
                ProvidedBuffers::Buffers(client_ends.clone()),
            );
            if client_reuse_addr {
                SocketHandler::set_reuseaddr(core_ctx, &socket, true).expect("can set");
            }
            if let Some(port) = client_port {
                SocketHandler::bind(
                    core_ctx,
                    bindings_ctx,
                    &socket,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                    Some(port),
                )
                .expect("failed to bind the client socket")
            }
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                server_port,
            )
            .expect("failed to connect");
            socket
        });
        // If drop rate is 0, the SYN is guaranteed to be delivered, so we can
        // look at the SYN queue deterministically.
        if drop_rate == 0.0 {
            // Step once for the SYN packet to be sent.
            let _: StepResult = net.step(handle_frame, handle_timer);
            // The listener should create a pending socket.
            assert_matches!(
                server.get().deref(),
                TcpSocketState::Bound(BoundSocketState::Listener((
                    MaybeListener::Listener(Listener {
                        accept_queue,
                        ..
                    }), ..))) => {
                    assert_eq!(accept_queue.ready_len(), 0);
                    assert_eq!(accept_queue.pending_len(), 1);
                }
            );
            // The handshake is not done, calling accept here should not succeed.
            net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
                assert_matches!(
                    SocketHandler::accept(core_ctx, bindings_ctx, &server),
                    Err(AcceptError::WouldBlock)
                );
            });
        }

        // Step the test network until the handshake is done.
        net.run_until_idle(&mut maybe_drop_frame, handle_timer);
        let (accepted, addr, accepted_ends) =
            net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
                SocketHandler::accept(core_ctx, bindings_ctx, &server).expect("failed to accept")
            });
        if let Some(port) = client_port {
            assert_eq!(
                addr,
                SocketAddr { ip: ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into(), port: port }
            );
        } else {
            assert_eq!(addr.ip, ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into());
        }

        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &client,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                    server_port,
                ),
                Ok(())
            );
        });

        let assert_connected = |conn_id: &TcpSocketId<I, _, _>| {
            assert_matches!(
            conn_id.get().deref(),
            TcpSocketState::Bound(BoundSocketState::Connected((
                conn, _sharing))) => {
                    let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                    assert_matches!(
                        conn,
                        Connection {
                            accept_queue: None,
                            state: State::Established(_),
                            ip_sock: _,
                            defunct: false,
                            socket_options: _,
                            soft_error: None,
                            handshake_status: HandshakeStatus::Completed { reported: true },
                        }
                    );
                })
        };

        assert_connected(&client);
        assert_connected(&accepted);

        let ClientBuffers { send: client_snd_end, receive: client_rcv_end } =
            client_ends.0.as_ref().lock().take().unwrap();
        let ClientBuffers { send: accepted_snd_end, receive: accepted_rcv_end } = accepted_ends;
        for snd_end in [client_snd_end.clone(), accepted_snd_end] {
            snd_end.lock().extend_from_slice(b"Hello");
        }

        for (c, id) in [(LOCAL, &client), (REMOTE, &accepted)] {
            net.with_context(c, |TcpCtx { core_ctx, bindings_ctx }| {
                SocketHandler::<I, _>::do_send(core_ctx, bindings_ctx, id)
            })
        }
        net.run_until_idle(&mut maybe_drop_frame, handle_timer);

        for rcv_end in [client_rcv_end, accepted_rcv_end] {
            assert_eq!(
                rcv_end.lock().read_with(|avail| {
                    let avail = avail.concat();
                    assert_eq!(avail, b"Hello");
                    avail.len()
                }),
                5
            );
        }

        // Check the listener is in correct state.
        assert_matches!(
            server.get().deref(),
            TcpSocketState::Bound(BoundSocketState::Listener((MaybeListener::Listener(l),..))) => {
                assert_eq!(l, &Listener::new(
                    backlog,
                    BufferSizes::default(),
                    SocketOptions::default(),
                    Default::default()
                ));
            }
        );

        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::shutdown(
                    core_ctx,
                    bindings_ctx,
                    &server,
                    Shutdown { receive: true, send: false },
                ),
                Ok(false)
            );
            SocketHandler::close(core_ctx, bindings_ctx, server);
        });

        (net, client, client_snd_end, accepted)
    }

    #[test]
    fn test_socket_addr_display() {
        assert_eq!(
            format!(
                "{}",
                SocketAddr {
                    ip: maybe_zoned(
                        SpecifiedAddr::new(Ipv4Addr::new([192, 168, 0, 1]))
                            .expect("failed to create specified addr"),
                        &None::<usize>,
                    ),
                    port: NonZeroU16::new(1024).expect("failed to create NonZeroU16"),
                }
            ),
            String::from("192.168.0.1:1024"),
        );
        assert_eq!(
            format!(
                "{}",
                SocketAddr {
                    ip: maybe_zoned(
                        SpecifiedAddr::new(Ipv6Addr::new([0x2001, 0xDB8, 0, 0, 0, 0, 0, 1]))
                            .expect("failed to create specified addr"),
                        &None::<usize>,
                    ),
                    port: NonZeroU16::new(1024).expect("failed to create NonZeroU16"),
                }
            ),
            String::from("[2001:db8::1]:1024")
        );
        assert_eq!(
            format!(
                "{}",
                SocketAddr {
                    ip: maybe_zoned(
                        SpecifiedAddr::new(Ipv6Addr::new([0xFE80, 0, 0, 0, 0, 0, 0, 1]))
                            .expect("failed to create specified addr"),
                        &Some(42),
                    ),
                    port: NonZeroU16::new(1024).expect("failed to create NonZeroU16"),
                }
            ),
            String::from("[fe80::1%42]:1024")
        );
    }

    #[ip_test]
    #[test_case(BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false }, I::UNSPECIFIED_ADDRESS)]
    #[test_case(BindConfig { client_port: Some(PORT_1), server_port: PORT_1, client_reuse_addr: false }, I::UNSPECIFIED_ADDRESS)]
    #[test_case(BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: true }, I::UNSPECIFIED_ADDRESS)]
    #[test_case(BindConfig { client_port: Some(PORT_1), server_port: PORT_1, client_reuse_addr: true }, I::UNSPECIFIED_ADDRESS)]
    #[test_case(BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false }, *<I as TestIpExt>::FAKE_CONFIG.remote_ip)]
    #[test_case(BindConfig { client_port: Some(PORT_1), server_port: PORT_1, client_reuse_addr: false }, *<I as TestIpExt>::FAKE_CONFIG.remote_ip)]
    #[test_case(BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: true }, *<I as TestIpExt>::FAKE_CONFIG.remote_ip)]
    #[test_case(BindConfig { client_port: Some(PORT_1), server_port: PORT_1, client_reuse_addr: true }, *<I as TestIpExt>::FAKE_CONFIG.remote_ip)]
    fn bind_listen_connect_accept<I: Ip + TcpTestIpExt>(
        bind_config: BindConfig,
        listen_addr: I::Addr,
    ) where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        let (_net, _client, _client_snd_end, _accepted) =
            bind_listen_connect_accept_inner::<I>(listen_addr, bind_config, 0, 0.0);
    }

    #[ip_test]
    #[test_case(*<I as TestIpExt>::FAKE_CONFIG.local_ip; "same addr")]
    #[test_case(I::UNSPECIFIED_ADDRESS; "any addr")]
    fn bind_conflict<I: Ip + TcpTestIpExt>(conflict_addr: I::Addr)
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        set_logger_for_test();
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        let s1 = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        let s2 = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );

        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &s1,
            Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
            Some(PORT_1),
        )
        .expect("first bind should succeed");
        assert_matches!(
            SocketHandler::bind(
                &mut core_ctx,
                &mut bindings_ctx,
                &s2,
                SpecifiedAddr::new(conflict_addr).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(PORT_1)
            ),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &s2,
            SpecifiedAddr::new(conflict_addr).map(|a| ZonedAddr::Unzoned(a).into()),
            Some(PORT_2),
        )
        .expect("able to rebind to a free address");
    }

    #[ip_test]
    #[test_case(const_unwrap_option(NonZeroU16::new(u16::MAX)), Ok(const_unwrap_option(NonZeroU16::new(u16::MAX))); "ephemeral available")]
    #[test_case(const_unwrap_option(NonZeroU16::new(100)), Err(LocalAddressError::FailedToAllocateLocalPort);
                "no ephemeral available")]
    fn bind_picked_port_all_others_taken<I: Ip + TcpTestIpExt>(
        available_port: NonZeroU16,
        expected_result: Result<NonZeroU16, LocalAddressError>,
    ) where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        for port in 1..=u16::MAX {
            let port = NonZeroU16::new(port).unwrap();
            if port == available_port {
                continue;
            }
            let socket =
                SocketHandler::create_socket(&mut core_ctx, &mut bindings_ctx, Default::default());

            SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, Some(port))
                .expect("uncontested bind");
            SocketHandler::listen(
                &mut core_ctx,
                &socket,
                const_unwrap_option(NonZeroUsize::new(1)),
            )
            .expect("can listen");
        }

        // Now that all but the LOCAL_PORT are occupied, ask the stack to
        // select a port.
        let socket = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        let result = SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, None)
            .map(|()| {
                assert_matches!(
                    SocketHandler::get_info(&mut core_ctx, &socket),
                    SocketInfo::Bound(bound) => bound.port
                )
            });
        assert_eq!(result, expected_result.map_err(From::from));
    }

    #[ip_test]
    fn bind_to_non_existent_address<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        let unbound = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        assert_matches!(
            SocketHandler::bind(
                &mut core_ctx,
                &mut bindings_ctx,
                &unbound,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                None
            ),
            Err(BindError::LocalAddressError(LocalAddressError::AddressMismatch))
        );

        assert_matches!(unbound.get().deref(), TcpSocketState::Unbound(_));
    }

    #[test]
    fn bind_addr_requires_zone() {
        let local_ip = LinkLocalAddr::new(net_ip_v6!("fe80::1")).unwrap().into_specified();

        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(
                Ipv6::FAKE_CONFIG.local_ip,
                Ipv6::FAKE_CONFIG.remote_ip,
                Ipv6::FAKE_CONFIG.subnet.prefix(),
            ));
        let unbound = SocketHandler::<Ipv6, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        assert_matches!(
            SocketHandler::bind(
                &mut core_ctx,
                &mut bindings_ctx,
                &unbound,
                Some(ZonedAddr::Unzoned(local_ip).into()),
                None
            ),
            Err(BindError::LocalAddressError(LocalAddressError::Zone(
                ZonedAddressError::RequiredZoneNotProvided
            )))
        );

        assert_matches!(unbound.get().deref(), TcpSocketState::Unbound(_));
    }

    #[test]
    fn connect_bound_requires_zone() {
        let ll_ip = LinkLocalAddr::new(net_ip_v6!("fe80::1")).unwrap().into_specified();

        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(
                Ipv6::FAKE_CONFIG.local_ip,
                Ipv6::FAKE_CONFIG.remote_ip,
                Ipv6::FAKE_CONFIG.subnet.prefix(),
            ));
        let socket = SocketHandler::<Ipv6, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, None)
            .expect("bind succeeds");
        assert_matches!(
            SocketHandler::connect(
                &mut core_ctx,
                &mut bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(ll_ip).into()),
                PORT_1,
            ),
            Err(ConnectError::Zone(ZonedAddressError::RequiredZoneNotProvided))
        );

        assert_matches!(socket.get().deref(), TcpSocketState::Bound(_));
    }

    #[test]
    fn connect_unbound_picks_link_local_source_addr() {
        set_logger_for_test();
        let client_ip = SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap();
        let server_ip = SpecifiedAddr::new(net_ip_v6!("1:2:3:4::")).unwrap();
        let mut net = FakeNetwork::new(
            [
                (LOCAL, TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(client_ip, server_ip, 0))),
                (REMOTE, TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(server_ip, client_ip, 0))),
            ],
            |net, meta| {
                if net == LOCAL {
                    alloc::vec![(REMOTE, meta, None)]
                } else {
                    alloc::vec![(LOCAL, meta, None)]
                }
            },
        );
        const PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(100));
        let client_connection = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket: TcpSocketId<Ipv6, _, _> =
                SocketHandler::<Ipv6, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(server_ip).into()),
                PORT,
            )
            .expect("can connect");
            socket
        });
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<Ipv6, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(core_ctx, bindings_ctx, &socket, None, Some(PORT))
                .expect("failed to bind the client socket");
            let _listener =
                SocketHandler::listen(core_ctx, &socket, NonZeroUsize::MIN).expect("can listen");
        });

        // Advance until the connection is established.
        net.run_until_idle(handle_frame, handle_timer);

        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &client_connection,
                    Some(ZonedAddr::Unzoned(server_ip).into()),
                    PORT
                ),
                Ok(())
            );

            let info = assert_matches!(
                SocketHandler::get_info(core_ctx, &client_connection),
                SocketInfo::Connection(info) => info
            );
            // The local address picked for the connection is link-local, which
            // means the device for the connection must also be set (since the
            // address requires a zone).
            let (local_ip, remote_ip) = assert_matches!(
                info,
                ConnectionInfo {
                    local_addr: SocketAddr { ip: local_ip, port: _ },
                    remote_addr: SocketAddr { ip: remote_ip, port: PORT },
                    device: Some(FakeWeakDeviceId(FakeDeviceId))
                } => (local_ip, remote_ip)
            );
            assert_eq!(
                local_ip.into_inner(),
                ZonedAddr::Zoned(
                    AddrAndZone::new(client_ip, FakeWeakDeviceId(FakeDeviceId)).unwrap()
                )
            );
            assert_eq!(remote_ip.into_inner(), ZonedAddr::Unzoned(server_ip));

            // Double-check that the bound device can't be changed after being set
            // implicitly.
            assert_matches!(
                SocketHandler::set_device(core_ctx, bindings_ctx, &client_connection, None),
                Err(SetDeviceError::ZoneChange)
            );
        });
    }

    #[test]
    fn accept_connect_picks_link_local_addr() {
        set_logger_for_test();
        let server_ip = SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap();
        let client_ip = SpecifiedAddr::new(net_ip_v6!("1:2:3:4::")).unwrap();
        let mut net = FakeNetwork::new(
            [
                (LOCAL, TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(server_ip, client_ip, 0))),
                (REMOTE, TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(client_ip, server_ip, 0))),
            ],
            |net, meta| {
                if net == LOCAL {
                    alloc::vec![(REMOTE, meta, None)]
                } else {
                    alloc::vec![(LOCAL, meta, None)]
                }
            },
        );
        const PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(100));
        let server_listener = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket: TcpSocketId<Ipv6, _, _> =
                SocketHandler::<Ipv6, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(core_ctx, bindings_ctx, &socket, None, Some(PORT))
                .expect("failed to bind the client socket");
            SocketHandler::listen(core_ctx, &socket, NonZeroUsize::MIN).expect("can listen");
            socket
        });
        let client_connection = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<Ipv6, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Zoned(AddrAndZone::new(server_ip, FakeDeviceId).unwrap()).into()),
                PORT,
            )
            .expect("failed to open a connection");
            socket
        });

        // Advance until the connection is established.
        net.run_until_idle(handle_frame, handle_timer);

        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let (server_connection, _addr, _buffers) =
                SocketHandler::accept(core_ctx, bindings_ctx, &server_listener)
                    .expect("connection is waiting");

            let info = assert_matches!(
                SocketHandler::get_info(core_ctx, &server_connection),
                SocketInfo::Connection(info) => info
            );
            // The local address picked for the connection is link-local, which
            // means the device for the connection must also be set (since the
            // address requires a zone).
            let (local_ip, remote_ip) = assert_matches!(
                info,
                ConnectionInfo {
                    local_addr: SocketAddr { ip: local_ip, port: PORT },
                    remote_addr: SocketAddr { ip: remote_ip, port: _ },
                    device: Some(FakeWeakDeviceId(FakeDeviceId))
                } => (local_ip, remote_ip)
            );
            assert_eq!(
                local_ip.into_inner(),
                ZonedAddr::Zoned(
                    AddrAndZone::new(server_ip, FakeWeakDeviceId(FakeDeviceId)).unwrap()
                )
            );
            assert_eq!(remote_ip.into_inner(), ZonedAddr::Unzoned(client_ip));

            // Double-check that the bound device can't be changed after being set
            // implicitly.
            assert_matches!(
                SocketHandler::set_device(core_ctx, bindings_ctx, &server_connection, None),
                Err(SetDeviceError::ZoneChange)
            );
        });
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &client_connection,
                    Some(
                        ZonedAddr::Zoned(AddrAndZone::new(server_ip, FakeDeviceId).unwrap()).into()
                    ),
                    PORT,
                ),
                Ok(())
            );
        });
    }

    // The test verifies that if client tries to connect to a closed port on
    // server, the connection is aborted and RST is received.
    #[ip_test]
    fn connect_reset<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        let mut net = new_test_net::<I>();

        let client = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let conn =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &conn,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                Some(PORT_1),
            )
            .expect("failed to bind the client socket");
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &conn,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                PORT_1,
            )
            .expect("failed to connect");
            conn
        });

        // Step one time for SYN packet to be delivered.
        let _: StepResult = net.step(handle_frame, handle_timer);
        // Assert that we got a RST back.
        net.collect_frames();
        assert_matches!(
            &net.iter_pending_frames().collect::<Vec<_>>()[..],
            [InstantAndData(_instant, PendingFrameData {
                dst_context: _,
                meta,
                frame,
            })] => {
            let mut buffer = Buf::new(frame, ..);
            match I::VERSION {
                IpVersion::V4 => {
                    let meta = assert_matches!(meta, DualStackSendIpPacketMeta::V4(v4) => v4);
                    let parsed = buffer.parse_with::<_, TcpSegment<_>>(
                        TcpParseArgs::new(*meta.src_ip, *meta.dst_ip)
                    ).expect("failed to parse");
                    assert!(parsed.rst())
                }
                IpVersion::V6 => {
                    let meta = assert_matches!(meta, DualStackSendIpPacketMeta::V6(v6) => v6);
                    let parsed = buffer.parse_with::<_, TcpSegment<_>>(
                        TcpParseArgs::new(*meta.src_ip, *meta.dst_ip)
                    ).expect("failed to parse");
                    assert!(parsed.rst())
                }
            }
        });

        net.run_until_idle(handle_frame, handle_timer);
        // Finally, the connection should be reset and bindings should have been
        // signaled.
        assert_matches!(
        client.get().deref(),
        TcpSocketState::Bound(BoundSocketState::Connected((
            conn, _sharing))) => {
                let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                assert_matches!(
                    conn,
                    Connection {
                    accept_queue: None,
                    state: State::Closed(Closed {
                        reason: Some(ConnectionError::ConnectionReset)
                    }),
                    ip_sock: _,
                    defunct: false,
                    socket_options: _,
                    soft_error: None,
                    handshake_status: HandshakeStatus::Aborted,
                    }
                );
            });
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_matches!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &client,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                    PORT_1
                ),
                Err(ConnectError::Aborted)
            );
        });
    }

    #[ip_test]
    fn retransmission<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        run_with_many_seeds(|seed| {
            let (_net, _client, _client_snd_end, _accepted) = bind_listen_connect_accept_inner::<I>(
                I::UNSPECIFIED_ADDRESS,
                BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false },
                seed,
                0.2,
            );
        });
    }

    const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(1845));

    #[ip_test]
    fn listener_with_bound_device_conflict<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<MultipleDevicesId, TcpBindingsCtx<MultipleDevicesId>>:
            TcpContext<I, TcpBindingsCtx<MultipleDevicesId>>,
    {
        set_logger_for_test();
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new_multiple_devices());

        let sock_a = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        assert_matches!(
            SocketHandler::set_device(
                &mut core_ctx,
                &mut bindings_ctx,
                &sock_a,
                Some(MultipleDevicesId::A),
            ),
            Ok(())
        );
        SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &sock_a, None, Some(LOCAL_PORT))
            .expect("bind should succeed");
        SocketHandler::listen(&mut core_ctx, &sock_a, const_unwrap_option(NonZeroUsize::new(10)))
            .expect("can listen");

        let socket = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        // Binding `socket` to the unspecified address should fail since the address
        // is shadowed by `sock_a`.
        assert_matches!(
            SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, Some(LOCAL_PORT)),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );

        // Once `socket` is bound to a different device, though, it no longer
        // conflicts.
        assert_matches!(
            SocketHandler::set_device(
                &mut core_ctx,
                &mut bindings_ctx,
                &socket,
                Some(MultipleDevicesId::B),
            ),
            Ok(())
        );
        SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, Some(LOCAL_PORT))
            .expect("no conflict");
    }

    #[test_case(None)]
    #[test_case(Some(MultipleDevicesId::B); "other")]
    fn set_bound_device_listener_on_zoned_addr(set_device: Option<MultipleDevicesId>) {
        set_logger_for_test();
        let ll_addr = LinkLocalAddr::new(Ipv6::LINK_LOCAL_UNICAST_SUBNET.network()).unwrap();

        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new(MultipleDevicesId::all().into_iter().map(|device| {
                    FakeDeviceConfig {
                        device,
                        local_ips: vec![ll_addr.into_specified()],
                        remote_ips: vec![ll_addr.into_specified()],
                    }
                })),
                Default::default(),
            ));

        let socket = SocketHandler::<Ipv6, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(
                ZonedAddr::Zoned(
                    AddrAndZone::new(ll_addr.into_specified(), MultipleDevicesId::A).unwrap(),
                )
                .into(),
            ),
            Some(LOCAL_PORT),
        )
        .expect("bind should succeed");

        assert_matches!(
            SocketHandler::set_device(&mut core_ctx, &mut bindings_ctx, &socket, set_device),
            Err(SetDeviceError::ZoneChange)
        );
    }

    #[test_case(None)]
    #[test_case(Some(MultipleDevicesId::B); "other")]
    fn set_bound_device_connected_to_zoned_addr(set_device: Option<MultipleDevicesId>) {
        set_logger_for_test();
        let ll_addr = LinkLocalAddr::new(Ipv6::LINK_LOCAL_UNICAST_SUBNET.network()).unwrap();

        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new(MultipleDevicesId::all().into_iter().map(|device| {
                    FakeDeviceConfig {
                        device,
                        local_ips: vec![ll_addr.into_specified()],
                        remote_ips: vec![ll_addr.into_specified()],
                    }
                })),
                Default::default(),
            ));

        let socket = SocketHandler::<Ipv6, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::connect(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(
                ZonedAddr::Zoned(
                    AddrAndZone::new(ll_addr.into_specified(), MultipleDevicesId::A).unwrap(),
                )
                .into(),
            ),
            LOCAL_PORT,
        )
        .expect("connect should succeed");

        assert_matches!(
            SocketHandler::set_device(&mut core_ctx, &mut bindings_ctx, &socket, set_device),
            Err(SetDeviceError::ZoneChange)
        );
    }

    #[ip_test]
    #[test_case(*<I as TestIpExt>::FAKE_CONFIG.local_ip, true; "specified bound")]
    #[test_case(I::UNSPECIFIED_ADDRESS, true; "unspecified bound")]
    #[test_case(*<I as TestIpExt>::FAKE_CONFIG.local_ip, false; "specified listener")]
    #[test_case(I::UNSPECIFIED_ADDRESS, false; "unspecified listener")]
    fn bound_socket_info<I: Ip + TcpTestIpExt>(ip_addr: I::Addr, listen: bool)
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        let socket = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );

        let (addr, port) =
            (SpecifiedAddr::new(ip_addr).map(|a| ZonedAddr::Unzoned(a).into()), PORT_1);

        SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, addr, Some(port))
            .expect("bind should succeed");
        if listen {
            SocketHandler::listen(
                &mut core_ctx,
                &socket,
                const_unwrap_option(NonZeroUsize::new(25)),
            )
            .expect("can listen");
        }
        let info = SocketHandler::get_info(&mut core_ctx, &socket);
        assert_eq!(
            info,
            SocketInfo::Bound(BoundInfo {
                addr: addr.map(|a| a.into_inner().map_zone(FakeWeakDeviceId).into()),
                port,
                device: None
            })
        );
    }

    #[ip_test]
    fn connection_info<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        let local =
            SocketAddr { ip: ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into(), port: PORT_1 };
        let remote =
            SocketAddr { ip: ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into(), port: PORT_2 };

        let socket = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(local.ip),
            Some(local.port),
        )
        .expect("bind should succeed");

        SocketHandler::connect(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(remote.ip),
            remote.port,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_info(&mut core_ctx, &socket),
            SocketInfo::Connection(ConnectionInfo {
                local_addr: local.map_zone(FakeWeakDeviceId),
                remote_addr: remote.map_zone(FakeWeakDeviceId),
                device: None,
            }),
        );
    }

    #[test_case(true; "any")]
    #[test_case(false; "link local")]
    fn accepted_connection_info_zone(listen_any: bool) {
        set_logger_for_test();
        let client_ip = SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap();
        let server_ip = SpecifiedAddr::new(net_ip_v6!("fe80::2")).unwrap();
        let mut net = FakeNetwork::new(
            [
                (
                    LOCAL,
                    TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(
                        server_ip,
                        client_ip,
                        Ipv6::LINK_LOCAL_UNICAST_SUBNET.prefix(),
                    )),
                ),
                (
                    REMOTE,
                    TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(
                        client_ip,
                        server_ip,
                        Ipv6::LINK_LOCAL_UNICAST_SUBNET.prefix(),
                    )),
                ),
            ],
            move |net, meta: DualStackSendIpPacketMeta<_>| {
                if net == LOCAL {
                    alloc::vec![(REMOTE, meta, None)]
                } else {
                    alloc::vec![(LOCAL, meta, None)]
                }
            },
        );

        let local_server = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<Ipv6, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            let device = FakeDeviceId;
            let bind_addr = match listen_any {
                true => None,
                false => {
                    Some(ZonedAddr::Zoned(AddrAndZone::new(server_ip, device).unwrap()).into())
                }
            };

            SocketHandler::bind(core_ctx, bindings_ctx, &socket, bind_addr, Some(PORT_1))
                .expect("failed to bind the client socket");
            SocketHandler::listen(core_ctx, &socket, const_unwrap_option(NonZeroUsize::new(1)))
                .expect("can listen");
            socket
        });

        let _remote_client = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<Ipv6, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            let device = FakeDeviceId;
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Zoned(AddrAndZone::new(server_ip, device).unwrap()).into()),
                PORT_1,
            )
            .expect("failed to connect");
            socket
        });

        net.run_until_idle(handle_frame, handle_timer);

        let ConnectionInfo { remote_addr, local_addr, device } =
            net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
                let (server_conn, _addr, _buffers) =
                    SocketHandler::accept(core_ctx, bindings_ctx, &local_server)
                        .expect("connection is available");
                assert_matches!(
                    SocketHandler::get_info(core_ctx, &server_conn),
                    SocketInfo::Connection(info) => info
                )
            });

        let device = assert_matches!(device, Some(device) => device);
        assert_eq!(
            local_addr,
            SocketAddr {
                ip: ZonedAddr::Zoned(AddrAndZone::new(server_ip, device).unwrap()).into(),
                port: PORT_1
            }
        );
        let SocketAddr { ip: remote_ip, port: _ } = remote_addr;
        assert_eq!(
            remote_ip,
            ZonedAddr::Zoned(AddrAndZone::new(client_ip, device).unwrap()).into()
        );
    }

    #[test]
    fn bound_connection_info_zoned_addrs() {
        let local_ip = LinkLocalAddr::new(net_ip_v6!("fe80::1")).unwrap().into_specified();
        let remote_ip = LinkLocalAddr::new(net_ip_v6!("fe80::2")).unwrap().into_specified();
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<Ipv6>(
                local_ip,
                remote_ip,
                Ipv6::LINK_LOCAL_UNICAST_SUBNET.prefix(),
            ));

        let local_addr = SocketAddr {
            ip: ZonedAddr::Zoned(AddrAndZone::new(local_ip, FakeDeviceId).unwrap()).into(),
            port: PORT_1,
        };
        let remote_addr = SocketAddr {
            ip: ZonedAddr::Zoned(AddrAndZone::new(remote_ip, FakeDeviceId).unwrap()).into(),
            port: PORT_2,
        };

        let socket = SocketHandler::<Ipv6, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(local_addr.ip),
            Some(local_addr.port),
        )
        .expect("bind should succeed");

        assert_eq!(
            SocketHandler::get_info(&mut core_ctx, &socket),
            SocketInfo::Bound(BoundInfo {
                addr: Some(local_addr.ip.into_inner().map_zone(FakeWeakDeviceId).into()),
                port: local_addr.port,
                device: Some(FakeWeakDeviceId(FakeDeviceId))
            })
        );

        SocketHandler::connect(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(remote_addr.ip),
            remote_addr.port,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_info(&mut core_ctx, &socket),
            SocketInfo::Connection(ConnectionInfo {
                local_addr: local_addr.map_zone(FakeWeakDeviceId),
                remote_addr: remote_addr.map_zone(FakeWeakDeviceId),
                device: Some(FakeWeakDeviceId(FakeDeviceId))
            })
        );
    }

    #[ip_test]
    // Assuming instant delivery of segments:
    // - If peer calls close, then the timeout we need to wait is in
    // TIME_WAIT, which is 2MSL.
    #[test_case(true, 2 * MSL; "peer calls close")]
    // - If not, we will be in the FIN_WAIT2 state and waiting for its
    // timeout.
    #[test_case(false, DEFAULT_FIN_WAIT2_TIMEOUT; "peer doesn't call close")]
    fn connection_close_peer_calls_close<I: Ip + TcpTestIpExt>(
        peer_calls_close: bool,
        expected_time_to_close: Duration,
    ) where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        let (mut net, local, _local_snd_end, remote) = bind_listen_connect_accept_inner::<I>(
            I::UNSPECIFIED_ADDRESS,
            BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false },
            0,
            0.0,
        );

        let weak_local = local.downgrade();
        let close_called = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            SocketHandler::close(core_ctx, bindings_ctx, local);
            bindings_ctx.now()
        });

        while {
            assert!(!net.step(handle_frame, handle_timer).is_idle());
            let is_fin_wait_2 = {
                let local = weak_local.upgrade().unwrap();
                let state = local.get();
                let state = assert_matches!(
                    state.deref(),
                    TcpSocketState::Bound(BoundSocketState::Connected((
                        conn, _sharing))) => {
                    let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                    assert_matches!(
                        conn,
                        Connection {
                            state,
                            ..
                        } => state
                    )
                }
                );
                matches!(state, State::FinWait2(_))
            };
            !is_fin_wait_2
        } {}

        let weak_remote = remote.downgrade();
        if peer_calls_close {
            net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
                SocketHandler::close(core_ctx, bindings_ctx, remote);
            });
        }

        net.run_until_idle(handle_frame, handle_timer);

        net.with_context(LOCAL, |TcpCtx { core_ctx: _, bindings_ctx }| {
            assert_eq!(bindings_ctx.now().duration_since(close_called), expected_time_to_close);
            assert_eq!(weak_local.upgrade(), None);
        });
        if peer_calls_close {
            assert_eq!(weak_remote.upgrade(), None);
        }
    }

    #[ip_test]
    fn connection_shutdown_then_close_peer_doesnt_call_close<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        let (mut net, local, _local_snd_end, _remote) = bind_listen_connect_accept_inner::<I>(
            I::UNSPECIFIED_ADDRESS,
            BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false },
            0,
            0.0,
        );
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::shutdown(
                    core_ctx,
                    bindings_ctx,
                    &local,
                    Shutdown { send: true, receive: false }
                ),
                Ok(true)
            );
        });
        loop {
            assert!(!net.step(handle_frame, handle_timer).is_idle());
            let is_fin_wait_2 = {
                let state = local.get();
                let state = assert_matches!(
                state.deref(),
                TcpSocketState::Bound(BoundSocketState::Connected((
                conn, _sharing))) => {
                let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                assert_matches!(
                    conn,
                    Connection {
                        state, ..
                    } => state
                )});
                matches!(state, State::FinWait2(_))
            };
            if is_fin_wait_2 {
                break;
            }
        }

        let weak_local = local.downgrade();
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            SocketHandler::close(core_ctx, bindings_ctx, local);
        });
        net.run_until_idle(handle_frame, handle_timer);
        assert_eq!(weak_local.upgrade(), None);
    }

    #[ip_test]
    fn connection_shutdown_then_close<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        let (mut net, local, _local_snd_end, remote) = bind_listen_connect_accept_inner::<I>(
            I::UNSPECIFIED_ADDRESS,
            BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false },
            0,
            0.0,
        );

        for (name, id) in [(LOCAL, &local), (REMOTE, &remote)] {
            net.with_context(name, |TcpCtx { core_ctx, bindings_ctx }| {
                assert_eq!(
                    SocketHandler::shutdown(
                        core_ctx,
                        bindings_ctx,
                        id,
                        Shutdown { send: true, receive: false }
                    ),
                    Ok(true)
                );
                assert_matches!(
                    id.get().deref(),
                    TcpSocketState::Bound(BoundSocketState::Connected((conn, _addr))) => {
                    let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                    assert_matches!(
                        conn,
                        Connection {
                            state: State::FinWait1(_),
                            ..
                        }
                    );
                });
                assert_eq!(
                    SocketHandler::shutdown(
                        core_ctx,
                        bindings_ctx,
                        id,
                        Shutdown { send: true, receive: false }
                    ),
                    Ok(true)
                );
            });
        }
        net.run_until_idle(handle_frame, handle_timer);
        for (name, id) in [(LOCAL, local), (REMOTE, remote)] {
            net.with_context(name, |TcpCtx { core_ctx, bindings_ctx }| {
                assert_matches!(
                    id.get().deref(),
                    TcpSocketState::Bound(BoundSocketState::Connected((conn, _sharing))) => {
                    let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                    assert_matches!(
                        conn,
                        Connection {
                            state: State::Closed(_),
                            ..
                        }
                    );
                });
                let weak_id = id.downgrade();
                SocketHandler::close(core_ctx, bindings_ctx, id);
                assert_eq!(weak_id.upgrade(), None)
            });
        }
    }

    #[ip_test]
    fn remove_unbound<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        let unbound = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        let weak_unbound = unbound.downgrade();
        SocketHandler::close(&mut core_ctx, &mut bindings_ctx, unbound);
        assert_eq!(weak_unbound.upgrade(), None);
    }

    #[ip_test]
    fn remove_bound<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        let socket = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
            None,
        )
        .expect("bind should succeed");
        let weak_socket = socket.downgrade();
        SocketHandler::close(&mut core_ctx, &mut bindings_ctx, socket);
        assert_eq!(weak_socket.upgrade(), None);
    }

    #[ip_test]
    fn shutdown_listener<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        let mut net = new_test_net::<I>();
        let local_listener = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                Some(PORT_1),
            )
            .expect("bind should succeed");
            SocketHandler::listen(core_ctx, &socket, NonZeroUsize::new(5).unwrap())
                .expect("can listen");
            socket
        });

        let remote_connection = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                PORT_1,
            )
            .expect("connect should succeed");
            socket
        });

        // After the following step, we should have one established connection
        // in the listener's accept queue, which ought to be aborted during
        // shutdown.
        net.run_until_idle(handle_frame, handle_timer);

        // The incoming connection was signaled, and the remote end was notified
        // of connection establishment.
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &remote_connection,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                    PORT_1
                ),
                Ok(())
            );
        });

        // Create a second half-open connection so that we have one entry in the
        // pending queue.
        let second_connection = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket = SocketHandler::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                PORT_1,
            )
            .expect("connect should succeed");
            socket
        });

        let _: StepResult = net.step(handle_frame, handle_timer);

        // We have a timer scheduled for the pending connection.
        net.with_context(LOCAL, |TcpCtx { core_ctx: _, bindings_ctx }| {
            assert_matches!(bindings_ctx.timers.timers().len(), 1);
        });

        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::shutdown(
                    core_ctx,
                    bindings_ctx,
                    &local_listener,
                    Shutdown { send: false, receive: true },
                ),
                Ok(false)
            );
        });

        // The timer for the pending connection should be cancelled.
        net.with_context(LOCAL, |TcpCtx { core_ctx: _, bindings_ctx }| {
            assert_eq!(bindings_ctx.timers.timers().len(), 0);
        });

        net.run_until_idle(handle_frame, handle_timer);

        // Both remote sockets should now be reset to Closed state.
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx: _ }| {
            for conn in [&remote_connection, &second_connection] {
                assert_eq!(
                    SocketHandler::get_socket_error(core_ctx, conn),
                    Some(ConnectionError::ConnectionReset),
                )
            }

            assert_matches!(
                remote_connection.get().deref(),
                TcpSocketState::Bound(BoundSocketState::Connected((
                    conn, _sharing))) => {
                        let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                        assert_matches!(
                            conn,
                            Connection {
                                state: State::Closed(Closed {
                                    reason: Some(ConnectionError::ConnectionReset)
                                }),
                                ..
                            }
                        );
                    }
            );
        });

        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let new_unbound =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            assert_matches!(
                SocketHandler::bind(
                    core_ctx,
                    bindings_ctx,
                    &new_unbound,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip,).into()),
                    Some(PORT_1),
                ),
                Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
            );
            // Bring the already-shutdown listener back to listener again.
            SocketHandler::listen(core_ctx, &local_listener, NonZeroUsize::new(5).unwrap())
                .expect("can listen again");
        });

        let new_remote_connection =
            net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
                let socket = SocketHandler::<I, _>::create_socket(
                    core_ctx,
                    bindings_ctx,
                    Default::default(),
                );
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &socket,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                    PORT_1,
                )
                .expect("connect should succeed");
                socket
            });

        net.run_until_idle(handle_frame, handle_timer);

        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_matches!(
                new_remote_connection.get().deref(),
                TcpSocketState::Bound(BoundSocketState::Connected((
                    conn, _sharing))) => {
                    let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                    assert_matches!(
                        conn,
                        Connection {
                            state: State::Established(_),
                            ..
                        }
                    );
                    });
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &new_remote_connection,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                    PORT_1,
                ),
                Ok(())
            );
        });
    }

    #[ip_test]
    fn set_buffer_size<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        set_logger_for_test();
        let mut net = new_test_net::<I>();

        let mut local_sizes = BufferSizes { send: 2048, receive: 2000 };
        let mut remote_sizes = BufferSizes { send: 1024, receive: 2000 };

        let local_listener = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let local_listener =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &local_listener,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                Some(PORT_1),
            )
            .expect("bind should succeed");
            SocketHandler::set_send_buffer_size(
                core_ctx,
                bindings_ctx,
                &local_listener,
                local_sizes.send,
            );
            SocketHandler::set_receive_buffer_size(
                core_ctx,
                bindings_ctx,
                &local_listener,
                local_sizes.receive,
            );
            SocketHandler::listen(core_ctx, &local_listener, NonZeroUsize::new(5).unwrap())
                .expect("can listen");
            local_listener
        });

        let remote_connection = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let remote_connection =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::set_send_buffer_size(
                core_ctx,
                bindings_ctx,
                &remote_connection,
                remote_sizes.send,
            );
            SocketHandler::set_receive_buffer_size(
                core_ctx,
                bindings_ctx,
                &remote_connection,
                local_sizes.receive,
            );
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &remote_connection,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                PORT_1,
            )
            .expect("connect should succeed");
            remote_connection
        });
        let mut step_and_increment_buffer_sizes_until_idle =
            |net: &mut TcpTestNetwork,
             local: &TcpSocketId<_, _, _>,
             remote: &TcpSocketId<_, _, _>| loop {
                local_sizes.send += 1;
                local_sizes.receive += 1;
                remote_sizes.send += 1;
                remote_sizes.receive += 1;
                net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
                    SocketHandler::set_send_buffer_size(
                        core_ctx,
                        bindings_ctx,
                        local,
                        local_sizes.send,
                    );
                    if let Some(size) =
                        SocketHandler::send_buffer_size(core_ctx, bindings_ctx, local)
                    {
                        assert_eq!(size, local_sizes.send);
                    }
                    SocketHandler::set_receive_buffer_size(
                        core_ctx,
                        bindings_ctx,
                        local,
                        local_sizes.receive,
                    );
                    if let Some(size) =
                        SocketHandler::receive_buffer_size(core_ctx, bindings_ctx, local)
                    {
                        assert_eq!(size, local_sizes.receive);
                    }
                });
                net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
                    SocketHandler::set_send_buffer_size(
                        core_ctx,
                        bindings_ctx,
                        remote,
                        remote_sizes.send,
                    );
                    if let Some(size) =
                        SocketHandler::send_buffer_size(core_ctx, bindings_ctx, remote)
                    {
                        assert_eq!(size, remote_sizes.send);
                    }
                    SocketHandler::set_receive_buffer_size(
                        core_ctx,
                        bindings_ctx,
                        remote,
                        remote_sizes.receive,
                    );
                    if let Some(size) =
                        SocketHandler::receive_buffer_size(core_ctx, bindings_ctx, remote)
                    {
                        assert_eq!(size, remote_sizes.receive);
                    }
                });
                if net.step(handle_frame, handle_timer).is_idle() {
                    break;
                }
            };

        // Set the send buffer size at each stage of sockets on both ends of the
        // handshake process just to make sure it doesn't break.
        step_and_increment_buffer_sizes_until_idle(&mut net, &local_listener, &remote_connection);

        let local_connection = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let (conn, _, _) = SocketHandler::accept(core_ctx, bindings_ctx, &local_listener)
                .expect("received connection");
            conn
        });

        step_and_increment_buffer_sizes_until_idle(&mut net, &local_connection, &remote_connection);

        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::shutdown(
                    core_ctx,
                    bindings_ctx,
                    &local_connection,
                    Shutdown { send: true, receive: false },
                ),
                Ok(true)
            );
        });

        step_and_increment_buffer_sizes_until_idle(&mut net, &local_connection, &remote_connection);

        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::shutdown(
                    core_ctx,
                    bindings_ctx,
                    &remote_connection,
                    Shutdown { send: true, receive: false },
                ),
                Ok(true),
            );
        });

        step_and_increment_buffer_sizes_until_idle(&mut net, &local_connection, &remote_connection);
    }

    #[ip_test]
    fn set_reuseaddr_unbound<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));

        let first_bound = {
            let socket = SocketHandler::<I, _>::create_socket(
                &mut core_ctx,
                &mut bindings_ctx,
                Default::default(),
            );
            SocketHandler::set_reuseaddr(&mut core_ctx, &socket, true).expect("can set");
            SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, None)
                .expect("bind succeeds");
            socket
        };
        let _second_bound = {
            let socket = SocketHandler::<I, _>::create_socket(
                &mut core_ctx,
                &mut bindings_ctx,
                Default::default(),
            );
            SocketHandler::set_reuseaddr(&mut core_ctx, &socket, true).expect("can set");
            SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, None)
                .expect("bind succeeds");
            socket
        };

        SocketHandler::listen(
            &mut core_ctx,
            &first_bound,
            const_unwrap_option(NonZeroUsize::new(10)),
        )
        .expect("can listen");
    }

    #[ip_test]
    #[test_case([true, true], Ok(()); "allowed with set")]
    #[test_case([false, true], Err(LocalAddressError::AddressInUse); "first unset")]
    #[test_case([true, false], Err(LocalAddressError::AddressInUse); "second unset")]
    #[test_case([false, false], Err(LocalAddressError::AddressInUse); "both unset")]
    fn reuseaddr_multiple_bound<I: Ip + TcpTestIpExt>(
        set_reuseaddr: [bool; 2],
        expected: Result<(), LocalAddressError>,
    ) where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));

        let first = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::set_reuseaddr(&mut core_ctx, &first, set_reuseaddr[0]).expect("can set");
        SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &first, None, Some(PORT_1))
            .expect("bind succeeds");

        let second = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::set_reuseaddr(&mut core_ctx, &second, set_reuseaddr[1]).expect("can set");
        let second_bind_result =
            SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &second, None, Some(PORT_1));

        assert_eq!(second_bind_result, expected.map_err(From::from));
    }

    #[ip_test]
    fn toggle_reuseaddr_bound_different_addrs<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let addrs = [1, 2].map(|i| I::get_other_ip_address(i));
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::<_>::with_devices_state(core::iter::once::<(
                    _,
                    _,
                    Vec<SpecifiedAddr<IpAddr>>,
                )>((
                    FakeDeviceId,
                    I::new_device_state(
                        addrs.iter().map(Witness::get),
                        I::FAKE_CONFIG.subnet.prefix(),
                    ),
                    vec![],
                ))),
                FakeDualStackTcpState::default(),
            ));

        let first = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &first,
            Some(ZonedAddr::Unzoned(addrs[0]).into()),
            Some(PORT_1),
        )
        .unwrap();

        let second = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &second,
            Some(ZonedAddr::Unzoned(addrs[1]).into()),
            Some(PORT_1),
        )
        .unwrap();
        // Setting and un-setting ReuseAddr should be fine since these sockets
        // don't conflict.
        SocketHandler::set_reuseaddr(&mut core_ctx, &first, true).expect("can set");
        SocketHandler::set_reuseaddr(&mut core_ctx, &first, false).expect("can un-set");
    }

    #[ip_test]
    fn unset_reuseaddr_bound_unspecified_specified<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));
        let first = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::set_reuseaddr(&mut core_ctx, &first, true).expect("can set");
        SocketHandler::bind(
            &mut core_ctx,
            &mut bindings_ctx,
            &first,
            Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
            Some(PORT_1),
        )
        .unwrap();

        let second = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::set_reuseaddr(&mut core_ctx, &second, true).expect("can set");
        SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &second, None, Some(PORT_1)).unwrap();

        // Both sockets can be bound because they have ReuseAddr set. Since
        // removing it would introduce inconsistent state, that's not allowed.
        assert_matches!(
            SocketHandler::set_reuseaddr(&mut core_ctx, &first, false),
            Err(SetReuseAddrError::AddrInUse)
        );
        assert_matches!(
            SocketHandler::set_reuseaddr(&mut core_ctx, &second, false),
            Err(SetReuseAddrError::AddrInUse)
        );
    }

    #[ip_test]
    fn reuseaddr_allows_binding_under_connection<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        set_logger_for_test();
        let mut net = new_test_net::<I>();

        let server = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let server =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::set_reuseaddr(core_ctx, &server, true).expect("can set");
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &server,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                Some(PORT_1),
            )
            .expect("failed to bind the client socket");
            SocketHandler::listen(core_ctx, &server, const_unwrap_option(NonZeroUsize::new(10)))
                .expect("can listen");
            server
        });

        let client = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let client =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &client,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                PORT_1,
            )
            .expect("connect should succeed");
            client
        });
        // Finish the connection establishment.
        net.run_until_idle(handle_frame, handle_timer);
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &client,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                    PORT_1
                ),
                Ok(())
            );
        });

        // Now accept the connection and close the listening socket. Then
        // binding a new socket on the same local address should fail unless the
        // socket has SO_REUSEADDR set.
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let (_server_conn, _, _): (_, SocketAddr<_, _>, ClientBuffers) =
                SocketHandler::accept(core_ctx, bindings_ctx, &server).expect("pending connection");

            assert_eq!(
                SocketHandler::shutdown(
                    core_ctx,
                    bindings_ctx,
                    &server,
                    Shutdown { send: false, receive: true },
                ),
                Ok(false)
            );
            SocketHandler::close(core_ctx, bindings_ctx, server);

            let unbound = SocketHandler::create_socket(core_ctx, bindings_ctx, Default::default());
            assert_eq!(
                SocketHandler::bind(core_ctx, bindings_ctx, &unbound, None, Some(PORT_1)),
                Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
            );

            // Binding should succeed after setting ReuseAddr.
            SocketHandler::set_reuseaddr(core_ctx, &unbound, true).expect("can set");
            SocketHandler::bind(core_ctx, bindings_ctx, &unbound, None, Some(PORT_1))
                .expect("bind succeeds");
        });
    }

    #[ip_test]
    #[test_case([true, true]; "specified specified")]
    #[test_case([false, true]; "any specified")]
    #[test_case([true, false]; "specified any")]
    #[test_case([false, false]; "any any")]
    fn set_reuseaddr_bound_allows_other_bound<I: Ip + TcpTestIpExt>(bind_specified: [bool; 2])
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));

        let [first_addr, second_addr] = bind_specified
            .map(|b| b.then_some(I::FAKE_CONFIG.local_ip).map(|a| ZonedAddr::Unzoned(a).into()));
        let first_bound = {
            let socket = SocketHandler::<I, _>::create_socket(
                &mut core_ctx,
                &mut bindings_ctx,
                Default::default(),
            );
            SocketHandler::bind(
                &mut core_ctx,
                &mut bindings_ctx,
                &socket,
                first_addr,
                Some(PORT_1),
            )
            .expect("bind succeeds");
            socket
        };

        let second = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );

        // Binding the second socket will fail because the first doesn't have
        // SO_REUSEADDR set.
        assert_matches!(
            SocketHandler::bind(
                &mut core_ctx,
                &mut bindings_ctx,
                &second,
                second_addr,
                Some(PORT_1)
            ),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );

        // Setting SO_REUSEADDR for the second socket isn't enough.
        SocketHandler::set_reuseaddr(&mut core_ctx, &second, true).expect("can set");
        assert_matches!(
            SocketHandler::bind(
                &mut core_ctx,
                &mut bindings_ctx,
                &second,
                second_addr,
                Some(PORT_1)
            ),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );

        // Setting SO_REUSEADDR for the first socket lets the second bind.
        SocketHandler::set_reuseaddr(&mut core_ctx, &first_bound, true).expect("only socket");
        SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &second, second_addr, Some(PORT_1))
            .expect("can bind");
    }

    #[ip_test]
    fn clear_reuseaddr_listener<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));

        let bound = {
            let socket = SocketHandler::<I, _>::create_socket(
                &mut core_ctx,
                &mut bindings_ctx,
                Default::default(),
            );
            SocketHandler::set_reuseaddr(&mut core_ctx, &socket, true).expect("can set");
            SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, Some(PORT_1))
                .expect("bind succeeds");
            socket
        };

        let listener = {
            let socket = SocketHandler::<I, _>::create_socket(
                &mut core_ctx,
                &mut bindings_ctx,
                Default::default(),
            );
            SocketHandler::set_reuseaddr(&mut core_ctx, &socket, true).expect("can set");

            SocketHandler::bind(&mut core_ctx, &mut bindings_ctx, &socket, None, Some(PORT_1))
                .expect("bind succeeds");
            SocketHandler::listen(
                &mut core_ctx,
                &socket,
                const_unwrap_option(NonZeroUsize::new(5)),
            )
            .expect("can listen");
            socket
        };

        // We can't clear SO_REUSEADDR on the listener because it's sharing with
        // the bound socket.
        assert_matches!(
            SocketHandler::set_reuseaddr(&mut core_ctx, &listener, false),
            Err(SetReuseAddrError::AddrInUse)
        );

        // We can, however, connect to the listener with the bound socket. Then
        // the unencumbered listener can clear SO_REUSEADDR.
        SocketHandler::connect(
            &mut core_ctx,
            &mut bindings_ctx,
            &bound,
            Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
            PORT_1,
        )
        .expect("can connect");
        SocketHandler::set_reuseaddr(&mut core_ctx, &listener, false).expect("can unset")
    }

    fn deliver_icmp_error<
        I: TcpTestIpExt + IcmpIpExt,
        CC: TcpContext<I, BC, DeviceId = FakeDeviceId>
            + TcpContext<I::OtherVersion, BC, DeviceId = FakeDeviceId>,
        BC: TcpBindingsContext<CC::WeakDeviceId>,
    >(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        original_src_ip: SpecifiedAddr<I::Addr>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        original_body: &[u8],
        err: I::ErrorCode,
    ) {
        <TcpIpTransportContext as IpTransportContext<I, _, _>>::receive_icmp_error(
            core_ctx,
            bindings_ctx,
            &FakeDeviceId,
            Some(original_src_ip),
            original_dst_ip,
            original_body,
            err,
        );
    }

    #[test_case(Icmpv4DestUnreachableCode::DestNetworkUnreachable => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestHostUnreachable => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestProtocolUnreachable => ConnectionError::ProtocolUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestPortUnreachable => ConnectionError::PortUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::SourceRouteFailed => ConnectionError::SourceRouteFailed)]
    #[test_case(Icmpv4DestUnreachableCode::DestNetworkUnknown => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestHostUnknown => ConnectionError::DestinationHostDown)]
    #[test_case(Icmpv4DestUnreachableCode::SourceHostIsolated => ConnectionError::SourceHostIsolated)]
    #[test_case(Icmpv4DestUnreachableCode::NetworkAdministrativelyProhibited => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::HostAdministrativelyProhibited => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::NetworkUnreachableForToS => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::HostUnreachableForToS => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::CommAdministrativelyProhibited => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::HostPrecedenceViolation => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::PrecedenceCutoffInEffect => ConnectionError::HostUnreachable)]
    fn icmp_destination_unreachable_connect_v4(
        error: Icmpv4DestUnreachableCode,
    ) -> ConnectionError {
        icmp_destination_unreachable_connect_inner::<Ipv4>(Icmpv4ErrorCode::DestUnreachable(error))
    }

    #[test_case(Icmpv6DestUnreachableCode::NoRoute => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::CommAdministrativelyProhibited => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::BeyondScope => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::AddrUnreachable => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::PortUnreachable => ConnectionError::PortUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::SrcAddrFailedPolicy => ConnectionError::SourceRouteFailed)]
    #[test_case(Icmpv6DestUnreachableCode::RejectRoute => ConnectionError::NetworkUnreachable)]
    fn icmp_destination_unreachable_connect_v6(
        error: Icmpv6DestUnreachableCode,
    ) -> ConnectionError {
        icmp_destination_unreachable_connect_inner::<Ipv6>(Icmpv6ErrorCode::DestUnreachable(error))
    }

    fn icmp_destination_unreachable_connect_inner<I: TcpTestIpExt + IcmpIpExt>(
        icmp_error: I::ErrorCode,
    ) -> ConnectionError
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<I, TcpBindingsCtx<FakeDeviceId>>
            + TcpContext<I::OtherVersion, TcpBindingsCtx<FakeDeviceId>>,
    {
        let TcpCtx { mut core_ctx, mut bindings_ctx } =
            TcpCtx::with_sync_ctx(TcpCoreCtx::new::<I>(
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.subnet.prefix(),
            ));

        let connection = SocketHandler::<I, _>::create_socket(
            &mut core_ctx,
            &mut bindings_ctx,
            Default::default(),
        );
        SocketHandler::connect(
            &mut core_ctx,
            &mut bindings_ctx,
            &connection,
            Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
            PORT_1,
        )
        .expect("failed to create a connection socket");
        let frames = core_ctx.inner.take_frames();
        let frame = assert_matches!(&frames[..], [(_meta, frame)] => frame);
        deliver_icmp_error::<I, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            &frame[0..8],
            icmp_error,
        );
        // The TCP handshake should be aborted.
        assert_eq!(
            SocketHandler::connect(
                &mut core_ctx,
                &mut bindings_ctx,
                &connection,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                PORT_1
            ),
            Err(ConnectError::Aborted)
        );
        SocketHandler::get_socket_error(&mut core_ctx, &connection).unwrap()
    }

    #[test_case(Icmpv4DestUnreachableCode::DestNetworkUnreachable => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestHostUnreachable => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestProtocolUnreachable => ConnectionError::ProtocolUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestPortUnreachable => ConnectionError::PortUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::SourceRouteFailed => ConnectionError::SourceRouteFailed)]
    #[test_case(Icmpv4DestUnreachableCode::DestNetworkUnknown => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::DestHostUnknown => ConnectionError::DestinationHostDown)]
    #[test_case(Icmpv4DestUnreachableCode::SourceHostIsolated => ConnectionError::SourceHostIsolated)]
    #[test_case(Icmpv4DestUnreachableCode::NetworkAdministrativelyProhibited => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::HostAdministrativelyProhibited => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::NetworkUnreachableForToS => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::HostUnreachableForToS => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::CommAdministrativelyProhibited => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::HostPrecedenceViolation => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv4DestUnreachableCode::PrecedenceCutoffInEffect => ConnectionError::HostUnreachable)]
    fn icmp_destination_unreachable_established_v4(
        error: Icmpv4DestUnreachableCode,
    ) -> ConnectionError {
        icmp_destination_unreachable_established_inner::<Ipv4>(Icmpv4ErrorCode::DestUnreachable(
            error,
        ))
    }

    #[test_case(Icmpv6DestUnreachableCode::NoRoute => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::CommAdministrativelyProhibited => ConnectionError::HostUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::BeyondScope => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::AddrUnreachable => ConnectionError::NetworkUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::PortUnreachable => ConnectionError::PortUnreachable)]
    #[test_case(Icmpv6DestUnreachableCode::SrcAddrFailedPolicy => ConnectionError::SourceRouteFailed)]
    #[test_case(Icmpv6DestUnreachableCode::RejectRoute => ConnectionError::NetworkUnreachable)]
    fn icmp_destination_unreachable_established_v6(
        error: Icmpv6DestUnreachableCode,
    ) -> ConnectionError {
        icmp_destination_unreachable_established_inner::<Ipv6>(Icmpv6ErrorCode::DestUnreachable(
            error,
        ))
    }

    fn icmp_destination_unreachable_established_inner<I: TcpTestIpExt + IcmpIpExt>(
        icmp_error: I::ErrorCode,
    ) -> ConnectionError
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
                I,
                TcpBindingsCtx<FakeDeviceId>,
                SingleStackConverter = I::SingleStackConverter,
                DualStackConverter = I::DualStackConverter,
            > + TcpContext<I::OtherVersion, TcpBindingsCtx<FakeDeviceId>>,
    {
        let (mut net, local, local_snd_end, _remote) = bind_listen_connect_accept_inner::<I>(
            I::UNSPECIFIED_ADDRESS,
            BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false },
            0,
            0.0,
        );
        local_snd_end.lock().extend_from_slice(b"Hello");
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            SocketHandler::do_send(core_ctx, bindings_ctx, &local);
        });
        net.collect_frames();
        let original_body = assert_matches!(
            &net.iter_pending_frames().collect::<Vec<_>>()[..],
            [InstantAndData(_instant, PendingFrameData {
                dst_context: _,
                meta: _,
                frame,
            })] => {
            frame.clone()
        });
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            deliver_icmp_error::<I, _, _>(
                core_ctx,
                bindings_ctx,
                I::FAKE_CONFIG.local_ip,
                I::FAKE_CONFIG.remote_ip,
                &original_body[..],
                icmp_error,
            );
            // An error should be posted on the connection.
            let error = assert_matches!(
                SocketHandler::get_socket_error(core_ctx, &local),
                Some(error) => error
            );
            // But it should stay established.
            assert_matches!(
                local.get().deref(),
                TcpSocketState::Bound(BoundSocketState::Connected((
                    conn, _sharing))) => {
                    let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                    assert_matches!(
                        conn,
                        Connection {
                            state: State::Established(_),
                            ..
                        }
                    );
                }
            );
            error
        })
    }

    #[ip_test]
    fn icmp_destination_unreachable_listener<I: Ip + TcpTestIpExt + IcmpIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<I, TcpBindingsCtx<FakeDeviceId>>
            + TcpContext<I::OtherVersion, TcpBindingsCtx<FakeDeviceId>>,
    {
        let mut net = new_test_net::<I>();

        let backlog = NonZeroUsize::new(1).unwrap();
        let server = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let server =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(core_ctx, bindings_ctx, &server, None, Some(PORT_1))
                .expect("failed to bind the server socket");
            SocketHandler::listen(core_ctx, &server, backlog).expect("can listen");
            server
        });

        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let conn =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &conn,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                PORT_1,
            )
            .expect("failed to connect");
        });

        assert!(!net.step(handle_frame, handle_timer).is_idle());

        net.collect_frames();
        let original_body = assert_matches!(
            &net.iter_pending_frames().collect::<Vec<_>>()[..],
            [InstantAndData(_instant, PendingFrameData {
                dst_context: _,
                meta: _,
                frame,
            })] => {
            frame.clone()
        });
        let icmp_error = I::map_ip(
            (),
            |()| Icmpv4ErrorCode::DestUnreachable(Icmpv4DestUnreachableCode::DestPortUnreachable),
            |()| Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::PortUnreachable),
        );
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let in_queue = {
                let state = server.get();
                let accept_queue = assert_matches!(
                    state.deref(),
                    TcpSocketState::Bound(BoundSocketState::Listener((
                        MaybeListener::Listener(Listener { accept_queue, .. }),
                        ..
                    ))) => accept_queue
                );
                assert_eq!(accept_queue.len(), 1);
                accept_queue.collect_pending().first().unwrap().downgrade()
            };
            deliver_icmp_error::<I, _, _>(
                core_ctx,
                bindings_ctx,
                I::FAKE_CONFIG.remote_ip,
                I::FAKE_CONFIG.local_ip,
                &original_body[..],
                icmp_error,
            );
            {
                let state = server.get();
                let queue_len = assert_matches!(
                    state.deref(),
                    TcpSocketState::Bound(BoundSocketState::Listener((
                        MaybeListener::Listener(Listener { accept_queue, .. }),
                        ..
                    ))) => accept_queue.len()
                );
                assert_eq!(queue_len, 0);
            }
            // Socket must've been destroyed.
            assert_eq!(in_queue.upgrade(), None);
        });
    }

    #[ip_test]
    fn time_wait_reuse<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        const CLIENT_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(2));
        const SERVER_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(1));
        let (mut net, local, _local_snd_end, remote) = bind_listen_connect_accept_inner::<I>(
            I::UNSPECIFIED_ADDRESS,
            BindConfig {
                client_port: Some(CLIENT_PORT),
                server_port: SERVER_PORT,
                client_reuse_addr: true,
            },
            0,
            0.0,
        );
        // Locally, we create a connection with a full accept queue.
        let listener = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let listener =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::set_reuseaddr(core_ctx, &listener, true).expect("can set");
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &listener,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                Some(CLIENT_PORT),
            )
            .expect("failed to bind");
            SocketHandler::listen(core_ctx, &listener, NonZeroUsize::new(1).unwrap())
                .expect("failed to listen");
            listener
        });
        // This connection is never used, just to keep accept queue full.
        let extra_conn = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let extra_conn =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &extra_conn,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                CLIENT_PORT,
            )
            .expect("failed to connect");
            extra_conn
        });
        net.run_until_idle(handle_frame, handle_timer);

        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &extra_conn,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                    CLIENT_PORT,
                ),
                Ok(())
            );
        });

        // Now we shutdown the sockets and try to bring the local socket to
        // TIME-WAIT.
        let weak_local = local.downgrade();
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            SocketHandler::close(core_ctx, bindings_ctx, local);
        });
        assert!(!net.step(handle_frame, handle_timer).is_idle());
        assert!(!net.step(handle_frame, handle_timer).is_idle());
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            SocketHandler::close(core_ctx, bindings_ctx, remote);
        });
        assert!(!net.step(handle_frame, handle_timer).is_idle());
        assert!(!net.step(handle_frame, handle_timer).is_idle());
        // The connection should go to TIME-WAIT.
        let (tw_last_seq, tw_last_ack, tw_expiry) = {
            assert_matches!(
                weak_local.upgrade().unwrap().get().deref(),
                TcpSocketState::Bound(BoundSocketState::Connected((
                    conn, _sharing))) => {
                    let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                    assert_matches!(
                        conn,
                        Connection {
                        state: State::TimeWait(TimeWait {
                            last_seq,
                            last_ack,
                            expiry,
                            ..
                        }), ..
                        } => (*last_seq, *last_ack, *expiry)
                    )
                }
            )
        };

        // Try to initiate a connection from the remote since we have an active
        // listener locally.
        let conn = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let conn =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &conn,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                CLIENT_PORT,
            )
            .expect("failed to connect");
            conn
        });
        while net.next_step() != Some(tw_expiry) {
            assert!(!net.step(handle_frame, handle_timer).is_idle());
        }
        // This attempt should fail due the full accept queue at the listener.
        assert_matches!(
        conn.get().deref(),
        TcpSocketState::Bound(BoundSocketState::Connected((
            conn, _sharing))) => {
                let (conn, _addr) = assert_this_stack_conn::<I, _, TcpCoreCtx<_, _>>(conn, &I::converter());
                assert_matches!(
                    conn,
                Connection {
                    state: State::Closed(Closed { reason: Some(ConnectionError::TimedOut) }),
                    ..
                }
                );
            });

        // Now free up the accept queue by accepting the connection.
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let _accepted = SocketHandler::accept(core_ctx, bindings_ctx, &listener)
                .expect("failed to accept a new connection");
        });
        let conn = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                Some(SERVER_PORT),
            )
            .expect("failed to bind");
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                CLIENT_PORT,
            )
            .expect("failed to connect");
            socket
        });
        net.collect_frames();
        assert_matches!(
            &net.iter_pending_frames().collect::<Vec<_>>()[..],
            [InstantAndData(_instant, PendingFrameData {
                dst_context: _,
                meta,
                frame,
            })] => {
            let mut buffer = Buf::new(frame, ..);
            let iss = match I::VERSION {
                IpVersion::V4 => {
                    let meta = assert_matches!(meta, DualStackSendIpPacketMeta::V4(meta) => meta);
                    let parsed = buffer.parse_with::<_, TcpSegment<_>>(
                        TcpParseArgs::new(*meta.src_ip, *meta.dst_ip)
                    ).expect("failed to parse");
                    assert!(parsed.syn());
                    SeqNum::new(parsed.seq_num())
                }
                IpVersion::V6 => {
                    let meta = assert_matches!(meta, DualStackSendIpPacketMeta::V6(meta) => meta);
                    let parsed = buffer.parse_with::<_, TcpSegment<_>>(
                        TcpParseArgs::new(*meta.src_ip, *meta.dst_ip)
                    ).expect("failed to parse");
                    assert!(parsed.syn());
                    SeqNum::new(parsed.seq_num())
                }
            };
            assert!(iss.after(tw_last_ack) && iss.before(tw_last_seq));
        });
        // The TIME-WAIT socket should be reused to establish the connection.
        net.run_until_idle(handle_frame, handle_timer);
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &conn,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                    CLIENT_PORT
                ),
                Ok(())
            );
        });
    }

    #[ip_test]
    fn conn_addr_not_available<I: Ip + TcpTestIpExt + IcmpIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        set_logger_for_test();
        let (mut net, _local, _local_snd_end, _remote) = bind_listen_connect_accept_inner::<I>(
            I::UNSPECIFIED_ADDRESS,
            BindConfig { client_port: Some(PORT_1), server_port: PORT_1, client_reuse_addr: true },
            0,
            0.0,
        );
        // Now we are using the same 4-tuple again to try to create a new
        // connection, this should fail.
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket =
                SocketHandler::<I, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::set_reuseaddr(core_ctx, &socket, true).expect("can set");
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip).into()),
                Some(PORT_1),
            )
            .expect("failed to bind");
            assert_eq!(
                SocketHandler::connect(
                    core_ctx,
                    bindings_ctx,
                    &socket,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                    PORT_1
                ),
                Err(ConnectError::ConnectionExists),
            )
        });
    }

    #[test]
    fn dual_stack_connect() {
        set_logger_for_test();
        let mut net = new_test_net::<Ipv4>();
        let backlog = NonZeroUsize::new(1).unwrap();
        let server = net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            let server =
                SocketHandler::<Ipv4, _>::create_socket(core_ctx, bindings_ctx, Default::default());
            SocketHandler::bind(
                core_ctx,
                bindings_ctx,
                &server,
                Some(ZonedAddr::Unzoned(Ipv4::FAKE_CONFIG.remote_ip).into()),
                Some(PORT_1),
            )
            .expect("failed to bind the server socket");
            SocketHandler::listen(core_ctx, &server, backlog).expect("can listen");
            server
        });

        let client_ends = WriteBackClientBuffers::default();
        let client = net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            let socket = SocketHandler::<Ipv6, _>::create_socket(
                core_ctx,
                bindings_ctx,
                ProvidedBuffers::Buffers(client_ends.clone()),
            );
            SocketHandler::connect(
                core_ctx,
                bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned((*Ipv4::FAKE_CONFIG.remote_ip).to_ipv6_mapped()).into()),
                PORT_1,
            )
            .expect("failed to connect");
            socket
        });

        // Step the test network until the handshake is done.
        net.run_until_idle(&mut handle_frame, handle_timer);
        let (accepted, addr, accepted_ends) =
            net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
                SocketHandler::accept(core_ctx, bindings_ctx, &server).expect("failed to accept")
            });
        assert_eq!(addr.ip, ZonedAddr::Unzoned(Ipv4::FAKE_CONFIG.local_ip).into());

        let ClientBuffers { send: client_snd_end, receive: client_rcv_end } =
            client_ends.0.as_ref().lock().take().unwrap();
        let ClientBuffers { send: accepted_snd_end, receive: accepted_rcv_end } = accepted_ends;
        for snd_end in [client_snd_end, accepted_snd_end] {
            snd_end.lock().extend_from_slice(b"Hello");
        }
        net.with_context(LOCAL, |TcpCtx { core_ctx, bindings_ctx }| {
            SocketHandler::<Ipv6, _>::do_send(core_ctx, bindings_ctx, &client)
        });
        net.with_context(REMOTE, |TcpCtx { core_ctx, bindings_ctx }| {
            SocketHandler::<Ipv4, _>::do_send(core_ctx, bindings_ctx, &accepted)
        });
        net.run_until_idle(&mut handle_frame, handle_timer);

        for rcv_end in [client_rcv_end, accepted_rcv_end] {
            assert_eq!(
                rcv_end.lock().read_with(|avail| {
                    let avail = avail.concat();
                    assert_eq!(avail, b"Hello");
                    avail.len()
                }),
                5
            );
        }

        // Verify that the client is connected to the IPv4 remote and has been
        // assigned an IPv4 local IP.
        let info = assert_matches!(
            SocketHandler::get_info(net.sync_ctx(LOCAL), &client),
            SocketInfo::Connection(info) => info
        );
        let (local_ip, remote_ip) = assert_matches!(
            info,
            ConnectionInfo {
                local_addr: SocketAddr { ip: local_ip, port: _ },
                remote_addr: SocketAddr { ip: remote_ip, port: PORT_1 },
                device: _
            } => (local_ip.addr(), remote_ip.addr())
        );
        assert_eq!(remote_ip, Ipv4::FAKE_CONFIG.remote_ip.to_ipv6_mapped());
        assert_matches!(local_ip.to_ipv4_mapped(), Some(_));
    }
}
