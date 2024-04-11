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

use assert_matches::assert_matches;
use derivative::Derivative;
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddr, IpAddress, IpInvariant, IpVersion, IpVersionMarker, Ipv4,
        Ipv4Addr, Ipv6, Ipv6Addr,
    },
    AddrAndPortFormatter, AddrAndZone, SpecifiedAddr, ZonedAddr,
};
use packet_formats::ip::IpProto;
use smallvec::{smallvec, SmallVec};
use thiserror::Error;
use tracing::{debug, error, trace};

use crate::{
    algorithm::{self, PortAllocImpl},
    context::{
        ContextPair, CoreTimerContext, CounterContext, CtxPair, InstantBindingsTypes, RngContext,
        TimerBindingsTypes, TimerContext2, TimerHandler, TracingContext,
    },
    convert::{BidirectionalConverter as _, OwnedOrRefsBidirectionalConverter},
    data_structures::socketmap::{IterShadows as _, SocketMap},
    device::{self, AnyDevice, DeviceIdContext, WeakId},
    error::{ExistsError, LocalAddressError, ZonedAddressError},
    inspect::{Inspector, InspectorDeviceExt},
    ip::{
        icmp::IcmpErrorCode,
        socket::{
            DefaultSendOptions, DeviceIpSocketHandler, IpSock, IpSockCreationError,
            IpSocketHandler, SendOptions,
        },
        EitherDeviceId, IpExt, IpSockCreateAndSendError, TransportIpContext,
    },
    socket::{
        self,
        address::{
            AddrIsMappedError, ConnAddr, ConnIpAddr, DualStackListenerIpAddr, DualStackRemoteIp,
            ListenerAddr, ListenerIpAddr, SocketIpAddr, TryUnmapResult,
        },
        AddrVec, Bound, EitherStack, IncompatibleError, InsertError, Inserter, ListenerAddrInfo,
        MaybeDualStack, NotDualStackCapableError, RemoveResult, SetDualStackEnabledError,
        ShutdownType, SocketMapAddrSpec, SocketMapAddrStateSpec,
        SocketMapAddrStateUpdateSharingSpec, SocketMapConflictPolicy, SocketMapStateSpec,
        SocketMapUpdateSharingPolicy, UpdateSharingError,
    },
    sync::RwLock,
    transport::tcp::{
        buffer::{IntoBuffers, ReceiveBuffer, SendBuffer, SendPayload},
        segment::Segment,
        seqnum::SeqNum,
        socket::{accept_queue::AcceptQueue, demux::tcp_serialize_segment, isn::IsnGenerator},
        state::{CloseError, CloseReason, Closed, Initial, State, Takeable},
        BufferSizes, ConnectionError, Control, Mss, OptionalBufferSizes, SocketOptions,
        TcpCounters,
    },
};

pub use accept_queue::ListenerNotifier;

/// A dual stack IP extension trait for TCP.
pub trait DualStackIpExt: crate::socket::DualStackIpExt {
    /// For `Ipv4`, this is [`EitherStack<TcpSocketId<Ipv4, _, _>, TcpSocketId<Ipv6, _, _>>`],
    /// and for `Ipv6` it is just `TcpSocketId<Ipv6>`.
    type DemuxSocketId<D: device::WeakId, BT: TcpBindingsTypes>: SpecSocketId;

    /// The type for a connection, for [`Ipv4`], this will be just the single
    /// stack version of the connection state and the connection address. For
    /// [`Ipv6`], this will be a `EitherStack`.
    type ConnectionAndAddr<D: device::WeakId, BT: TcpBindingsTypes>: Send + Sync + Debug;

    /// The type for the address that the listener is listening on. This should
    /// be just [`ListenerIpAddr`] for [`Ipv4`], but a [`DualStackListenerIpAddr`]
    /// for [`Ipv6`].
    type ListenerIpAddr: Send + Sync + Debug + Clone;

    /// IP options unique to a particular IP version.
    type DualStackIpOptions: Send + Sync + Debug + Default + Clone + Copy;

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

    fn get_conn_info<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &Self::ConnectionAndAddr<D, BT>,
    ) -> ConnectionInfo<Self::Addr, D>;
    fn get_accept_queue_mut<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &mut Self::ConnectionAndAddr<D, BT>,
    ) -> &mut Option<
        AcceptQueue<
            TcpSocketId<Self, D, BT>,
            BT::ReturnedBuffers,
            BT::ListenerNotifierOrProvidedBuffers,
        >,
    >;
    fn get_defunct<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &Self::ConnectionAndAddr<D, BT>,
    ) -> bool;
    fn get_state<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &Self::ConnectionAndAddr<D, BT>,
    ) -> &State<BT::Instant, BT::ReceiveBuffer, BT::SendBuffer, BT::ListenerNotifierOrProvidedBuffers>;
    fn get_bound_info<D: device::WeakId>(
        listener_addr: &ListenerAddr<Self::ListenerIpAddr, D>,
    ) -> BoundInfo<Self::Addr, D>;

    fn destroy_socket_with_demux_id<
        CC: TcpContext<Self, BC> + TcpContext<Self::OtherVersion, BC>,
        BC: TcpBindingsContext<Self, CC::WeakDeviceId>
            + TcpBindingsContext<Self::OtherVersion, CC::WeakDeviceId>,
    >(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        demux_id: Self::DemuxSocketId<CC::WeakDeviceId, BC>,
    );
}

impl DualStackIpExt for Ipv4 {
    type DemuxSocketId<D: device::WeakId, BT: TcpBindingsTypes> =
        EitherStack<TcpSocketId<Ipv4, D, BT>, TcpSocketId<Ipv6, D, BT>>;
    type ConnectionAndAddr<D: device::WeakId, BT: TcpBindingsTypes> =
        (Connection<Ipv4, Ipv4, D, BT>, ConnAddr<ConnIpAddr<Ipv4Addr, NonZeroU16, NonZeroU16>, D>);
    type ListenerIpAddr = ListenerIpAddr<Ipv4Addr, NonZeroU16>;
    type DualStackIpOptions = ();

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
    fn get_conn_info<D: device::WeakId, BT: TcpBindingsTypes>(
        (_conn, addr): &Self::ConnectionAndAddr<D, BT>,
    ) -> ConnectionInfo<Self::Addr, D> {
        addr.clone().into()
    }
    fn get_accept_queue_mut<D: device::WeakId, BT: TcpBindingsTypes>(
        (conn, _addr): &mut Self::ConnectionAndAddr<D, BT>,
    ) -> &mut Option<
        AcceptQueue<
            TcpSocketId<Self, D, BT>,
            BT::ReturnedBuffers,
            BT::ListenerNotifierOrProvidedBuffers,
        >,
    > {
        &mut conn.accept_queue
    }
    fn get_defunct<D: device::WeakId, BT: TcpBindingsTypes>(
        (conn, _addr): &Self::ConnectionAndAddr<D, BT>,
    ) -> bool {
        conn.defunct
    }
    fn get_state<D: device::WeakId, BT: TcpBindingsTypes>(
        (conn, _addr): &Self::ConnectionAndAddr<D, BT>,
    ) -> &State<BT::Instant, BT::ReceiveBuffer, BT::SendBuffer, BT::ListenerNotifierOrProvidedBuffers>
    {
        &conn.state
    }
    fn get_bound_info<D: device::WeakId>(
        listener_addr: &ListenerAddr<Self::ListenerIpAddr, D>,
    ) -> BoundInfo<Self::Addr, D> {
        listener_addr.clone().into()
    }

    fn destroy_socket_with_demux_id<
        CC: TcpContext<Self, BC> + TcpContext<Self::OtherVersion, BC>,
        BC: TcpBindingsContext<Self, CC::WeakDeviceId>
            + TcpBindingsContext<Self::OtherVersion, CC::WeakDeviceId>,
    >(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        demux_id: Self::DemuxSocketId<CC::WeakDeviceId, BC>,
    ) {
        match demux_id {
            EitherStack::ThisStack(id) => destroy_socket(core_ctx, bindings_ctx, id),
            EitherStack::OtherStack(id) => destroy_socket(core_ctx, bindings_ctx, id),
        }
    }
}

/// Socket options that are accessible on IPv6 sockets.
#[derive(Derivative, Debug, Clone, Copy, PartialEq, Eq)]
#[derivative(Default)]
pub struct Ipv6Options {
    #[derivative(Default(value = "true"))]
    pub(crate) dual_stack_enabled: bool,
}

impl DualStackIpExt for Ipv6 {
    type DemuxSocketId<D: device::WeakId, BT: TcpBindingsTypes> = TcpSocketId<Ipv6, D, BT>;
    type ConnectionAndAddr<D: device::WeakId, BT: TcpBindingsTypes> = EitherStack<
        (Connection<Ipv6, Ipv6, D, BT>, ConnAddr<ConnIpAddr<Ipv6Addr, NonZeroU16, NonZeroU16>, D>),
        (Connection<Ipv6, Ipv4, D, BT>, ConnAddr<ConnIpAddr<Ipv4Addr, NonZeroU16, NonZeroU16>, D>),
    >;
    type DualStackIpOptions = Ipv6Options;
    type ListenerIpAddr = DualStackListenerIpAddr<Ipv6Addr, NonZeroU16>;

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
    fn get_conn_info<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &Self::ConnectionAndAddr<D, BT>,
    ) -> ConnectionInfo<Self::Addr, D> {
        match conn_and_addr {
            EitherStack::ThisStack((_conn, addr)) => addr.clone().into(),
            EitherStack::OtherStack((
                _conn,
                ConnAddr {
                    ip:
                        ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) },
                    device,
                },
            )) => ConnectionInfo {
                local_addr: SocketAddr {
                    ip: maybe_zoned(local_ip.addr().to_ipv6_mapped(), device),
                    port: *local_port,
                },
                remote_addr: SocketAddr {
                    ip: maybe_zoned(remote_ip.addr().to_ipv6_mapped(), device),
                    port: *remote_port,
                },
                device: device.clone(),
            },
        }
    }
    fn get_accept_queue_mut<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &mut Self::ConnectionAndAddr<D, BT>,
    ) -> &mut Option<
        AcceptQueue<
            TcpSocketId<Self, D, BT>,
            BT::ReturnedBuffers,
            BT::ListenerNotifierOrProvidedBuffers,
        >,
    > {
        match conn_and_addr {
            EitherStack::ThisStack((conn, _addr)) => &mut conn.accept_queue,
            EitherStack::OtherStack((conn, _addr)) => &mut conn.accept_queue,
        }
    }
    fn get_defunct<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &Self::ConnectionAndAddr<D, BT>,
    ) -> bool {
        match conn_and_addr {
            EitherStack::ThisStack((conn, _addr)) => conn.defunct,
            EitherStack::OtherStack((conn, _addr)) => conn.defunct,
        }
    }
    fn get_state<D: device::WeakId, BT: TcpBindingsTypes>(
        conn_and_addr: &Self::ConnectionAndAddr<D, BT>,
    ) -> &State<BT::Instant, BT::ReceiveBuffer, BT::SendBuffer, BT::ListenerNotifierOrProvidedBuffers>
    {
        match conn_and_addr {
            EitherStack::ThisStack((conn, _addr)) => &conn.state,
            EitherStack::OtherStack((conn, _addr)) => &conn.state,
        }
    }
    fn get_bound_info<D: device::WeakId>(
        ListenerAddr { ip, device }: &ListenerAddr<Self::ListenerIpAddr, D>,
    ) -> BoundInfo<Self::Addr, D> {
        match ip {
            DualStackListenerIpAddr::ThisStack(ip) => {
                ListenerAddr { ip: ip.clone(), device: device.clone() }.into()
            }
            DualStackListenerIpAddr::OtherStack(ListenerIpAddr {
                addr,
                identifier: local_port,
            }) => BoundInfo {
                addr: Some(maybe_zoned(
                    addr.map(|a| a.addr()).unwrap_or(Ipv4::UNSPECIFIED_ADDRESS).to_ipv6_mapped(),
                    &device,
                )),
                port: *local_port,
                device: device.clone(),
            },
            DualStackListenerIpAddr::BothStacks(local_port) => {
                BoundInfo { addr: None, port: *local_port, device: device.clone() }
            }
        }
    }

    fn destroy_socket_with_demux_id<
        CC: TcpContext<Self, BC> + TcpContext<Self::OtherVersion, BC>,
        BC: TcpBindingsContext<Self, CC::WeakDeviceId>
            + TcpBindingsContext<Self::OtherVersion, CC::WeakDeviceId>,
    >(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        demux_id: Self::DemuxSocketId<CC::WeakDeviceId, BC>,
    ) {
        destroy_socket(core_ctx, bindings_ctx, demux_id)
    }
}

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

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> From<WeakTcpSocketId<I, D, BT>>
    for TimerId<D, BT>
{
    fn from(f: WeakTcpSocketId<I, D, BT>) -> Self {
        I::map_ip(f, TimerId::V4, TimerId::V6)
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

pub trait TcpBindingsTypes: InstantBindingsTypes + TimerBindingsTypes + 'static {
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
// TODO(https://fxbug.dev/42083407): Remove type parameters from this trait,
// they're no longer needed.
pub trait TcpBindingsContext<I: DualStackIpExt, D: device::WeakId>:
    Sized + TimerContext2 + TracingContext + RngContext + TcpBindingsTypes
{
}

impl<I, D, O> TcpBindingsContext<I, D> for O
where
    I: DualStackIpExt,
    O: TimerContext2 + TracingContext + RngContext + TcpBindingsTypes + Sized,
    D: device::WeakId,
{
}

pub trait TcpDemuxContext<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>:
    TcpCoreTimerContext<I, D, BT>
{
    type IpTransportCtx<'a>: TransportIpContext<I, BT, DeviceId = D::Strong, WeakDeviceId = D>
        + DeviceIpSocketHandler<I, BT>
        + TcpCoreTimerContext<I, D, BT>
        + CounterContext<TcpCounters<I>>
        // NB: We need to be able to access counters for the `OtherVersion` to
        // attribute IPv4 packets on dual stack sockets to the IPv6 counters.
        + CounterContext<TcpCounters<I::OtherVersion>>;

    /// Calls `f` with non-mutable access to the demux state.
    fn with_demux<O, F: FnOnce(&DemuxState<I, D, BT>) -> O>(&mut self, cb: F) -> O;

    /// Calls `f` with mutable access to the demux state.
    fn with_demux_mut<O, F: FnOnce(&mut DemuxState<I, D, BT>) -> O>(&mut self, cb: F) -> O;

    /// Calls `f` with mutable access to the demux state and a transport core
    /// context.
    // TODO(https://fxbug.dev/327489613): This function allows us doing IP-level
    // operations while holding on to the demux lock. We might want to revisit
    // when we enable multithreading.
    fn with_demux_mut_and_ip_transport_ctx<
        O,
        F: FnOnce(&mut DemuxState<I, D, BT>, &mut Self::IpTransportCtx<'_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

/// Provides access to the current stack of the context.
///
/// This is useful when dealing with logic that applies to the current stack
/// but we want to be version agnostic: we have different associated types for
/// single-stack and dual-stack contexts, we can use this function to turn them
/// into the same type that only provides access to the current version of the
/// stack and trims down access to `I::OtherVersion`.
pub trait AsThisStack<T> {
    /// Get the this stack version of the context.
    fn as_this_stack(&mut self) -> &mut T;
}

impl<T> AsThisStack<T> for T {
    fn as_this_stack(&mut self) -> &mut T {
        self
    }
}

/// A shortcut for the `CoreTimerContext` required by TCP.
pub trait TcpCoreTimerContext<I: DualStackIpExt, D: device::WeakId, BC: TcpBindingsTypes>:
    CoreTimerContext<WeakTcpSocketId<I, D, BC>, BC>
{
}

impl<CC, I, D, BC> TcpCoreTimerContext<I, D, BC> for CC
where
    I: DualStackIpExt,
    D: device::WeakId,
    BC: TcpBindingsTypes,
    CC: CoreTimerContext<WeakTcpSocketId<I, D, BC>, BC>,
{
}

/// Core context for TCP.
pub trait TcpContext<I: DualStackIpExt, BC: TcpBindingsTypes>:
    TcpDemuxContext<I, Self::WeakDeviceId, BC> + IpSocketHandler<I, BC>
{
    /// The core context for the current version of the IP protocol. This is
    /// used to be version agnostic when the operation is on the current stack.
    type ThisStackIpTransportAndDemuxCtx<'a>: TransportIpContext<I, BC, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + DeviceIpSocketHandler<I, BC>
        + TcpDemuxContext<I, Self::WeakDeviceId, BC>
        + CounterContext<TcpCounters<I>>;

    /// The core context that will give access to this version of the IP layer.
    type SingleStackIpTransportAndDemuxCtx<'a>: TransportIpContext<I, BC, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + DeviceIpSocketHandler<I, BC>
        + TcpDemuxContext<I, Self::WeakDeviceId, BC>
        + AsThisStack<Self::ThisStackIpTransportAndDemuxCtx<'a>>
        + CounterContext<TcpCounters<I>>;

    /// A collection of type assertions that must be true in the single stack
    /// version, associated types and concrete types must unify and we can
    /// inspect types by converting them into the concrete types.
    type SingleStackConverter: OwnedOrRefsBidirectionalConverter<
            I::ConnectionAndAddr<Self::WeakDeviceId, BC>,
            (
                Connection<I, I, Self::WeakDeviceId, BC>,
                ConnAddr<ConnIpAddr<<I as Ip>::Addr, NonZeroU16, NonZeroU16>, Self::WeakDeviceId>,
            ),
        > + OwnedOrRefsBidirectionalConverter<I::ListenerIpAddr, ListenerIpAddr<I::Addr, NonZeroU16>>
        + OwnedOrRefsBidirectionalConverter<
            ListenerAddr<I::ListenerIpAddr, Self::WeakDeviceId>,
            ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, Self::WeakDeviceId>,
        >;

    /// The core context that will give access to both versions of the IP layer.
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
        + TcpDualStackContext<I, Self::WeakDeviceId, BC>
        + AsThisStack<Self::ThisStackIpTransportAndDemuxCtx<'a>>
        + CounterContext<TcpCounters<I>>
        + CounterContext<TcpCounters<I::OtherVersion>>;

    /// A collection of type assertions that must be true in the dual stack
    /// version, associated types and concrete types must unify and we can
    /// inspect types by converting them into the concrete types.
    type DualStackConverter: OwnedOrRefsBidirectionalConverter<
            I::ConnectionAndAddr<Self::WeakDeviceId, BC>,
            EitherStack<
                (
                    Connection<I, I, Self::WeakDeviceId, BC>,
                    ConnAddr<
                        ConnIpAddr<<I as Ip>::Addr, NonZeroU16, NonZeroU16>,
                        Self::WeakDeviceId,
                    >,
                ),
                (
                    Connection<I, I::OtherVersion, Self::WeakDeviceId, BC>,
                    ConnAddr<
                        ConnIpAddr<<I::OtherVersion as Ip>::Addr, NonZeroU16, NonZeroU16>,
                        Self::WeakDeviceId,
                    >,
                ),
            >,
        > + OwnedOrRefsBidirectionalConverter<
            I::ListenerIpAddr,
            DualStackListenerIpAddr<I::Addr, NonZeroU16>,
        > + OwnedOrRefsBidirectionalConverter<
            ListenerAddr<I::ListenerIpAddr, Self::WeakDeviceId>,
            ListenerAddr<DualStackListenerIpAddr<I::Addr, NonZeroU16>, Self::WeakDeviceId>,
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
        F: FnMut(&TcpSocketId<I, Self::WeakDeviceId, BC>, &TcpSocketState<I, Self::WeakDeviceId, BC>),
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

/// A ZST that helps convert IPv6 socket IDs into IPv4 demux IDs.
#[derive(Clone, Copy)]
pub struct Ipv6SocketIdToIpv4DemuxIdConverter;

/// This trait allows us to work around the life-time issue when we need to
/// convert an IPv6 socket ID into an IPv4 demux ID without holding on the
/// a dual-stack CoreContext.
pub trait DualStackDemuxIdConverter<I: DualStackIpExt> {
    /// Turns a [`TcpSocketId`] into the demuxer ID of the other stack.
    fn convert<D: device::WeakId, BT: TcpBindingsTypes>(
        &self,
        id: TcpSocketId<I, D, BT>,
    ) -> <I::OtherVersion as DualStackIpExt>::DemuxSocketId<D, BT>;
}

impl DualStackDemuxIdConverter<Ipv6> for Ipv6SocketIdToIpv4DemuxIdConverter {
    fn convert<D: device::WeakId, BT: TcpBindingsTypes>(
        &self,
        id: TcpSocketId<Ipv6, D, BT>,
    ) -> <Ipv4 as DualStackIpExt>::DemuxSocketId<D, BT> {
        EitherStack::OtherStack(id)
    }
}

/// A provider of dualstack socket functionality required by TCP sockets.
pub trait TcpDualStackContext<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    type Converter: DualStackDemuxIdConverter<I> + Clone + Copy;

    type DualStackIpTransportCtx<'a>: TransportIpContext<I, BT, DeviceId = D::Strong, WeakDeviceId = D>
        + DeviceIpSocketHandler<I, BT>
        + TcpCoreTimerContext<I, D, BT>
        + TransportIpContext<I::OtherVersion, BT, DeviceId = D::Strong, WeakDeviceId = D>
        + DeviceIpSocketHandler<I::OtherVersion, BT>
        + TcpCoreTimerContext<I::OtherVersion, D, BT>
        + CounterContext<TcpCounters<I>>;

    fn other_demux_id_converter(&self) -> Self::Converter;

    /// Turns a [`TcpSocketId`] into the demuxer ID of the other stack.
    fn into_other_demux_socket_id(
        &self,
        id: TcpSocketId<I, D, BT>,
    ) -> <I::OtherVersion as DualStackIpExt>::DemuxSocketId<D, BT> {
        self.other_demux_id_converter().convert(id)
    }

    /// Gets the enabled state of dual stack operations on the given socket.
    fn dual_stack_enabled(&self, ip_options: &I::DualStackIpOptions) -> bool;
    /// Sets the enabled state of dual stack operations on the given socket.
    fn set_dual_stack_enabled(&self, ip_options: &mut I::DualStackIpOptions, value: bool);

    /// Calls `f` with mutable access to both demux states.
    fn with_both_demux_mut<
        O,
        F: FnOnce(&mut DemuxState<I, D, BT>, &mut DemuxState<I::OtherVersion, D, BT>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls `f` with mutable access to both demux states and a transport
    /// core context can operate on both IP versions.
    // TODO(https://fxbug.dev/327489613): This function allows us doing IP-level
    // operations while holding on to the demux locks. We might want to revisit
    // when we enable multithreading.
    fn with_both_demux_mut_and_ip_transport_ctx<
        O,
        F: FnOnce(
            &mut DemuxState<I, D, BT>,
            &mut DemuxState<I::OtherVersion, D, BT>,
            &mut Self::DualStackIpTransportCtx<'_>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

/// Socket address includes the ip address and the port number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, GenericOverIp)]
#[generic_over_ip(A, IpAddress)]
pub struct SocketAddr<A: IpAddress, D> {
    /// The IP component of the address.
    pub ip: ZonedAddr<SpecifiedAddr<A>, D>,
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
        SocketAddr { ip: ip.map_zone(f), port }
    }
}

impl<A: IpAddress, D: fmt::Display> fmt::Display for SocketAddr<A, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        let Self { ip, port } = self;
        let formatter = AddrAndPortFormatter::<_, _, A::Version>::new(
            ip.as_ref().map_addr(core::convert::AsRef::<A>::as_ref),
            port,
        );
        formatter.fmt(f)
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
pub trait SpecSocketId: Clone + Eq + PartialEq + Debug + 'static {}
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
    type ListenerId = I::DemuxSocketId<D, BT>;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenerSharingState {
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
pub enum BoundSocketState<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    Listener((MaybeListener<I, D, BT>, ListenerSharingState, ListenerAddr<I::ListenerIpAddr, D>)),
    Connected { conn: I::ConnectionAndAddr<D, BT>, sharing: SharingState, timer: BT::Timer },
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
                listener.as_ref().is_some_and(|x| id == x) || bound.contains(id)
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
pub enum SharingState {
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
pub struct Unbound<D, Extra> {
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
pub enum TcpSocketSetEntry<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
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
pub struct TcpSocketSet<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>(
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
            let accept_queue = match &(*guard).socket_state {
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
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

type BoundSocketMap<I, D, BT> = socket::BoundSocketMap<I, D, TcpPortSpec, TcpSocketSpec<I, D, BT>>;

pub struct DemuxState<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    socketmap: BoundSocketMap<I, D, BT>,
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
pub struct TcpSocketState<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    pub(crate) socket_state: TcpSocketStateInner<I, D, BT>,
    // The following only contains IP version specific options, `socket_state`
    // may still hold generic IP options (inside of the `IpSock`).
    // TODO(https://issues.fuchsia.dev/324279602): We are planning to move the
    // options outside of the `IpSock` struct. Once that's happening, we need to
    // change here.
    pub(crate) ip_options: I::DualStackIpOptions,
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub enum TcpSocketStateInner<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    Unbound(Unbound<D, BT::ListenerNotifierOrProvidedBuffers>),
    Bound(BoundSocketState<I, D, BT>),
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> PortAllocImpl
    for BoundSocketMap<I, D, BT>
{
    const EPHEMERAL_RANGE: RangeInclusive<u16> = 49152..=65535;
    type Id = Option<SocketIpAddr<I::Addr>>;
    /// The TCP port allocator takes an extra optional argument with a port to
    /// avoid.
    ///
    /// This is used to sidestep possible self-connections when allocating a
    /// local port on a connect call with an unset local port.
    type PortAvailableArg = Option<NonZeroU16>;

    fn is_port_available(&self, addr: &Self::Id, port: u16, arg: &Option<NonZeroU16>) -> bool {
        // We can safely unwrap here, because the ports received in
        // `is_port_available` are guaranteed to be in `EPHEMERAL_RANGE`.
        let port = NonZeroU16::new(port).unwrap();

        // Reject ports matching the argument.
        if arg.is_some_and(|a| a == port) {
            return false;
        }

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

/// When binding to IPv6 ANY address (::), we need to allocate a port that is
/// available in both stacks.
impl<'a, I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> PortAllocImpl
    for (&'a BoundSocketMap<I, D, BT>, &'a BoundSocketMap<I::OtherVersion, D, BT>)
{
    const EPHEMERAL_RANGE: RangeInclusive<u16> =
        <BoundSocketMap<I, D, BT> as PortAllocImpl>::EPHEMERAL_RANGE;
    type Id = ();
    type PortAvailableArg = ();

    fn is_port_available(&self, (): &Self::Id, port: u16, (): &Self::PortAvailableArg) -> bool {
        let (this, other) = self;
        this.is_port_available(&None, port, &None) && other.is_port_available(&None, port, &None)
    }
}

impl<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> Sockets<I, D, BT> {
    pub(crate) fn new() -> Self {
        Self {
            demux: RwLock::new(DemuxState { socketmap: Default::default() }),
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

impl<SockI: DualStackIpExt, WireI: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes>
    Connection<SockI, WireI, D, BT>
{
    /// Updates this connection's state to reflect the error.
    ///
    /// The connection's soft error, if previously unoccupied, holds the error.
    fn on_icmp_error<CC: CounterContext<TcpCounters<SockI>>>(
        &mut self,
        core_ctx: &mut CC,
        seq: SeqNum,
        error: IcmpErrorCode,
    ) {
        let Connection { soft_error, state, .. } = self;
        let new_soft_error =
            core_ctx.with_counters(|counters| state.on_icmp_error(counters, error, seq));
        *soft_error = soft_error.or(new_soft_error);
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
pub struct Listener<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
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
pub struct BoundState<Extra> {
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
pub enum MaybeListener<I: DualStackIpExt, D: device::WeakId, BT: TcpBindingsTypes> {
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
    pub(crate) fn new(socket_state: TcpSocketStateInner<I, D, BT>) -> (Self, PrimaryRc<I, D, BT>) {
        let primary = PrimaryRc::new(RwLock::new(TcpSocketState {
            socket_state,
            ip_options: Default::default(),
        }));
        let socket = Self(PrimaryRc::clone_strong(&primary));
        (socket, primary)
    }

    pub(crate) fn new_cyclic<
        F: FnOnce(WeakTcpSocketId<I, D, BT>) -> TcpSocketStateInner<I, D, BT>,
    >(
        init: F,
    ) -> (Self, PrimaryRc<I, D, BT>) {
        let primary = PrimaryRc::new_cyclic(move |weak| {
            let socket_state = init(WeakTcpSocketId(weak));
            RwLock::new(TcpSocketState { socket_state, ip_options: Default::default() })
        });
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

fn bind_install_in_demux<I, BC, DC>(
    core_ctx: &mut DC,
    bindings_ctx: &mut BC,
    demux_socket_id: I::DemuxSocketId<DC::WeakDeviceId, BC>,
    addr: Option<ZonedAddr<SocketIpAddr<I::Addr>, DC::DeviceId>>,
    bound_device: &Option<DC::WeakDeviceId>,
    port: Option<NonZeroU16>,
    sharing: SharingState,
    DemuxState { socketmap }: &mut DemuxState<I, DC::WeakDeviceId, BC>,
) -> Result<
    (ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, DC::WeakDeviceId>, ListenerSharingState),
    LocalAddressError,
>
where
    I: DualStackIpExt,
    BC: TcpBindingsTypes + RngContext,
    DC: TransportIpContext<I, BC> + DeviceIpSocketHandler<I, BC>,
{
    let (local_ip, device) = match addr {
        Some(addr) => {
            // Extract the specified address and the device. The
            // device is either the one from the address or the one
            // to which the socket was previously bound.
            let (addr, required_device) =
                crate::transport::resolve_addr_with_device::<I::Addr, _, _, _>(
                    addr,
                    bound_device.clone(),
                )
                .map_err(LocalAddressError::Zone)?;

            let mut assigned_to = core_ctx.get_devices_with_assigned_addr(addr.clone().into());

            if !assigned_to.any(|d| {
                required_device.as_ref().map_or(true, |device| device == &EitherDeviceId::Strong(d))
            }) {
                return Err(LocalAddressError::AddressMismatch);
            }

            (Some(addr), required_device)
        }
        None => (None, bound_device.clone().map(EitherDeviceId::Weak)),
    };

    let weak_device = device.map(|d| d.as_weak(core_ctx).into_owned());
    let port = match port {
        None => {
            match algorithm::simple_randomized_port_alloc(
                &mut bindings_ctx.rng(),
                &local_ip,
                socketmap,
                &None,
            ) {
                Some(port) => NonZeroU16::new(port).expect("ephemeral ports must be non-zero"),
                None => {
                    return Err(LocalAddressError::FailedToAllocateLocalPort);
                }
            }
        }
        Some(port) => port,
    };

    let addr = ListenerAddr {
        ip: ListenerIpAddr { addr: local_ip, identifier: port },
        device: weak_device,
    };
    let sharing = ListenerSharingState { sharing, listening: false };

    let _inserted = socketmap
        .listeners_mut()
        .try_insert(addr.clone(), sharing.clone(), demux_socket_id)
        .map_err(|_: (InsertError, ListenerSharingState)| LocalAddressError::AddressInUse)?;

    Ok((addr, sharing))
}

fn try_update_listener_sharing<I, CC, BT>(
    core_ctx: MaybeDualStack<
        (&mut CC::DualStackIpTransportAndDemuxCtx<'_>, CC::DualStackConverter),
        (&mut CC::SingleStackIpTransportAndDemuxCtx<'_>, CC::SingleStackConverter),
    >,
    id: &TcpSocketId<I, CC::WeakDeviceId, BT>,
    addr: ListenerAddr<I::ListenerIpAddr, CC::WeakDeviceId>,
    sharing: &ListenerSharingState,
    new_sharing: ListenerSharingState,
) -> Result<ListenerSharingState, UpdateSharingError>
where
    I: DualStackIpExt,
    CC: TcpContext<I, BT>,
    BT: TcpBindingsTypes,
{
    match core_ctx {
        MaybeDualStack::NotDualStack((core_ctx, converter)) => {
            core_ctx.with_demux_mut(|DemuxState { socketmap }| {
                let mut entry = socketmap
                    .listeners_mut()
                    .entry(&I::into_demux_socket_id(id.clone()), &converter.convert(addr))
                    .expect("invalid listener id");
                entry.try_update_sharing(sharing, new_sharing)
            })
        }
        MaybeDualStack::DualStack((core_ctx, converter)) => match converter.convert(addr) {
            ListenerAddr { ip: DualStackListenerIpAddr::ThisStack(ip), device } => {
                TcpDemuxContext::<I, _, _>::with_demux_mut(core_ctx, |DemuxState { socketmap }| {
                    let mut entry = socketmap
                        .listeners_mut()
                        .entry(&I::into_demux_socket_id(id.clone()), &ListenerAddr { ip, device })
                        .expect("invalid listener id");
                    entry.try_update_sharing(sharing, new_sharing)
                })
            }
            ListenerAddr { ip: DualStackListenerIpAddr::OtherStack(ip), device } => {
                let demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                TcpDemuxContext::<I::OtherVersion, _, _>::with_demux_mut(
                    core_ctx,
                    |DemuxState { socketmap }| {
                        let mut entry = socketmap
                            .listeners_mut()
                            .entry(&demux_id, &ListenerAddr { ip, device })
                            .expect("invalid listener id");
                        entry.try_update_sharing(sharing, new_sharing)
                    },
                )
            }
            ListenerAddr { ip: DualStackListenerIpAddr::BothStacks(port), device } => {
                let other_demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                let demux_id = I::into_demux_socket_id(id.clone());
                core_ctx.with_both_demux_mut(
                    |DemuxState { socketmap: this_socketmap, .. },
                     DemuxState { socketmap: other_socketmap, .. }| {
                        let this_stack_listener_addr = ListenerAddr {
                            ip: ListenerIpAddr { addr: None, identifier: port },
                            device: device.clone(),
                        };
                        let mut this_stack_entry = this_socketmap
                            .listeners_mut()
                            .entry(&demux_id, &this_stack_listener_addr)
                            .expect("invalid listener id");
                        this_stack_entry.try_update_sharing(sharing, new_sharing)?;
                        let mut other_stack_entry = other_socketmap
                            .listeners_mut()
                            .entry(
                                &other_demux_id,
                                &ListenerAddr {
                                    ip: ListenerIpAddr { addr: None, identifier: port },
                                    device,
                                },
                            )
                            .expect("invalid listener id");
                        match other_stack_entry.try_update_sharing(sharing, new_sharing) {
                            Ok(()) => Ok(()),
                            Err(err) => {
                                this_stack_entry
                                    .try_update_sharing(&new_sharing, *sharing)
                                    .expect("failed to revert the sharing setting");
                                Err(err)
                            }
                        }
                    },
                )
            }
        },
    }?;
    Ok(new_sharing)
}

/// The TCP socket API.
pub struct TcpApi<I: Ip, C>(C, IpVersionMarker<I>);

impl<I: Ip, C> TcpApi<I, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, IpVersionMarker::new())
    }
}

/// A local alias for [`TcpSocketId`] for use in [`TcpApi`].
///
/// TODO(https://github.com/rust-lang/rust/issues/8995): Make this an inherent
/// associated type.
type TcpApiSocketId<I, C> = TcpSocketId<
    I,
    <<C as ContextPair>::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
    <C as ContextPair>::BindingsContext,
>;

impl<I, C> TcpApi<I, C>
where
    I: DualStackIpExt,
    C: ContextPair,
    C::CoreContext: TcpContext<I, C::BindingsContext> + CounterContext<TcpCounters<I>>,
    C::BindingsContext:
        TcpBindingsContext<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair, IpVersionMarker { .. }) = self;
        pair.core_ctx()
    }

    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, IpVersionMarker { .. }) = self;
        pair.contexts()
    }

    /// Creates a new socket in unbound state.
    pub fn create(
        &mut self,
        socket_extra: <C::BindingsContext as TcpBindingsTypes>::ListenerNotifierOrProvidedBuffers,
    ) -> TcpApiSocketId<I, C> {
        self.core_ctx().with_all_sockets_mut(|all_sockets| {
            let (sock, primary) = TcpSocketId::new(TcpSocketStateInner::Unbound(Unbound {
                buffer_sizes: C::BindingsContext::default_buffer_sizes(),
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

    /// Binds an unbound socket to a local socket address.
    ///
    /// Requests that the given socket be bound to the local address, if one is
    /// provided; otherwise to all addresses. If `port` is specified (is
    /// `Some`), the socket will be bound to that port. Otherwise a port will be
    /// selected to not conflict with existing bound or connected sockets.
    pub fn bind(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        addr: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        port: Option<NonZeroU16>,
    ) -> Result<(), BindError> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        enum BindAddr<I: DualStackIpExt, D> {
            BindInBothStacks,
            BindInOneStack(
                EitherStack<
                    Option<ZonedAddr<SocketIpAddr<I::Addr>, D>>,
                    Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, D>>,
                >,
            ),
        }
        debug!("bind {id:?} to {addr:?}:{port:?}");
        let bind_addr = match addr {
            None => I::map_ip(
                (),
                |()| BindAddr::BindInOneStack(EitherStack::ThisStack(None)),
                |()| BindAddr::BindInBothStacks,
            ),
            Some(addr) => match socket::address::try_unmap(addr) {
                TryUnmapResult::CannotBeUnmapped(addr) => {
                    BindAddr::BindInOneStack(EitherStack::ThisStack(Some(addr)))
                }
                TryUnmapResult::Mapped(addr) => {
                    BindAddr::BindInOneStack(EitherStack::OtherStack(addr))
                }
            },
        };

        // TODO(https://fxbug.dev/42055442): Check if local_ip is a unicast address.
        let (core_ctx, bindings_ctx) = self.contexts();
        let result = core_ctx.with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options } = socket_state;
            let Unbound { bound_device, buffer_sizes, socket_options, sharing, socket_extra } =
                match socket_state {
                    TcpSocketStateInner::Unbound(u) => u,
                    TcpSocketStateInner::Bound(_) => return Err(BindError::AlreadyBound),
                };

            let bound_state = BoundState {
                buffer_sizes: buffer_sizes.clone(),
                socket_options: socket_options.clone(),
                socket_extra: socket_extra.clone(),
            };

            // TODO(https://fxbug.dev/327489613): We have some code duplication
            // below calling `bind_install_in_demux` because we want to only
            // take the other stack's demux lock when necessary.
            // TODO(https://fxbug.dev/327488712): To maximize code reuse,
            // `bind_install_in_demux` doesn't acquire locks directly, but this
            // may cause worse contention down the road when multithreading is
            // enabled.
            let (listener_addr, sharing) = match core_ctx {
                MaybeDualStack::NotDualStack((core_ctx, converter)) => match bind_addr {
                    BindAddr::BindInOneStack(EitherStack::ThisStack(local_addr)) => {
                        let (addr, sharing) =
                            core_ctx.with_demux_mut_and_ip_transport_ctx(|demux, core_ctx| {
                                bind_install_in_demux(
                                    core_ctx,
                                    bindings_ctx,
                                    I::into_demux_socket_id(id.clone()),
                                    local_addr,
                                    bound_device,
                                    port,
                                    *sharing,
                                    demux,
                                )
                            })?;
                        (converter.convert_back(addr), sharing)
                    }
                    BindAddr::BindInOneStack(EitherStack::OtherStack(_)) | BindAddr::BindInBothStacks => {
                        return Err(LocalAddressError::CannotBindToAddress.into());
                    }
                },
                MaybeDualStack::DualStack((core_ctx, converter)) => {
                    let bind_addr = match (
                            core_ctx.dual_stack_enabled(&ip_options),
                            bind_addr
                        ) {
                        // Allow binding in both stacks when dual stack is
                        // enabled.
                        (true, BindAddr::BindInBothStacks)
                            => BindAddr::<I, _>::BindInBothStacks,
                        // Only bind in this stack if dual stack is not enabled.
                        (false, BindAddr::BindInBothStacks)
                            => BindAddr::BindInOneStack(EitherStack::ThisStack(None)),
                        // Binding to this stack is always allowed.
                        (true | false, BindAddr::BindInOneStack(EitherStack::ThisStack(ip)))
                            => BindAddr::BindInOneStack(EitherStack::ThisStack(ip)),
                        // Can bind to the other stack only when dual stack is
                        // enabled, otherwise an error is returned.
                        (true, BindAddr::BindInOneStack(EitherStack::OtherStack(ip)))
                            => BindAddr::BindInOneStack(EitherStack::OtherStack(ip)),
                        (false, BindAddr::BindInOneStack(EitherStack::OtherStack(_)))
                            => return Err(LocalAddressError::CannotBindToAddress.into()),
                    };
                    match bind_addr {
                        BindAddr::BindInOneStack(EitherStack::ThisStack(addr)) => {
                            let (ListenerAddr { ip, device }, sharing) =
                                TcpDemuxContext::<I, _, _>::with_demux_mut_and_ip_transport_ctx(core_ctx, |demux, core_ctx| {
                                    bind_install_in_demux(
                                        core_ctx,
                                        bindings_ctx,
                                        I::into_demux_socket_id(id.clone()),
                                        addr,
                                        bound_device,
                                        port,
                                        *sharing,
                                        demux,
                                    )
                                })?;
                            (
                                converter.convert_back(ListenerAddr {
                                    ip: DualStackListenerIpAddr::ThisStack(ip),
                                    device,
                                }),
                                sharing,
                            )
                        }
                        BindAddr::BindInOneStack(EitherStack::OtherStack(addr)) => {
                            let other_demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                            let (ListenerAddr { ip, device }, sharing) =
                                TcpDemuxContext::<I::OtherVersion, _, _>::with_demux_mut_and_ip_transport_ctx(core_ctx, |demux, core_ctx| {
                                    bind_install_in_demux(
                                        core_ctx,
                                        bindings_ctx,
                                        other_demux_id,
                                        addr,
                                        bound_device,
                                        port,
                                        *sharing,
                                        demux,
                                    )
                                })?;
                            (
                                converter.convert_back(ListenerAddr {
                                    ip: DualStackListenerIpAddr::OtherStack(ip),
                                    device,
                                }),
                                sharing,
                            )
                        }
                        BindAddr::BindInBothStacks => {
                            let other_demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                            let (port, device, sharing) =
                                core_ctx.with_both_demux_mut_and_ip_transport_ctx(|demux, other_demux, core_ctx| {
                                    // We need to allocate the port for both
                                    // stacks before `bind_inner` tries to make
                                    // a decision, because it might give two
                                    // unrelated ports which is undesired.
                                    let port = match port {
                                        Some(port) => port,
                                        None => match algorithm::simple_randomized_port_alloc(
                                            &mut bindings_ctx.rng(),
                                            &(),
                                            &(&demux.socketmap, &other_demux.socketmap),
                                            &(),
                                        ){
                                            Some(port) => NonZeroU16::new(port)
                                                .expect("ephemeral ports must be non-zero"),
                                            None => {
                                                return Err(LocalAddressError::FailedToAllocateLocalPort);
                                            }
                                        }
                                    };
                                    let (this_stack_addr, this_stack_sharing) = bind_install_in_demux(
                                        core_ctx,
                                        bindings_ctx,
                                        I::into_demux_socket_id(id.clone()),
                                        None,
                                        bound_device,
                                        Some(port),
                                        *sharing,
                                        demux,
                                    )?;
                                    match bind_install_in_demux(
                                        core_ctx,
                                        bindings_ctx,
                                        other_demux_id,
                                        None,
                                        bound_device,
                                        Some(port),
                                        *sharing,
                                        other_demux,
                                    ) {
                                        Ok((ListenerAddr { ip, device }, other_stack_sharing)) => {
                                            assert_eq!(this_stack_addr.ip.identifier, ip.identifier);
                                            assert_eq!(this_stack_sharing, other_stack_sharing);
                                            Ok((port, device, this_stack_sharing))
                                        }
                                        Err(err) => {
                                            demux.socketmap.listeners_mut().remove(&I::into_demux_socket_id(id.clone()), &this_stack_addr).expect("failed to unbind");
                                            Err(err)
                                        }
                                    }
                                })?;
                            (
                                ListenerAddr {
                                    ip: converter.convert_back(DualStackListenerIpAddr::BothStacks(port)),
                                    device,
                                },
                                sharing,
                            )
                        }
                    }
                },
            };

            *socket_state = TcpSocketStateInner::Bound(BoundSocketState::Listener((
                MaybeListener::Bound(bound_state),
                sharing,
                listener_addr,
            )));
            Ok(())
        });
        match &result {
            Err(BindError::LocalAddressError(LocalAddressError::FailedToAllocateLocalPort)) => {
                core_ctx.increment(|counters| &counters.failed_port_reservations);
            }
            Err(_) | Ok(_) => {}
        }
        result
    }

    /// Listens on an already bound socket.
    pub fn listen(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        backlog: NonZeroUsize,
    ) -> Result<(), ListenError> {
        debug!("listen on {id:?} with backlog {backlog}");
        self.core_ctx().with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            let (listener, listener_sharing, addr) = match socket_state {
                TcpSocketStateInner::Bound(BoundSocketState::Listener((l, sharing, addr))) => {
                    match l {
                        MaybeListener::Listener(_) => return Err(ListenError::NotSupported),
                        MaybeListener::Bound(_) => (l, sharing, addr),
                    }
                }
                TcpSocketStateInner::Bound(BoundSocketState::Connected { .. })
                | TcpSocketStateInner::Unbound(_) => return Err(ListenError::NotSupported),
            };
            let new_sharing = {
                let ListenerSharingState { sharing, listening } = listener_sharing;
                debug_assert!(!*listening, "invalid bound ID that has a listener socket");
                ListenerSharingState { sharing: *sharing, listening: true }
            };
            *listener_sharing = try_update_listener_sharing::<_, C::CoreContext, _>(
                core_ctx,
                id,
                addr.clone(),
                listener_sharing,
                new_sharing,
            )
            .map_err(|UpdateSharingError| ListenError::ListenerExists)?;

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

    /// Accepts an established socket from the queue of a listener socket.
    pub fn accept(
        &mut self,
        id: &TcpApiSocketId<I, C>,
    ) -> Result<
        (
            TcpApiSocketId<I, C>,
            SocketAddr<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId>,
            <C::BindingsContext as TcpBindingsTypes>::ReturnedBuffers,
        ),
        AcceptError,
    > {
        let (conn_id, client_buffers) = self.core_ctx().with_socket_mut(id, |socket_state| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            debug!("accept on {id:?}");
            let Listener { backlog: _, buffer_sizes: _, socket_options: _, accept_queue } =
                match socket_state {
                    TcpSocketStateInner::Bound(BoundSocketState::Listener((
                        MaybeListener::Listener(l),
                        _sharing,
                        _addr,
                    ))) => l,
                    TcpSocketStateInner::Unbound(_)
                    | TcpSocketStateInner::Bound(BoundSocketState::Connected { .. })
                    | TcpSocketStateInner::Bound(BoundSocketState::Listener((
                        MaybeListener::Bound(_),
                        _,
                        _,
                    ))) => return Err(AcceptError::NotSupported),
                };
            let (conn_id, client_buffers) =
                accept_queue.pop_ready().ok_or(AcceptError::WouldBlock)?;

            Ok::<_, AcceptError>((conn_id, client_buffers))
        })?;

        let remote_addr =
            self.core_ctx().with_socket_mut_and_converter(&conn_id, |socket_state, _converter| {
                let TcpSocketState { socket_state, ip_options: _ } = socket_state;
                let conn_and_addr = assert_matches!(
                    socket_state,
                    TcpSocketStateInner::Bound(BoundSocketState::Connected{ conn, .. }) => conn,
                    "invalid socket ID"
                );
                *I::get_accept_queue_mut(conn_and_addr) = None;
                let ConnectionInfo { local_addr: _, remote_addr, device: _ } =
                    I::get_conn_info(conn_and_addr);
                remote_addr
            });

        debug!("accepted connection {conn_id:?} from {remote_addr:?} on {id:?}");
        Ok((conn_id, remote_addr, client_buffers))
    }

    /// Connects a socket to a remote address.
    ///
    /// When the method returns, the connection is not guaranteed to be
    /// established. It is up to the caller (Bindings) to determine when the
    /// connection has been established. Bindings are free to use anything
    /// available on the platform to check, for instance, signals.
    pub fn connect(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        remote_ip: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        remote_port: NonZeroU16,
    ) -> Result<(), ConnectError> {
        #[derive(Clone)]
        enum LocalAddr<I: DualStackIpExt, D> {
            BoundInOneStack(
                EitherStack<
                    ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>,
                    ListenerAddr<ListenerIpAddr<<I::OtherVersion as Ip>::Addr, NonZeroU16>, D>,
                >,
            ),
            BoundInBothStacks {
                port: NonZeroU16,
                device: Option<D>,
            },
            NotBound,
        }
        impl<I: DualStackIpExt, D> LocalAddr<I, D> {
            fn as_this_stack_listener_addr(
                self,
            ) -> Option<ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>> {
                match self {
                    Self::BoundInOneStack(EitherStack::ThisStack(local_addr)) => Some(local_addr),
                    Self::BoundInBothStacks { port, device } => Some(ListenerAddr {
                        ip: ListenerIpAddr { addr: None, identifier: port },
                        device,
                    }),
                    Self::BoundInOneStack(EitherStack::OtherStack(_)) | Self::NotBound => None,
                }
            }
            fn as_other_stack_listener_addr(
                self,
            ) -> Option<ListenerAddr<ListenerIpAddr<<I::OtherVersion as Ip>::Addr, NonZeroU16>, D>>
            {
                match self {
                    Self::BoundInOneStack(EitherStack::OtherStack(local_addr)) => Some(local_addr),
                    Self::BoundInBothStacks { port, device } => Some(ListenerAddr {
                        ip: ListenerIpAddr { addr: None, identifier: port },
                        device,
                    }),
                    Self::BoundInOneStack(EitherStack::ThisStack(_)) | Self::NotBound => None,
                }
            }
        }
        let (core_ctx, bindings_ctx) = self.contexts();
        let result =
            core_ctx.with_socket_mut_isn_transport_demux(id, |core_ctx, socket_state, isn| {
                let TcpSocketState { socket_state, ip_options } = socket_state;
                debug!("connect on {id:?} to {remote_ip:?}:{remote_port}");
                let remote_ip = socket::address::dual_stack_remote_ip::<I, _>(remote_ip);
                let (local_addr, sharing, socket_options, buffer_sizes, socket_extra) =
                    match socket_state {
                        TcpSocketStateInner::Bound(BoundSocketState::Connected {
                            conn,
                            sharing: _,
                            timer: _,
                        }) => {
                            let handshake_status = match core_ctx {
                                MaybeDualStack::NotDualStack((_core_ctx, converter)) => {
                                    let (conn, _addr) = converter.convert(conn);
                                    &mut conn.handshake_status
                                }
                                MaybeDualStack::DualStack((_core_ctx, converter)) => {
                                    match converter.convert(conn) {
                                        EitherStack::ThisStack((conn, _addr)) => {
                                            &mut conn.handshake_status
                                        }
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
                        TcpSocketStateInner::Unbound(Unbound {
                            bound_device: _,
                            socket_extra,
                            buffer_sizes,
                            socket_options,
                            sharing,
                        }) => (
                            LocalAddr::<I, _>::NotBound,
                            *sharing,
                            *socket_options,
                            *buffer_sizes,
                            socket_extra.clone(),
                        ),
                        TcpSocketStateInner::Bound(BoundSocketState::Listener((
                            listener,
                            ListenerSharingState { sharing, listening: _ },
                            addr,
                        ))) => {
                            let local_addr = match &core_ctx {
                                MaybeDualStack::DualStack((_core_ctx, converter)) => {
                                    match converter.convert(addr.clone()) {
                                        ListenerAddr {
                                            ip: DualStackListenerIpAddr::ThisStack(ip),
                                            device,
                                        } => LocalAddr::BoundInOneStack(EitherStack::ThisStack(
                                            ListenerAddr { ip, device },
                                        )),
                                        ListenerAddr {
                                            ip: DualStackListenerIpAddr::OtherStack(ip),
                                            device,
                                        } => LocalAddr::BoundInOneStack(EitherStack::OtherStack(
                                            ListenerAddr { ip, device },
                                        )),
                                        ListenerAddr {
                                            ip: DualStackListenerIpAddr::BothStacks(port),
                                            device,
                                        } => LocalAddr::BoundInBothStacks { port, device },
                                    }
                                }
                                MaybeDualStack::NotDualStack((_core_ctx, converter)) => {
                                    LocalAddr::BoundInOneStack(EitherStack::ThisStack(
                                        converter.convert(addr.clone()),
                                    ))
                                }
                            };
                            match listener {
                                MaybeListener::Bound(BoundState {
                                    buffer_sizes,
                                    socket_options,
                                    socket_extra,
                                }) => (
                                    local_addr,
                                    *sharing,
                                    *socket_options,
                                    *buffer_sizes,
                                    socket_extra.clone(),
                                ),
                                MaybeListener::Listener(_) => return Err(ConnectError::Listener),
                            }
                        }
                    };
                fn single_stack_remover<
                    WireI: DualStackIpExt,
                    D: device::WeakId,
                    BT: TcpBindingsTypes,
                >(
                    socketmap: &mut BoundSocketMap<WireI, D, BT>,
                    demux_id: &WireI::DemuxSocketId<D, BT>,
                    listener_addr: &ListenerAddr<ListenerIpAddr<WireI::Addr, NonZeroU16>, D>,
                ) {
                    socketmap
                        .listeners_mut()
                        .remove(demux_id, listener_addr)
                        .expect("failed to remove a bound socket");
                }
                // TODO(https://fxbug.dev/327489613): We have some code duplication
                // below calling `connect_inner` because we want to only take the
                // other stack's demux lock when necessary.
                // TODO(https://fxbug.dev/327488712): To maximize code reuse,
                // `connect_inner` doesn't acquire locks directly, but this may cause
                // worse contention down the road when multithreading is enabled.
                match (core_ctx, local_addr, remote_ip) {
                    // If not dual stack, we allow the connect operation if socket
                    // was not bound or bound to a this-stack local address before,
                    // and the remote address also belongs to this stack.
                    (
                        MaybeDualStack::NotDualStack((core_ctx, converter)),
                        local_addr @ (LocalAddr::BoundInOneStack(EitherStack::ThisStack(_))
                        | LocalAddr::NotBound),
                        DualStackRemoteIp::ThisStack(remote_ip),
                    ) => {
                        core_ctx.with_demux_mut_and_ip_transport_ctx(|demux, core_ctx| {
                            connect_inner(
                                core_ctx,
                                bindings_ctx,
                                id,
                                &I::into_demux_socket_id(id.clone()),
                                isn,
                                local_addr.as_this_stack_listener_addr(),
                                remote_ip,
                                remote_port,
                                socket_extra,
                                buffer_sizes,
                                socket_options,
                                sharing,
                                demux,
                                single_stack_remover,
                                |conn, addr| converter.convert_back((conn, addr)),
                                <C::CoreContext as CoreTimerContext<_, _>>::convert_timer,
                                socket_state,
                            )
                        })?;
                        Ok(())
                    }
                    // If dual stack, we can perform a this-stack only connection
                    // if both the local and remote IP addresses belong to this
                    // stack, or it was never bound to any local address.
                    (
                        MaybeDualStack::DualStack((core_ctx, converter)),
                        local_addr @ (LocalAddr::BoundInOneStack(EitherStack::ThisStack(_))
                        | LocalAddr::NotBound),
                        DualStackRemoteIp::ThisStack(remote_ip),
                    ) => core_ctx.with_demux_mut_and_ip_transport_ctx(|demux, core_ctx| {
                        connect_inner(
                            core_ctx,
                            bindings_ctx,
                            id,
                            &I::into_demux_socket_id(id.clone()),
                            isn,
                            local_addr.as_this_stack_listener_addr(),
                            remote_ip,
                            remote_port,
                            socket_extra,
                            buffer_sizes,
                            socket_options,
                            sharing,
                            demux,
                            single_stack_remover,
                            |conn, addr| {
                                converter.convert_back(EitherStack::ThisStack((conn, addr)))
                            },
                            <C::CoreContext as CoreTimerContext<_, _>>::convert_timer,
                            socket_state,
                        )
                    }),
                    // If dual stack, we can perform an other-stack only connection
                    // if both the local and remote IP addresses belong to other
                    // stack, or it was never bound to any local address.
                    (
                        MaybeDualStack::DualStack((core_ctx, converter)),
                        local_addr @ (LocalAddr::BoundInOneStack(EitherStack::OtherStack(_))
                        | LocalAddr::NotBound),
                        DualStackRemoteIp::OtherStack(remote_ip),
                    ) => {
                        if !core_ctx.dual_stack_enabled(ip_options) {
                            return Err(ConnectError::NoRoute);
                        }
                        let other_demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                        core_ctx.with_demux_mut_and_ip_transport_ctx(|demux, core_ctx| {
                            connect_inner(
                                core_ctx,
                                bindings_ctx,
                                id,
                                &other_demux_id,
                                isn,
                                local_addr.as_other_stack_listener_addr(),
                                remote_ip,
                                remote_port,
                                socket_extra,
                                buffer_sizes,
                                socket_options,
                                sharing,
                                demux,
                                single_stack_remover,
                                |conn, addr| {
                                    converter.convert_back(EitherStack::OtherStack((conn, addr)))
                                },
                                <C::CoreContext as CoreTimerContext<_, _>>::convert_timer,
                                socket_state,
                            )
                        })
                    }
                    // If dual stack, we can perform a this-stack only connection
                    // if remote IP addresses belong to this stack, but it was bound
                    // in both stacks, we need to remove it from both demux states.
                    (
                        MaybeDualStack::DualStack((core_ctx, converter)),
                        LocalAddr::BoundInBothStacks { port, device },
                        DualStackRemoteIp::ThisStack(remote_ip),
                    ) => {
                        let other_demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                        core_ctx.with_both_demux_mut_and_ip_transport_ctx(
                            |demux, other_demux, core_ctx| {
                                connect_inner(
                                    core_ctx,
                                    bindings_ctx,
                                    id,
                                    &I::into_demux_socket_id(id.clone()),
                                    isn,
                                    Some(ListenerAddr {
                                        ip: ListenerIpAddr { addr: None, identifier: port },
                                        device: device.clone(),
                                    }),
                                    remote_ip,
                                    remote_port,
                                    socket_extra,
                                    buffer_sizes,
                                    socket_options,
                                    sharing,
                                    demux,
                                    |socketmap, demux_id, listener_addr| {
                                        single_stack_remover(socketmap, demux_id, listener_addr);
                                        single_stack_remover(
                                            &mut other_demux.socketmap,
                                            &other_demux_id,
                                            &ListenerAddr {
                                                ip: ListenerIpAddr { addr: None, identifier: port },
                                                device,
                                            },
                                        );
                                    },
                                    |conn, addr| {
                                        converter.convert_back(EitherStack::ThisStack((conn, addr)))
                                    },
                                    <C::CoreContext as CoreTimerContext<_, _>>::convert_timer,
                                    socket_state,
                                )
                            },
                        )
                    }
                    // If dual stack, we can perform an other-stack only connection
                    // if remote IP addresses belong to the other stack, but it was
                    // bound in both stacks, we need to remove it from both demux
                    // states.
                    (
                        MaybeDualStack::DualStack((core_ctx, converter)),
                        LocalAddr::BoundInBothStacks { port, device },
                        DualStackRemoteIp::OtherStack(remote_ip),
                    ) => {
                        let other_demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                        core_ctx.with_both_demux_mut_and_ip_transport_ctx(
                            |demux, other_demux, core_ctx| {
                                connect_inner(
                                    core_ctx,
                                    bindings_ctx,
                                    id,
                                    &other_demux_id,
                                    isn,
                                    Some(ListenerAddr {
                                        ip: ListenerIpAddr { addr: None, identifier: port },
                                        device: device.clone(),
                                    }),
                                    remote_ip,
                                    remote_port,
                                    socket_extra,
                                    buffer_sizes,
                                    socket_options,
                                    sharing,
                                    other_demux,
                                    |socketmap, demux_id, listener_addr| {
                                        single_stack_remover(socketmap, demux_id, listener_addr);
                                        single_stack_remover(
                                            &mut demux.socketmap,
                                            &I::into_demux_socket_id(id.clone()),
                                            &ListenerAddr {
                                                ip: ListenerIpAddr { addr: None, identifier: port },
                                                device,
                                            },
                                        );
                                    },
                                    |conn, addr| {
                                        converter
                                            .convert_back(EitherStack::OtherStack((conn, addr)))
                                    },
                                    <C::CoreContext as CoreTimerContext<_, _>>::convert_timer,
                                    socket_state,
                                )
                            },
                        )
                    }
                    // Not possible for a non-dual-stack socket to bind in the other
                    // stack.
                    (
                        MaybeDualStack::NotDualStack(_),
                        LocalAddr::BoundInOneStack(EitherStack::OtherStack(_))
                        | LocalAddr::BoundInBothStacks { .. },
                        DualStackRemoteIp::ThisStack(_) | DualStackRemoteIp::OtherStack(_),
                    ) => unreachable!("The socket cannot be bound in the other stack"),
                    // Can't connect from one stack to the other.
                    (
                        MaybeDualStack::DualStack(_),
                        LocalAddr::BoundInOneStack(EitherStack::OtherStack(_)),
                        DualStackRemoteIp::ThisStack(_),
                    ) => Err(ConnectError::NoRoute),
                    // Can't connect from one stack to the other.
                    (
                        MaybeDualStack::DualStack(_) | MaybeDualStack::NotDualStack(_),
                        LocalAddr::BoundInOneStack(EitherStack::ThisStack(_)),
                        DualStackRemoteIp::OtherStack(_),
                    ) => Err(ConnectError::NoRoute),
                    // Can't connect to the other stack for non-dual-stack sockets.
                    (
                        MaybeDualStack::NotDualStack(_),
                        LocalAddr::NotBound,
                        DualStackRemoteIp::OtherStack(_),
                    ) => Err(ConnectError::NoRoute),
                }
            });
        match &result {
            Ok(()) => {}
            Err(err) => {
                core_ctx.increment(|counters| &counters.failed_connection_attempts);
                match err {
                    ConnectError::NoRoute => {
                        core_ctx.increment(|counters| &counters.active_open_no_route_errors)
                    }
                    ConnectError::NoPort => {
                        core_ctx.increment(|counters| &counters.failed_port_reservations)
                    }
                    _ => {}
                }
            }
        }
        result
    }

    /// Closes a socket.
    pub fn close(&mut self, id: TcpApiSocketId<I, C>) {
        let (core_ctx, bindings_ctx) = self.contexts();
        let (destroy, pending) =
            core_ctx.with_socket_mut_transport_demux(&id, |core_ctx, socket_state| {
                let TcpSocketState { socket_state, ip_options: _ } = socket_state;
                match socket_state {
                    TcpSocketStateInner::Unbound(_) => (true, None),
                    TcpSocketStateInner::Bound(BoundSocketState::Listener((
                        maybe_listener,
                        _sharing,
                        addr,
                    ))) => {
                        match core_ctx {
                            MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                                TcpDemuxContext::<I, _, _>::with_demux_mut(
                                    core_ctx,
                                    |DemuxState { socketmap }| {
                                        socketmap
                                            .listeners_mut()
                                            .remove(
                                                &I::into_demux_socket_id(id.clone()),
                                                &converter.convert(addr),
                                            )
                                            .expect("failed to remove from socketmap");
                                    },
                                );
                            }
                            MaybeDualStack::DualStack((core_ctx, converter)) => {
                                match converter.convert(addr.clone()) {
                                    ListenerAddr {
                                        ip: DualStackListenerIpAddr::ThisStack(ip),
                                        device,
                                    } => TcpDemuxContext::<I, _, _>::with_demux_mut(
                                        core_ctx,
                                        |DemuxState { socketmap }| {
                                            socketmap
                                                .listeners_mut()
                                                .remove(
                                                    &I::into_demux_socket_id(id.clone()),
                                                    &ListenerAddr { ip, device },
                                                )
                                                .expect("failed to remove from socketmap");
                                        },
                                    ),
                                    ListenerAddr {
                                        ip: DualStackListenerIpAddr::OtherStack(ip),
                                        device,
                                    } => {
                                        let other_demux_id =
                                            core_ctx.into_other_demux_socket_id(id.clone());
                                        TcpDemuxContext::<I::OtherVersion, _, _>::with_demux_mut(
                                            core_ctx,
                                            |DemuxState { socketmap }| {
                                                socketmap
                                                    .listeners_mut()
                                                    .remove(
                                                        &other_demux_id,
                                                        &ListenerAddr { ip, device },
                                                    )
                                                    .expect("failed to remove from socketmap");
                                            },
                                        );
                                    }
                                    ListenerAddr {
                                        ip: DualStackListenerIpAddr::BothStacks(port),
                                        device,
                                    } => {
                                        let other_demux_id =
                                            core_ctx.into_other_demux_socket_id(id.clone());
                                        core_ctx.with_both_demux_mut(|demux, other_demux| {
                                            demux
                                                .socketmap
                                                .listeners_mut()
                                                .remove(
                                                    &I::into_demux_socket_id(id.clone()),
                                                    &ListenerAddr {
                                                        ip: ListenerIpAddr {
                                                            addr: None,
                                                            identifier: port,
                                                        },
                                                        device: device.clone(),
                                                    },
                                                )
                                                .expect("failed to remove from socketmap");
                                            other_demux
                                                .socketmap
                                                .listeners_mut()
                                                .remove(
                                                    &other_demux_id,
                                                    &ListenerAddr {
                                                        ip: ListenerIpAddr {
                                                            addr: None,
                                                            identifier: port,
                                                        },
                                                        device,
                                                    },
                                                )
                                                .expect("failed to remove from socketmap");
                                        });
                                    }
                                }
                            }
                        };
                        let pending = match maybe_listener {
                            MaybeListener::Bound(_) => None,
                            MaybeListener::Listener(listener) => {
                                let (pending, _socket_extra) = listener.accept_queue.close();
                                Some(pending)
                            }
                        };
                        (true, pending)
                    }
                    TcpSocketStateInner::Bound(BoundSocketState::Connected {
                        conn,
                        sharing: _,
                        timer,
                    }) => {
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
                            timer: &mut BC::Timer,
                        ) -> bool
                        where
                            SockI: DualStackIpExt,
                            WireI: DualStackIpExt,
                            BC: TcpBindingsContext<SockI, CC::WeakDeviceId>,
                            CC: TransportIpContext<WireI, BC>
                                + TcpDemuxContext<WireI, CC::WeakDeviceId, BC>
                                + CounterContext<TcpCounters<SockI>>,
                        {
                            conn.defunct = true;
                            let already_closed = match core_ctx.with_counters(|counters| {
                                conn.state.close(
                                    counters,
                                    CloseReason::Close { now: bindings_ctx.now() },
                                    &conn.socket_options,
                                )
                            }) {
                                Err(CloseError::NoConnection) => true,
                                Err(CloseError::Closing) | Ok(()) => false,
                            };
                            // The connection transitions to closed because of
                            // this call, we need to unregister it from the
                            // socketmap.
                            let now_closed = if !already_closed {
                                do_send_inner(&id, conn, &addr, timer, core_ctx, bindings_ctx);
                                let now_closed = matches!(conn.state, State::Closed(_));
                                if now_closed {
                                    core_ctx.with_demux_mut(|DemuxState { socketmap }| {
                                        socketmap
                                            .conns_mut()
                                            .remove(demux_id, addr)
                                            .expect("failed to remove from socketmap");
                                    });
                                    let _: Option<_> = bindings_ctx.cancel_timer2(timer);
                                }
                                now_closed
                            } else {
                                true
                            };
                            if now_closed {
                                debug_assert!(
                                    core_ctx.with_demux_mut(|DemuxState { socketmap }| {
                                        socketmap.conns_mut().entry(demux_id, addr).is_none()
                                    }),
                                    "lingering state in socketmap: demux_id: {:?}, addr: {:?}",
                                    demux_id,
                                    addr,
                                );
                                debug_assert_eq!(
                                    bindings_ctx.scheduled_instant2(timer),
                                    None,
                                    "lingering timer for {:?}",
                                    id,
                                )
                            };
                            now_closed
                        }
                        let closed = match core_ctx {
                            MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                                let (conn, addr) = converter.convert(conn);
                                do_close(
                                    core_ctx,
                                    bindings_ctx,
                                    &id,
                                    &I::into_demux_socket_id(id.clone()),
                                    conn,
                                    addr,
                                    timer,
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
                                        timer,
                                    ),
                                    EitherStack::OtherStack((conn, addr)) => do_close(
                                        core_ctx,
                                        bindings_ctx,
                                        &id,
                                        &core_ctx.into_other_demux_socket_id(id.clone()),
                                        conn,
                                        addr,
                                        timer,
                                    ),
                                }
                            }
                        };
                        (closed, None)
                    }
                }
            });

        close_pending_sockets(core_ctx, bindings_ctx, pending.into_iter().flatten());

        if destroy {
            destroy_socket(core_ctx, bindings_ctx, id);
        }
    }

    /// Shuts down a socket.
    ///
    /// For a connection, calling this function signals the other side of the
    /// connection that we will not be sending anything over the connection; The
    /// connection will be removed from the socketmap if the state moves to the
    /// `Closed` state.
    ///
    /// For a Listener, calling this function brings it back to bound state and
    /// shutdowns all the connection that is currently ready to be accepted.
    ///
    /// Returns Err(NoConnection) if the shutdown option does not apply.
    /// Otherwise, Whether a connection has been shutdown is returned, i.e., if
    /// the socket was a listener, the operation will succeed but false will be
    /// returned.
    pub fn shutdown(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        shutdown: ShutdownType,
    ) -> Result<bool, NoConnection> {
        let (core_ctx, bindings_ctx) = self.contexts();
        let (shutdown_send, shutdown_receive) = shutdown.to_send_receive();
        let (result, pending) =
            core_ctx.with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
                let TcpSocketState { socket_state, ip_options: _ } = socket_state;
                match socket_state {
                    TcpSocketStateInner::Unbound(_) => Err(NoConnection),
                    TcpSocketStateInner::Bound(BoundSocketState::Connected {
                        conn,
                        sharing: _,
                        timer,
                    }) => {
                        if !shutdown_send {
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
                            timer: &mut BC::Timer,
                        ) -> Result<(), NoConnection>
                        where
                            SockI: DualStackIpExt,
                            WireI: DualStackIpExt,
                            BC: TcpBindingsContext<SockI, CC::WeakDeviceId>,
                            CC: TransportIpContext<WireI, BC>
                                + TcpDemuxContext<WireI, CC::WeakDeviceId, BC>
                                + CounterContext<TcpCounters<SockI>>,
                        {
                            match core_ctx.with_counters(|counters| {
                                conn.state.close(
                                    counters,
                                    CloseReason::Shutdown,
                                    &conn.socket_options,
                                )
                            }) {
                                Ok(()) => {
                                    do_send_inner(id, conn, addr, timer, core_ctx, bindings_ctx);
                                    Ok(())
                                }
                                Err(CloseError::NoConnection) => Err(NoConnection),
                                Err(CloseError::Closing) => Ok(()),
                            }
                        }
                        match core_ctx {
                            MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                                let (conn, addr) = converter.convert(conn);
                                do_shutdown(core_ctx, bindings_ctx, id, conn, addr, timer)?
                            }
                            MaybeDualStack::DualStack((core_ctx, converter)) => {
                                match converter.convert(conn) {
                                    EitherStack::ThisStack((conn, addr)) => {
                                        do_shutdown(core_ctx, bindings_ctx, id, conn, addr, timer)?
                                    }
                                    EitherStack::OtherStack((conn, addr)) => {
                                        do_shutdown(core_ctx, bindings_ctx, id, conn, addr, timer)?
                                    }
                                }
                            }
                        };
                        Ok((true, None))
                    }
                    TcpSocketStateInner::Bound(BoundSocketState::Listener((
                        maybe_listener,
                        sharing,
                        addr,
                    ))) => {
                        if !shutdown_receive {
                            return Ok((false, None));
                        }
                        match maybe_listener {
                            MaybeListener::Bound(_) => return Err(NoConnection),
                            MaybeListener::Listener(_) => {}
                        }

                        let new_sharing = {
                            let ListenerSharingState { sharing, listening } = sharing;
                            assert!(*listening, "listener {id:?} is not listening");
                            ListenerSharingState { listening: false, sharing: sharing.clone() }
                        };
                        *sharing = try_update_listener_sharing::<_, C::CoreContext, _>(
                            core_ctx,
                            id,
                            addr.clone(),
                            sharing,
                            new_sharing,
                        )
                        .unwrap_or_else(|e| {
                            unreachable!(
                                "downgrading a TCP listener to bound should not fail, got {e:?}"
                            )
                        });

                        let queued_items =
                            replace_with::replace_with_and(maybe_listener, |maybe_listener| {
                                let Listener {
                                    backlog: _,
                                    accept_queue,
                                    buffer_sizes,
                                    socket_options,
                                } = assert_matches!(maybe_listener,
                            MaybeListener::Listener(l) => l, "must be a listener");
                                let (pending, socket_extra) = accept_queue.close();
                                let bound_state =
                                    BoundState { buffer_sizes, socket_options, socket_extra };
                                (MaybeListener::Bound(bound_state), pending)
                            });

                        Ok((false, Some(queued_items)))
                    }
                }
            })?;

        close_pending_sockets(core_ctx, bindings_ctx, pending.into_iter().flatten());

        Ok(result)
    }

    fn set_device_conn<WireI, CC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut C::BindingsContext,
        addr: &mut ConnAddr<ConnIpAddr<WireI::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
        demux_id: &WireI::DemuxSocketId<CC::WeakDeviceId, C::BindingsContext>,
        ip_sock: &mut IpSock<WireI, CC::WeakDeviceId, DefaultSendOptions>,
        new_device: Option<CC::DeviceId>,
    ) -> Result<(), SetDeviceError>
    where
        WireI: DualStackIpExt,
        CC: TransportIpContext<WireI, C::BindingsContext>
            + TcpDemuxContext<WireI, CC::WeakDeviceId, C::BindingsContext>,
    {
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
            .map_err(|_: (IpSockCreationError, DefaultSendOptions)| SetDeviceError::Unroutable)?;
        core_ctx.with_demux_mut(|DemuxState { socketmap }| {
            let entry = socketmap
                .conns_mut()
                .entry(demux_id, addr)
                .unwrap_or_else(|| panic!("invalid listener ID {:?}", demux_id));
            match entry
                .try_update_addr(ConnAddr { device: new_socket.device().cloned(), ..addr.clone() })
            {
                Ok(entry) => {
                    *addr = entry.get_addr().clone();
                    *ip_sock = new_socket;
                    Ok(())
                }
                Err((ExistsError, _entry)) => Err(SetDeviceError::Conflict),
            }
        })
    }

    /// Updates the `old_device` to the new device if it is allowed. Note that
    /// this `old_device` will be updated in-place, so it should come from the
    /// outside socketmap address.
    fn set_device_listener<WireI, D>(
        demux_id: &WireI::DemuxSocketId<D, C::BindingsContext>,
        ip_addr: ListenerIpAddr<WireI::Addr, NonZeroU16>,
        old_device: &mut Option<D>,
        new_device: Option<&D::Strong>,
        weak_device: Option<D>,
        DemuxState { socketmap }: &mut DemuxState<WireI, D, C::BindingsContext>,
    ) -> Result<(), SetDeviceError>
    where
        WireI: DualStackIpExt,
        D: device::WeakId,
    {
        let entry = socketmap
            .listeners_mut()
            .entry(demux_id, &ListenerAddr { ip: ip_addr, device: old_device.clone() })
            .expect("invalid ID");

        if !crate::socket::can_device_change(
            ip_addr.addr.as_ref().map(|a| a.as_ref()), /* local_ip */
            None,                                      /* remote_ip */
            old_device.as_ref(),
            new_device,
        ) {
            return Err(SetDeviceError::ZoneChange);
        }

        match entry.try_update_addr(ListenerAddr { device: weak_device, ip: ip_addr }) {
            Ok(entry) => {
                *old_device = entry.get_addr().device.clone();
                Ok(())
            }
            Err((ExistsError, _entry)) => Err(SetDeviceError::Conflict),
        }
    }

    /// Sets the device on a socket.
    ///
    /// Passing `None` clears the bound device.
    pub fn set_device(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        new_device: Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    ) -> Result<(), SetDeviceError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        let weak_device = new_device.as_ref().map(|d| core_ctx.downgrade_device_id(d));
        core_ctx.with_socket_mut_transport_demux(id, move |core_ctx, socket_state| {
            debug!("set device on {id:?} to {new_device:?}");
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            match socket_state {
                TcpSocketStateInner::Unbound(unbound) => {
                    unbound.bound_device = weak_device;
                    Ok(())
                }
                TcpSocketStateInner::Bound(BoundSocketState::Connected {
                    conn: conn_and_addr,
                    sharing: _,
                    timer: _,
                }) => {
                    let this_or_other_stack = match core_ctx {
                        MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                            let (conn, addr) = converter.convert(conn_and_addr);
                            EitherStack::ThisStack((
                                core_ctx.as_this_stack(),
                                &mut conn.ip_sock,
                                addr,
                                I::into_demux_socket_id(id.clone()),
                            ))
                        }
                        MaybeDualStack::DualStack((core_ctx, converter)) => {
                            match converter.convert(conn_and_addr) {
                                EitherStack::ThisStack((conn, addr)) => EitherStack::ThisStack((
                                    core_ctx.as_this_stack(),
                                    &mut conn.ip_sock,
                                    addr,
                                    I::into_demux_socket_id(id.clone()),
                                )),
                                EitherStack::OtherStack((conn, addr)) => {
                                    let demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                                    EitherStack::OtherStack((
                                        core_ctx,
                                        &mut conn.ip_sock,
                                        addr,
                                        demux_id,
                                    ))
                                }
                            }
                        }
                    };
                    match this_or_other_stack {
                        EitherStack::ThisStack((core_ctx, ip_sock, addr, demux_id)) => {
                            Self::set_device_conn::<I, _>(
                                core_ctx,
                                bindings_ctx,
                                addr,
                                &demux_id,
                                ip_sock,
                                new_device,
                            )
                        }
                        EitherStack::OtherStack((core_ctx, ip_sock, addr, demux_id)) => {
                            Self::set_device_conn::<I::OtherVersion, _>(
                                core_ctx,
                                bindings_ctx,
                                addr,
                                &demux_id,
                                ip_sock,
                                new_device,
                            )
                        }
                    }
                }
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    _listener,
                    _sharing,
                    addr,
                ))) => {
                    let new_device = new_device.as_ref();
                    match core_ctx {
                        MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                            let ListenerAddr { ip, device } = converter.convert(addr);
                            core_ctx.with_demux_mut(|demux| {
                                Self::set_device_listener(
                                    &I::into_demux_socket_id(id.clone()),
                                    ip.clone(),
                                    device,
                                    new_device,
                                    weak_device,
                                    demux,
                                )
                            })
                        }
                        MaybeDualStack::DualStack((core_ctx, converter)) => {
                            match converter.convert(addr) {
                                ListenerAddr {
                                    ip: DualStackListenerIpAddr::ThisStack(ip),
                                    device,
                                } => {
                                    TcpDemuxContext::<I, _, _>::with_demux_mut(core_ctx, |demux| {
                                        Self::set_device_listener(
                                            &I::into_demux_socket_id(id.clone()),
                                            ip.clone(),
                                            device,
                                            new_device,
                                            weak_device,
                                            demux,
                                        )
                                    })
                                }
                                ListenerAddr {
                                    ip: DualStackListenerIpAddr::OtherStack(ip),
                                    device,
                                } => {
                                    let other_demux_id =
                                        core_ctx.into_other_demux_socket_id(id.clone());
                                    TcpDemuxContext::<I::OtherVersion, _, _>::with_demux_mut(
                                        core_ctx,
                                        |demux| {
                                            Self::set_device_listener(
                                                &other_demux_id,
                                                ip.clone(),
                                                device,
                                                new_device,
                                                weak_device,
                                                demux,
                                            )
                                        },
                                    )
                                }
                                ListenerAddr {
                                    ip: DualStackListenerIpAddr::BothStacks(port),
                                    device,
                                } => {
                                    let other_demux_id =
                                        core_ctx.into_other_demux_socket_id(id.clone());
                                    let old_device = device
                                        .as_ref()
                                        .and_then(|d| core_ctx.upgrade_weak_device_id(d));
                                    let old_weak_device = device.clone();
                                    core_ctx.with_both_demux_mut(|demux, other_demux| {
                                        Self::set_device_listener(
                                            &I::into_demux_socket_id(id.clone()),
                                            ListenerIpAddr { addr: None, identifier: *port },
                                            device,
                                            new_device.clone(),
                                            weak_device.clone(),
                                            demux,
                                        )?;
                                        match Self::set_device_listener(
                                            &other_demux_id,
                                            ListenerIpAddr { addr: None, identifier: *port },
                                            device,
                                            new_device,
                                            weak_device,
                                            other_demux,
                                        ) {
                                            Ok(()) => Ok(()),
                                            Err(e) => {
                                                Self::set_device_listener(
                                                    &I::into_demux_socket_id(id.clone()),
                                                    ListenerIpAddr {
                                                        addr: None,
                                                        identifier: *port,
                                                    },
                                                    device,
                                                    old_device.as_ref(),
                                                    old_weak_device,
                                                    demux,
                                                )
                                                .expect("failed to revert back the device setting");
                                                Err(e)
                                            }
                                        }
                                    })
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    /// Get information for a TCP socket.
    pub fn get_info(
        &mut self,
        id: &TcpApiSocketId<I, C>,
    ) -> SocketInfo<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId> {
        self.core_ctx().with_socket_and_converter(
            id,
            |TcpSocketState { socket_state, ip_options: _ }, _converter| match socket_state {
                TcpSocketStateInner::Unbound(unbound) => SocketInfo::Unbound(unbound.into()),
                TcpSocketStateInner::Bound(BoundSocketState::Connected {
                    conn: conn_and_addr,
                    sharing: _,
                    timer: _,
                }) => SocketInfo::Connection(I::get_conn_info(conn_and_addr)),
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    _listener,
                    _sharing,
                    addr,
                ))) => SocketInfo::Bound(I::get_bound_info(addr)),
            },
        )
    }

    /// Call this function whenever a socket can push out more data. That means
    /// either:
    ///
    /// - A retransmission timer fires.
    /// - An ack received from peer so that our send window is enlarged.
    /// - The user puts data into the buffer and we are notified.
    pub fn do_send(&mut self, conn_id: &TcpApiSocketId<I, C>) {
        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx.with_socket_mut_transport_demux(conn_id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            let (conn, timer) = assert_matches!(
                socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Connected {
                    conn, sharing: _, timer
                }) => (conn, timer)
            );
            match core_ctx {
                MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                    let (conn, addr) = converter.convert(conn);
                    do_send_inner(conn_id, conn, addr, timer, core_ctx, bindings_ctx);
                }
                MaybeDualStack::DualStack((core_ctx, converter)) => match converter.convert(conn) {
                    EitherStack::ThisStack((conn, addr)) => {
                        do_send_inner(conn_id, conn, addr, timer, core_ctx, bindings_ctx)
                    }
                    EitherStack::OtherStack((conn, addr)) => {
                        do_send_inner(conn_id, conn, addr, timer, core_ctx, bindings_ctx)
                    }
                },
            }
        })
    }

    fn handle_timer(
        &mut self,
        weak_id: WeakTcpSocketId<
            I,
            <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
            C::BindingsContext,
        >,
    ) {
        let id = match weak_id.upgrade() {
            Some(c) => c,
            None => return,
        };
        let (core_ctx, bindings_ctx) = self.contexts();
        // Alias refs so we can move weak_id to the closure.
        let id_alias = &id;
        let bindings_ctx_alias = &mut *bindings_ctx;
        let closed_and_defunct =
            core_ctx.with_socket_mut_transport_demux(&id, move |core_ctx, socket_state| {
                let TcpSocketState { socket_state, ip_options: _ } = socket_state;
                let id = id_alias;
                let bindings_ctx = bindings_ctx_alias;
                let (conn, timer) = assert_matches!(
                    socket_state,
                    TcpSocketStateInner::Bound(BoundSocketState::Connected{ conn, sharing: _, timer}) => (conn, timer)
                );
                fn do_handle_timer<SockI, WireI, CC, BC>(
                    core_ctx: &mut CC,
                    bindings_ctx: &mut BC,
                    id: &TcpSocketId<SockI, CC::WeakDeviceId, BC>,
                    demux_id: &WireI::DemuxSocketId<CC::WeakDeviceId, BC>,
                    conn: &mut Connection<SockI, WireI, CC::WeakDeviceId, BC>,
                    addr: &ConnAddr<
                        ConnIpAddr<<WireI as Ip>::Addr, NonZeroU16, NonZeroU16>,
                        CC::WeakDeviceId,
                    >,
                    timer: &mut BC::Timer,
                ) -> bool
                where
                    SockI: DualStackIpExt,
                    WireI: DualStackIpExt,
                    BC: TcpBindingsContext<SockI, CC::WeakDeviceId>,
                    CC: TransportIpContext<WireI, BC>
                        + TcpDemuxContext<WireI, CC::WeakDeviceId, BC>
                        + CounterContext<TcpCounters<SockI>>,
                {
                    #[derive(Eq, PartialEq)]
                    enum PrevState {
                        Closed,
                        NotClosed{ time_wait: bool }
                    }

                    let prev_state = match conn.state {
                        State::Closed(_) => PrevState::Closed,
                        State::TimeWait(_) => PrevState::NotClosed{ time_wait: true },
                        _ => PrevState::NotClosed{ time_wait: false }
                    };
                    do_send_inner(id, conn, addr, timer, core_ctx, bindings_ctx);
                    let is_now_closed = matches!(conn.state, State::Closed(_));
                    match (is_now_closed, prev_state) {
                        // Moved to closed state, remove from demux and cancel
                        // timers.
                        (true, PrevState::NotClosed{ time_wait }) => {
                            let result = core_ctx.with_demux_mut(|DemuxState { socketmap }| {
                                socketmap
                                    .conns_mut()
                                    .remove(demux_id, addr)
                            });
                            // Carve out an exception for time wait demux
                            // removal, since it could've been removed from the
                            // demux already as part of reuse.
                            //
                            // We can log rather silently because the demux will
                            // not allow us to remove the wrong connection, the
                            // panic is here to catch paths that are doing
                            // cleanup in the wrong way.
                            result.unwrap_or_else(|e| {
                                if time_wait {
                                    debug!(
                                        "raced with timewait removal for {id:?} {addr:?}: {e:?}"
                                    );
                                } else {
                                    panic!("failed to remove from socketmap: {e:?}");
                                }
                            });
                            let _: Option<_> = bindings_ctx.cancel_timer2(timer);
                        }
                        (true, PrevState::Closed) | (false, _) => (),
                    }
                    conn.defunct && is_now_closed
                }
                match core_ctx {
                    MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                        let (conn, addr) = converter.convert(conn);
                        do_handle_timer(
                            core_ctx,
                            bindings_ctx,
                            id,
                            &I::into_demux_socket_id(id.clone()),
                            conn,
                            addr,
                            timer,
                        )
                    }
                    MaybeDualStack::DualStack((core_ctx, converter)) => {
                        match converter.convert(conn) {
                            EitherStack::ThisStack((conn, addr)) => do_handle_timer(
                                core_ctx,
                                bindings_ctx,
                                id,
                                &I::into_demux_socket_id(id.clone()),
                                conn,
                                addr,
                                timer,
                            ),
                            EitherStack::OtherStack((conn, addr)) => do_handle_timer(
                                core_ctx,
                                bindings_ctx,
                                id,
                                &core_ctx.into_other_demux_socket_id(id.clone()),
                                conn,
                                addr,
                                timer,
                            ),
                        }
                    }
                }
            });
        if closed_and_defunct {
            // Remove the entry from the primary map and drop primary.
            destroy_socket(core_ctx, bindings_ctx, id);
        }
    }

    /// Access options mutably for a TCP socket.
    pub fn with_socket_options_mut<R, F: FnOnce(&mut SocketOptions) -> R>(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        f: F,
    ) -> R {
        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx.with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            match socket_state {
                TcpSocketStateInner::Unbound(unbound) => f(&mut unbound.socket_options),
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    MaybeListener::Bound(bound),
                    _,
                    _,
                ))) => f(&mut bound.socket_options),
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    MaybeListener::Listener(listener),
                    _,
                    _,
                ))) => f(&mut listener.socket_options),
                TcpSocketStateInner::Bound(BoundSocketState::Connected {
                    conn,
                    sharing: _,
                    timer,
                }) => match core_ctx {
                    MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                        let (conn, addr) = converter.convert(conn);
                        let old = conn.socket_options;
                        let result = f(&mut conn.socket_options);
                        if old != conn.socket_options {
                            do_send_inner(id, conn, &*addr, timer, core_ctx, bindings_ctx);
                        }
                        result
                    }
                    MaybeDualStack::DualStack((core_ctx, converter)) => {
                        match converter.convert(conn) {
                            EitherStack::ThisStack((conn, addr)) => {
                                let old = conn.socket_options;
                                let result = f(&mut conn.socket_options);
                                if old != conn.socket_options {
                                    do_send_inner(id, conn, &*addr, timer, core_ctx, bindings_ctx);
                                }
                                result
                            }
                            EitherStack::OtherStack((conn, addr)) => {
                                let old = conn.socket_options;
                                let result = f(&mut conn.socket_options);
                                if old != conn.socket_options {
                                    do_send_inner(id, conn, &*addr, timer, core_ctx, bindings_ctx);
                                }
                                result
                            }
                        }
                    }
                },
            }
        })
    }

    /// Access socket options immutably for a TCP socket
    pub fn with_socket_options<R, F: FnOnce(&SocketOptions) -> R>(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        f: F,
    ) -> R {
        self.core_ctx().with_socket_and_converter(
            id,
            |TcpSocketState { socket_state, ip_options: _ }, converter| match socket_state {
                TcpSocketStateInner::Unbound(unbound) => f(&unbound.socket_options),
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    MaybeListener::Bound(bound),
                    _,
                    _,
                ))) => f(&bound.socket_options),
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    MaybeListener::Listener(listener),
                    _,
                    _,
                ))) => f(&listener.socket_options),
                TcpSocketStateInner::Bound(BoundSocketState::Connected {
                    conn,
                    sharing: _,
                    timer: _,
                }) => {
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
            },
        )
    }

    /// Set the size of the send buffer for this socket and future derived
    /// sockets.
    pub fn set_send_buffer_size(&mut self, id: &TcpApiSocketId<I, C>, size: usize) {
        set_buffer_size::<SendBufferSize, I, _, _>(self.core_ctx(), id, size)
    }

    /// Get the size of the send buffer for this socket and future derived
    /// sockets.
    pub fn send_buffer_size(&mut self, id: &TcpApiSocketId<I, C>) -> Option<usize> {
        get_buffer_size::<SendBufferSize, I, _, _>(self.core_ctx(), id)
    }

    /// Set the size of the send buffer for this socket and future derived
    /// sockets.
    pub fn set_receive_buffer_size(&mut self, id: &TcpApiSocketId<I, C>, size: usize) {
        set_buffer_size::<ReceiveBufferSize, I, _, _>(self.core_ctx(), id, size)
    }

    /// Get the size of the receive buffer for this socket and future derived
    /// sockets.
    pub fn receive_buffer_size(&mut self, id: &TcpApiSocketId<I, C>) -> Option<usize> {
        get_buffer_size::<ReceiveBufferSize, I, _, _>(self.core_ctx(), id)
    }

    /// Sets the POSIX SO_REUSEADDR socket option on a socket.
    pub fn set_reuseaddr(
        &mut self,
        id: &TcpApiSocketId<I, C>,
        reuse: bool,
    ) -> Result<(), SetReuseAddrError> {
        let new_sharing = match reuse {
            true => SharingState::ReuseAddress,
            false => SharingState::Exclusive,
        };
        self.core_ctx().with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            match socket_state {
                TcpSocketStateInner::Unbound(unbound) => {
                    unbound.sharing = new_sharing;
                    Ok(())
                }
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    _listener,
                    old_sharing,
                    addr,
                ))) => {
                    if new_sharing == old_sharing.sharing {
                        return Ok(());
                    }
                    let new_sharing = {
                        let ListenerSharingState { sharing: _, listening } = old_sharing;
                        ListenerSharingState { sharing: new_sharing, listening: *listening }
                    };
                    *old_sharing = try_update_listener_sharing::<_, C::CoreContext, _>(
                        core_ctx,
                        id,
                        addr.clone(),
                        old_sharing,
                        new_sharing,
                    )
                    .map_err(|UpdateSharingError| SetReuseAddrError::AddrInUse)?;
                    Ok(())
                }
                TcpSocketStateInner::Bound(BoundSocketState::Connected { .. }) => {
                    // TODO(https://fxbug.dev/42180094): Support setting the option
                    // for connection sockets.
                    Err(SetReuseAddrError::NotSupported)
                }
            }
        })
    }

    /// Gets the POSIX SO_REUSEADDR socket option on a socket.
    pub fn reuseaddr(&mut self, id: &TcpApiSocketId<I, C>) -> bool {
        self.core_ctx().with_socket(id, |TcpSocketState { socket_state, ip_options: _ }| {
            match socket_state {
                TcpSocketStateInner::Unbound(Unbound { sharing, .. })
                | TcpSocketStateInner::Bound(
                    BoundSocketState::Connected { sharing, .. }
                    | BoundSocketState::Listener((_, ListenerSharingState { sharing, .. }, _)),
                ) => match sharing {
                    SharingState::Exclusive => false,
                    SharingState::ReuseAddress => true,
                },
            }
        })
    }

    /// Gets the `dual_stack_enabled` option value.
    pub fn dual_stack_enabled(
        &mut self,
        id: &TcpSocketId<
            I,
            <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
            C::BindingsContext,
        >,
    ) -> Result<bool, NotDualStackCapableError> {
        self.core_ctx().with_socket_mut_transport_demux(
            id,
            |core_ctx, TcpSocketState { socket_state: _, ip_options }| match core_ctx {
                MaybeDualStack::NotDualStack(_) => Err(NotDualStackCapableError),
                MaybeDualStack::DualStack((core_ctx, _converter)) => {
                    Ok(core_ctx.dual_stack_enabled(ip_options))
                }
            },
        )
    }

    /// Sets the `dual_stack_enabled` option value.
    pub fn set_dual_stack_enabled(
        &mut self,
        id: &TcpSocketId<
            I,
            <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
            C::BindingsContext,
        >,
        value: bool,
    ) -> Result<(), SetDualStackEnabledError> {
        self.core_ctx().with_socket_mut_transport_demux(id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options } = socket_state;
            match core_ctx {
                MaybeDualStack::NotDualStack(_) => Err(SetDualStackEnabledError::NotCapable),
                MaybeDualStack::DualStack((core_ctx, _converter)) => match socket_state {
                    TcpSocketStateInner::Unbound(_) => {
                        Ok(core_ctx.set_dual_stack_enabled(ip_options, value))
                    }
                    TcpSocketStateInner::Bound(_) => Err(SetDualStackEnabledError::SocketIsBound),
                },
            }
        })
    }

    fn on_icmp_error_conn(
        core_ctx: &mut C::CoreContext,
        bindings_ctx: &mut C::BindingsContext,
        id: TcpSocketId<
            I,
            <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
            C::BindingsContext,
        >,
        seq: SeqNum,
        error: IcmpErrorCode,
    ) {
        let destroy = core_ctx.with_socket_mut_transport_demux(&id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            let (conn_and_addr, timer) = assert_matches!(
                socket_state,
                TcpSocketStateInner::Bound(
                    BoundSocketState::Connected { conn, sharing: _, timer } ) => (conn, timer),
                "invalid socket ID");
            let (accept_queue, state, soft_error, handshake_status, this_or_other_stack) =
                match core_ctx {
                    MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                        let (conn, addr) = converter.convert(conn_and_addr);
                        conn.on_icmp_error(core_ctx, seq, error);
                        (
                            &mut conn.accept_queue,
                            &mut conn.state,
                            &mut conn.soft_error,
                            &mut conn.handshake_status,
                            EitherStack::ThisStack((
                                core_ctx.as_this_stack(),
                                I::into_demux_socket_id(id.clone()),
                                addr,
                            )),
                        )
                    }
                    MaybeDualStack::DualStack((core_ctx, converter)) => {
                        match converter.convert(conn_and_addr) {
                            EitherStack::ThisStack((conn, addr)) => {
                                conn.on_icmp_error(core_ctx, seq, error);
                                (
                                    &mut conn.accept_queue,
                                    &mut conn.state,
                                    &mut conn.soft_error,
                                    &mut conn.handshake_status,
                                    EitherStack::ThisStack((
                                        core_ctx.as_this_stack(),
                                        I::into_demux_socket_id(id.clone()),
                                        addr,
                                    )),
                                )
                            }
                            EitherStack::OtherStack((conn, addr)) => {
                                conn.on_icmp_error(core_ctx, seq, error);
                                let demux_id = core_ctx.into_other_demux_socket_id(id.clone());
                                (
                                    &mut conn.accept_queue,
                                    &mut conn.state,
                                    &mut conn.soft_error,
                                    &mut conn.handshake_status,
                                    EitherStack::OtherStack((core_ctx, demux_id, addr)),
                                )
                            }
                        }
                    }
                };

            if let State::Closed(Closed { reason }) = state {
                debug!("handshake_status: {handshake_status:?}");
                let _: bool = handshake_status.update_if_pending(HandshakeStatus::Aborted);
                // Always unregister the socket from the socketmap.
                match this_or_other_stack {
                    EitherStack::ThisStack((core_ctx, demux_id, addr)) => {
                        TcpDemuxContext::<I, _, _>::with_demux_mut(
                            core_ctx,
                            |DemuxState { socketmap }| {
                                socketmap
                                    .conns_mut()
                                    .remove(&demux_id, addr)
                                    .expect("failed to remove from socketmap");
                            },
                        );
                    }
                    EitherStack::OtherStack((core_ctx, demux_id, addr)) => {
                        TcpDemuxContext::<I::OtherVersion, _, _>::with_demux_mut(
                            core_ctx,
                            |DemuxState { socketmap }| {
                                socketmap
                                    .conns_mut()
                                    .remove(&demux_id, addr)
                                    .expect("failed to remove from socketmap");
                            },
                        );
                    }
                };
                let _: Option<_> = bindings_ctx.cancel_timer2(timer);
                match accept_queue {
                    Some(accept_queue) => {
                        accept_queue.remove(&id);
                        // destroy the socket if not held by the user.
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
            destroy_socket(core_ctx, bindings_ctx, id);
        }
    }

    fn on_icmp_error(
        &mut self,
        orig_src_ip: SpecifiedAddr<I::Addr>,
        orig_dst_ip: SpecifiedAddr<I::Addr>,
        orig_src_port: NonZeroU16,
        orig_dst_port: NonZeroU16,
        seq: SeqNum,
        error: IcmpErrorCode,
    ) where
        C::CoreContext: TcpContext<I::OtherVersion, C::BindingsContext>
            + CounterContext<TcpCounters<I::OtherVersion>>,
        C::BindingsContext: TcpBindingsContext<
            I::OtherVersion,
            <C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
        >,
    {
        let (core_ctx, bindings_ctx) = self.contexts();

        let orig_src_ip = match SocketIpAddr::try_from(orig_src_ip) {
            Ok(ip) => ip,
            Err(AddrIsMappedError {}) => {
                trace!("ignoring ICMP error from IPv4-mapped-IPv6 source: {}", orig_src_ip);
                return;
            }
        };
        let orig_dst_ip = match SocketIpAddr::try_from(orig_dst_ip) {
            Ok(ip) => ip,
            Err(AddrIsMappedError {}) => {
                trace!("ignoring ICMP error to IPv4-mapped-IPv6 destination: {}", orig_dst_ip);
                return;
            }
        };

        let id = TcpDemuxContext::<I, _, _>::with_demux(core_ctx, |DemuxState { socketmap }| {
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

        match I::into_dual_stack_ip_socket(id) {
            EitherStack::ThisStack(id) => {
                Self::on_icmp_error_conn(core_ctx, bindings_ctx, id, seq, error)
            }
            EitherStack::OtherStack(id) => TcpApi::<I::OtherVersion, C>::on_icmp_error_conn(
                core_ctx,
                bindings_ctx,
                id,
                seq,
                error,
            ),
        };
    }

    /// Gets the last error on the connection.
    pub fn get_socket_error(&mut self, id: &TcpApiSocketId<I, C>) -> Option<ConnectionError> {
        self.core_ctx().with_socket_mut_and_converter(id, |socket_state, converter| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            match socket_state {
                TcpSocketStateInner::Unbound(_)
                | TcpSocketStateInner::Bound(BoundSocketState::Listener(_)) => None,
                TcpSocketStateInner::Bound(BoundSocketState::Connected {
                    conn,
                    sharing: _,
                    timer: _,
                }) => {
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
            }
        })
    }

    /// Provides access to shared and per-socket TCP stats via a visitor.
    pub fn inspect<N>(&mut self, inspector: &mut N)
    where
        N: Inspector
            + InspectorDeviceExt<<C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId>,
    {
        self.core_ctx().for_each_socket(|socket_id, socket_state| {
            inspector.record_debug_child(socket_id, |node| {
                node.record_str("TransportProtocol", "TCP");
                node.record_str(
                    "NetworkProtocol",
                    match I::VERSION {
                        IpVersion::V4 => "IPv4",
                        IpVersion::V6 => "IPv6",
                    },
                );
                let TcpSocketState { socket_state, ip_options: _ } = socket_state;
                match socket_state {
                    TcpSocketStateInner::Unbound(_) => {
                        node.record_local_socket_addr::<N, I::Addr, _, NonZeroU16>(None);
                        node.record_remote_socket_addr::<N, I::Addr, _, NonZeroU16>(None);
                    }
                    TcpSocketStateInner::Bound(BoundSocketState::Listener((
                        state,
                        _sharing,
                        addr,
                    ))) => {
                        let BoundInfo { addr, port, device } = I::get_bound_info(addr);
                        let local = addr.map_or_else(
                            || ZonedAddr::Unzoned(I::UNSPECIFIED_ADDRESS),
                            |addr| maybe_zoned(addr.addr(), &device).into(),
                        );
                        node.record_local_socket_addr::<N, _, _, _>(Some((local, port)));
                        node.record_remote_socket_addr::<N, I::Addr, _, NonZeroU16>(None);
                        match state {
                            MaybeListener::Bound(_bound_state) => {}
                            MaybeListener::Listener(Listener { accept_queue, backlog, .. }) => node
                                .record_child("AcceptQueue", |node| {
                                    node.record_usize("BacklogSize", *backlog);
                                    accept_queue.inspect(node);
                                }),
                        };
                    }
                    TcpSocketStateInner::Bound(BoundSocketState::Connected {
                        conn: conn_and_addr,
                        ..
                    }) => {
                        if I::get_defunct(conn_and_addr) {
                            return;
                        }
                        let state = I::get_state(conn_and_addr);
                        let ConnectionInfo {
                            local_addr: SocketAddr { ip: local_ip, port: local_port },
                            remote_addr: SocketAddr { ip: remote_ip, port: remote_port },
                            device: _,
                        } = I::get_conn_info(conn_and_addr);
                        node.record_local_socket_addr::<N, I::Addr, _, _>(Some((
                            local_ip.into(),
                            local_port,
                        )));
                        node.record_remote_socket_addr::<N, I::Addr, _, _>(Some((
                            remote_ip.into(),
                            remote_port,
                        )));
                        node.record_display("State", state);
                    }
                }
            });
        })
    }
}

/// Destroys the socket with `id`.
fn destroy_socket<
    I: DualStackIpExt,
    CC: TcpContext<I, BC>,
    BC: TcpBindingsContext<I, CC::WeakDeviceId>,
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
    BC: TcpBindingsContext<I, CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
{
    for conn_id in pending {
        core_ctx.with_socket_mut_transport_demux(&conn_id, |core_ctx, socket_state| {
            let TcpSocketState { socket_state, ip_options: _ } = socket_state;
            let (conn_and_addr, timer) = assert_matches!(
                socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Connected{
                    conn, sharing: _, timer
                }) => (conn, timer),
                "invalid socket ID"
            );
            let _: Option<BC::Instant> = bindings_ctx.cancel_timer2(timer);
            let this_or_other_stack = match core_ctx {
                MaybeDualStack::NotDualStack((core_ctx, converter)) => {
                    let (conn, addr) = converter.convert(conn_and_addr);
                    EitherStack::ThisStack((
                        core_ctx.as_this_stack(),
                        I::into_demux_socket_id(conn_id.clone()),
                        &mut conn.state,
                        &conn.ip_sock,
                        addr.clone(),
                    ))
                }
                MaybeDualStack::DualStack((core_ctx, converter)) => match converter
                    .convert(conn_and_addr)
                {
                    EitherStack::ThisStack((conn, addr)) => EitherStack::ThisStack((
                        core_ctx.as_this_stack(),
                        I::into_demux_socket_id(conn_id.clone()),
                        &mut conn.state,
                        &conn.ip_sock,
                        addr.clone(),
                    )),
                    EitherStack::OtherStack((conn, addr)) => {
                        let other_demux_id = core_ctx.into_other_demux_socket_id(conn_id.clone());
                        EitherStack::OtherStack((
                            core_ctx,
                            other_demux_id,
                            &mut conn.state,
                            &conn.ip_sock,
                            addr.clone(),
                        ))
                    }
                },
            };

            match this_or_other_stack {
                EitherStack::ThisStack((core_ctx, demux_id, state, ip_sock, conn_addr)) => {
                    close_pending_socket(
                        core_ctx,
                        bindings_ctx,
                        &conn_id,
                        &demux_id,
                        timer,
                        state,
                        ip_sock,
                        &conn_addr,
                    )
                }
                EitherStack::OtherStack((core_ctx, demux_id, state, ip_sock, conn_addr)) => {
                    close_pending_socket(
                        core_ctx,
                        bindings_ctx,
                        &conn_id,
                        &demux_id,
                        timer,
                        state,
                        ip_sock,
                        &conn_addr,
                    )
                }
            }
        });
        destroy_socket(core_ctx, bindings_ctx, conn_id);
    }
}

fn close_pending_socket<WireI, SockI, DC, BC>(
    core_ctx: &mut DC,
    bindings_ctx: &mut BC,
    sock_id: &TcpSocketId<SockI, DC::WeakDeviceId, BC>,
    demux_id: &WireI::DemuxSocketId<DC::WeakDeviceId, BC>,
    timer: &mut BC::Timer,
    state: &mut State<
        BC::Instant,
        BC::ReceiveBuffer,
        BC::SendBuffer,
        BC::ListenerNotifierOrProvidedBuffers,
    >,
    ip_sock: &IpSock<WireI, DC::WeakDeviceId, DefaultSendOptions>,
    conn_addr: &ConnAddr<ConnIpAddr<WireI::Addr, NonZeroU16, NonZeroU16>, DC::WeakDeviceId>,
) where
    WireI: DualStackIpExt,
    SockI: DualStackIpExt,
    DC: TransportIpContext<WireI, BC>
        + DeviceIpSocketHandler<WireI, BC>
        + TcpDemuxContext<WireI, DC::WeakDeviceId, BC>
        + CounterContext<TcpCounters<SockI>>,
    BC: TcpBindingsContext<SockI, DC::WeakDeviceId>,
{
    if !matches!(state, State::Closed(_)) {
        core_ctx.with_demux_mut(|DemuxState { socketmap }| {
            socketmap
                .conns_mut()
                .remove(demux_id, conn_addr)
                .expect("failed to remove from socketmap");
        });
        let _: Option<_> = bindings_ctx.cancel_timer2(timer);
    }

    if let Some(reset) = core_ctx.with_counters(|counters| state.abort(counters)) {
        let ConnAddr { ip, device: _ } = conn_addr;
        send_tcp_segment(core_ctx, bindings_ctx, Some(sock_id), Some(ip_sock), *ip, reset.into());
    }
}

fn do_send_inner<SockI, WireI, CC, BC>(
    conn_id: &TcpSocketId<SockI, CC::WeakDeviceId, BC>,
    conn: &mut Connection<SockI, WireI, CC::WeakDeviceId, BC>,
    addr: &ConnAddr<ConnIpAddr<WireI::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
    timer: &mut BC::Timer,
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
) where
    SockI: DualStackIpExt,
    WireI: DualStackIpExt,
    BC: TcpBindingsContext<SockI, CC::WeakDeviceId>,
    CC: TransportIpContext<WireI, BC> + CounterContext<TcpCounters<SockI>>,
{
    while let Some(seg) = core_ctx.with_counters(|counters| {
        conn.state.poll_send(counters, u32::MAX, bindings_ctx.now(), &conn.socket_options)
    }) {
        send_tcp_segment(
            core_ctx,
            bindings_ctx,
            Some(conn_id),
            Some(&conn.ip_sock),
            addr.ip.clone(),
            seg,
        );
    }

    if let Some(instant) = conn.state.poll_send_at() {
        let _: Option<_> = bindings_ctx.schedule_timer_instant2(instant, timer);
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
    BC: TcpBindingsContext<I, CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
>(
    core_ctx: &mut CC,
    id: &TcpSocketId<I, CC::WeakDeviceId, BC>,
    size: usize,
) {
    core_ctx.with_socket_mut_and_converter(
        id,
        |TcpSocketState { socket_state, ip_options: _ }, converter| match socket_state {
            TcpSocketStateInner::Unbound(Unbound { buffer_sizes, .. }) => {
                Which::set_unconnected_size(buffer_sizes, size)
            }
            TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
            TcpSocketStateInner::Bound(BoundSocketState::Listener((
                MaybeListener::Listener(Listener { buffer_sizes, .. })
                | MaybeListener::Bound(BoundState { buffer_sizes, .. }),
                _,
                _,
            ))) => Which::set_unconnected_size(buffer_sizes, size),
        },
    )
}

fn get_buffer_size<
    Which: AccessBufferSize,
    I: DualStackIpExt,
    BC: TcpBindingsContext<I, CC::WeakDeviceId>,
    CC: TcpContext<I, BC>,
>(
    core_ctx: &mut CC,
    id: &TcpSocketId<I, CC::WeakDeviceId, BC>,
) -> Option<usize> {
    core_ctx.with_socket_and_converter(id, |socket_state, converter| {
        let TcpSocketState { socket_state, ip_options: _ } = socket_state;
        let sizes = match socket_state {
            TcpSocketStateInner::Unbound(Unbound { buffer_sizes, .. }) => {
                buffer_sizes.into_optional()
            }
            TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
            TcpSocketStateInner::Bound(BoundSocketState::Listener((maybe_listener, _, _))) => {
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
    /// The socket cannot bind to the local address.
    #[error(transparent)]
    LocalAddressError(#[from] LocalAddressError),
}

fn connect_inner<CC, BC, SockI, WireI>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    sock_id: &TcpSocketId<SockI, CC::WeakDeviceId, BC>,
    demux_id: &WireI::DemuxSocketId<CC::WeakDeviceId, BC>,
    isn: &IsnGenerator<BC::Instant>,
    listener_addr: Option<ListenerAddr<ListenerIpAddr<WireI::Addr, NonZeroU16>, CC::WeakDeviceId>>,
    remote_ip: ZonedAddr<SocketIpAddr<WireI::Addr>, CC::DeviceId>,
    remote_port: NonZeroU16,
    netstack_buffers: BC::ListenerNotifierOrProvidedBuffers,
    buffer_sizes: BufferSizes,
    socket_options: SocketOptions,
    sharing: SharingState,
    DemuxState { socketmap }: &mut DemuxState<WireI, CC::WeakDeviceId, BC>,
    remove_op: impl FnOnce(
        &mut BoundSocketMap<WireI, CC::WeakDeviceId, BC>,
        &WireI::DemuxSocketId<CC::WeakDeviceId, BC>,
        &ListenerAddr<ListenerIpAddr<WireI::Addr, NonZeroU16>, CC::WeakDeviceId>,
    ),
    convert_back_op: impl FnOnce(
        Connection<SockI, WireI, CC::WeakDeviceId, BC>,
        ConnAddr<ConnIpAddr<WireI::Addr, NonZeroU16, NonZeroU16>, CC::WeakDeviceId>,
    ) -> SockI::ConnectionAndAddr<CC::WeakDeviceId, BC>,
    convert_timer: impl FnOnce(WeakTcpSocketId<SockI, CC::WeakDeviceId, BC>) -> BC::DispatchId,
    socket_state: &mut TcpSocketStateInner<SockI, CC::WeakDeviceId, BC>,
) -> Result<(), ConnectError>
where
    SockI: DualStackIpExt,
    WireI: DualStackIpExt,
    BC: TcpBindingsContext<SockI, CC::WeakDeviceId>,
    CC: TransportIpContext<WireI, BC>
        + DeviceIpSocketHandler<WireI, BC>
        + CounterContext<TcpCounters<SockI>>,
{
    let local_ip = listener_addr.as_ref().and_then(|la| la.ip.addr);
    let bound_device = listener_addr.as_ref().and_then(|la| la.device.clone());
    let local_port = listener_addr.as_ref().map(|la| la.ip.identifier);
    let (remote_ip, device) = crate::transport::resolve_addr_with_device::<WireI::Addr, _, _, _>(
        remote_ip,
        bound_device,
    )?;

    let ip_sock = core_ctx
        .new_ip_socket(
            bindings_ctx,
            device.as_ref().map(|d| d.as_ref()),
            local_ip,
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

    let local_port = local_port.map_or_else(
        // NB: Pass the remote port into the allocator to avoid unexpected
        // self-connections when allocating a local port. This could be
        // optimized by checking if the IP socket has resolved to local
        // delivery, but excluding a single port should be enough here and
        // avoids adding more dependencies.
        || match algorithm::simple_randomized_port_alloc(
            &mut bindings_ctx.rng(),
            &Some(*ip_sock.local_ip()),
            socketmap,
            &Some(remote_port),
        ) {
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
        .try_insert(conn_addr.clone(), sharing, demux_id.clone())
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
    if let Some(listener_addr) = &listener_addr {
        remove_op(socketmap, demux_id, listener_addr);
    }

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
    send_tcp_segment(
        core_ctx,
        bindings_ctx,
        Some(&sock_id),
        Some(&ip_sock),
        conn_addr.ip,
        syn.into(),
    );

    let mut timer = bindings_ctx.new_timer(convert_timer(sock_id.downgrade()));
    assert_eq!(bindings_ctx.schedule_timer_instant2(poll_send_at, &mut timer), None);

    let conn = convert_back_op(
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
    );
    *socket_state =
        TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, sharing, timer });

    core_ctx.increment(|counters| &counters.active_connection_openings);
    Ok(())
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
    pub addr: Option<ZonedAddr<SpecifiedAddr<A>, D>>,
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
) -> ZonedAddr<SpecifiedAddr<A>, D> {
    device
        .as_ref()
        .and_then(|device| {
            AddrAndZone::new(ip, device).map(|az| ZonedAddr::Zoned(az.map_zone(Clone::clone)))
        })
        .unwrap_or(ZonedAddr::Unzoned(ip))
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

impl<CC, BC> TimerHandler<BC, TimerId<CC::WeakDeviceId, BC>> for CC
where
    BC: TcpBindingsContext<Ipv4, CC::WeakDeviceId> + TcpBindingsContext<Ipv6, CC::WeakDeviceId>,
    CC: TcpContext<Ipv4, BC>
        + TcpContext<Ipv6, BC>
        + CounterContext<TcpCounters<Ipv4>>
        + CounterContext<TcpCounters<Ipv6>>,
{
    fn handle_timer(&mut self, bindings_ctx: &mut BC, timer_id: TimerId<CC::WeakDeviceId, BC>) {
        let ctx_pair = CtxPair { core_ctx: self, bindings_ctx };
        match timer_id {
            TimerId::V4(conn_id) => TcpApi::new(ctx_pair).handle_timer(conn_id),
            TimerId::V6(conn_id) => TcpApi::new(ctx_pair).handle_timer(conn_id),
        }
    }
}

/// Send the given TCP Segment.
///
/// A centralized send path for TCP segments that increments counters and logs
/// errors.
///
/// When `ip_sock` is some, it is used to send the segment, otherwise, one is
/// constructed on demand to send a oneshot segment.
///
/// `socket_id` is used strictly for logging. `None` can be provided in cases
/// where the segment is not associated with any particular socket.
fn send_tcp_segment<'a, WireI, SockI, CC, BC, D, O>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    socket_id: Option<&TcpSocketId<SockI, D, BC>>,
    ip_sock: Option<&IpSock<WireI, D, O>>,
    conn_addr: ConnIpAddr<WireI::Addr, NonZeroU16, NonZeroU16>,
    segment: Segment<SendPayload<'a>>,
) where
    WireI: Ip + IpExt,
    SockI: Ip + IpExt + DualStackIpExt,
    CC: CounterContext<TcpCounters<SockI>>
        + IpSocketHandler<WireI, BC, DeviceId = D::Strong, WeakDeviceId = D>,
    BC: TcpBindingsTypes,
    D: WeakId,
    O: SendOptions<WireI>,
{
    let control = segment.contents.control();
    let result = match ip_sock {
        Some(ip_sock) => {
            let body = tcp_serialize_segment(segment, conn_addr);
            core_ctx
                .send_ip_packet(bindings_ctx, ip_sock, body, None)
                .map_err(|(_serializer, err)| IpSockCreateAndSendError::Send(err))
        }
        None => {
            let ConnIpAddr { local: (local_ip, _), remote: (remote_ip, _) } = conn_addr;
            core_ctx
                .send_oneshot_ip_packet(
                    bindings_ctx,
                    None,
                    Some(local_ip),
                    remote_ip,
                    IpProto::Tcp.into(),
                    DefaultSendOptions,
                    |_addr| tcp_serialize_segment(segment, conn_addr),
                    None,
                )
                .map_err(|(err, _options)| err)
        }
    };
    match result {
        Ok(()) => {
            core_ctx.increment(|counters| &counters.segments_sent);
            match control {
                None => {}
                Some(Control::RST) => core_ctx.increment(|counters| &counters.resets_sent),
                Some(Control::SYN) => core_ctx.increment(|counters| &counters.syns_sent),
                Some(Control::FIN) => core_ctx.increment(|counters| &counters.fins_sent),
            }
        }
        Err(err) => {
            core_ctx.increment(|counters| &counters.segment_send_errors);
            match socket_id {
                Some(socket_id) => debug!("{:?}: failed to send segment: {:?}", socket_id, err),
                None => debug!("TCP: failed to send segment: {:?}, {:?}", err, segment),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, rc::Rc, string::String, sync::Arc, vec, vec::Vec};
    use core::{cell::RefCell, ffi::CStr, num::NonZeroU16, time::Duration};

    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use net_declare::net_ip_v6;
    use net_types::{
        ip::{AddrSubnet, Ip, Ipv4, Ipv6, Ipv6SourceAddr, Mtu},
        LinkLocalAddr, Witness,
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
                FakeFrameCtx, FakeInstantCtx, FakeLinkResolutionNotifier, FakeNetwork,
                FakeNetworkContext, FakeTimerCtx, InstantAndData, PendingFrameData, StepResult,
                WithFakeFrameContext, WithFakeTimerContext, WrappedFakeCoreCtx,
            },
            ContextProvider, InstantContext as _, SendFrameContext,
        },
        device::{
            link::LinkDevice,
            socket::DeviceSocketTypes,
            testutil::{FakeDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
            DeviceLayerStateTypes,
        },
        filter::FilterBindingsTypes,
        ip::{
            device::{
                nud::LinkResolutionContext,
                state::{
                    DualStackIpDeviceState, IpDeviceStateIpExt, Ipv4AddrConfig, Ipv4AddressEntry,
                    Ipv4DeviceState, Ipv6AddrConfig, Ipv6AddressEntry, Ipv6DadState,
                    Ipv6DeviceState,
                },
                IpDeviceAddr,
            },
            icmp::{IcmpIpExt, Icmpv4ErrorCode, Icmpv6ErrorCode},
            socket::testutil::FakeDualStackIpSocketCtx,
            socket::{
                testutil::{FakeDeviceConfig, FakeFilterDeviceId},
                IpSocketBindingsContext, IpSocketContext, MmsError,
            },
            testutil::DualStackSendIpPacketMeta,
            types::{ResolvedRoute, RoutableIpAddr},
            HopLimits, IpTransportContext, ResolveRouteError, SendIpPacketMeta,
        },
        sync::Mutex,
        testutil::ContextPair,
        testutil::{
            new_rng, run_with_many_seeds, set_logger_for_test, FakeCryptoRng, MonotonicIdentifier,
            TestIpExt, DEFAULT_INTERFACE_METRIC,
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
        uninstantiable::{Uninstantiable, UninstantiableWrapper},
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
        ) -> DualStackIpDeviceState<TcpBindingsCtx<FakeDeviceId>>;

        fn converter() -> MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter>;
    }

    /// This trait anchors the timer DispatchId for our context implementations
    /// that require a core converter.
    ///
    /// This is required because we implement the traits on [`TcpCoreCtx`]
    /// abstracting away the bindings types, even though they're always
    /// [`TcpBindingsCtx`].
    trait TcpTestBindingsTypes<D: device::StrongId>:
        TcpBindingsTypes<DispatchId = TimerId<D::Weak, Self>> + Sized
    {
    }

    impl<D, BT> TcpTestBindingsTypes<D> for BT
    where
        BT: TcpBindingsTypes<DispatchId = TimerId<D::Weak, Self>> + Sized,
        D: device::StrongId,
    {
    }

    struct FakeTcpState<I: TcpTestIpExt, D: FakeStrongDeviceId, BT: TcpBindingsTypes> {
        isn_generator: Rc<IsnGenerator<BT::Instant>>,
        demux: Rc<RefCell<DemuxState<I, D::Weak, BT>>>,
        // Always destroy all sockets last so the strong references in the demux
        // are gone.
        all_sockets: TcpSocketSet<I, D::Weak, BT>,
        counters: TcpCounters<I>,
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
                demux: Rc::new(RefCell::new(DemuxState { socketmap: Default::default() })),
                counters: Default::default(),
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
        FakeDualStackIpSocketCtx<D, BT>,
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

    impl<D: FakeFilterDeviceId<()>> FakeNetworkContext for TcpCtx<D> {
        type TimerId = TimerId<D::Weak, TcpBindingsCtx<D>>;
        type SendMeta = DualStackSendIpPacketMeta<D>;
        type RecvMeta = DualStackSendIpPacketMeta<D>;
        fn handle_frame(&mut self, meta: Self::RecvMeta, buffer: Buf<Vec<u8>>) {
            let Self { core_ctx, bindings_ctx } = self;
            match meta {
                DualStackSendIpPacketMeta::V4(meta) => {
                    <TcpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                        core_ctx,
                        bindings_ctx,
                        &meta.device,
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
                        &meta.device,
                        Ipv6::recv_src_addr(*meta.src_ip),
                        meta.dst_ip,
                        buffer,
                    )
                    .expect("failed to deliver bytes");
                }
            }
        }
        fn handle_timer(&mut self, timer: Self::TimerId) {
            match timer {
                TimerId::V4(id) => self.tcp_api().handle_timer(id),
                TimerId::V6(id) => self.tcp_api().handle_timer(id),
            }
        }
        fn process_queues(&mut self) -> bool {
            false
        }
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

    impl<D: FakeStrongDeviceId> ContextProvider for TcpBindingsCtx<D> {
        type Context = Self;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    impl<D: LinkDevice + FakeStrongDeviceId> LinkResolutionContext<D> for TcpBindingsCtx<D> {
        type Notifier = FakeLinkResolutionNotifier<D>;
    }

    /// Delegate implementation to internal thing.
    impl<D: FakeStrongDeviceId> TimerBindingsTypes for TcpBindingsCtx<D> {
        type DispatchId = <FakeTimerCtx<TimerId<D::Weak, Self>> as TimerBindingsTypes>::DispatchId;
        type Timer = <FakeTimerCtx<TimerId<D::Weak, Self>> as TimerBindingsTypes>::Timer;
    }

    /// Delegate implementation to internal thing.
    impl<D: FakeStrongDeviceId> TimerContext2 for TcpBindingsCtx<D> {
        fn new_timer(&mut self, id: Self::DispatchId) -> Self::Timer {
            self.timers.new_timer(id)
        }

        fn schedule_timer_instant2(
            &mut self,
            time: Self::Instant,
            timer: &mut Self::Timer,
        ) -> Option<Self::Instant> {
            self.timers.schedule_timer_instant2(time, timer)
        }

        fn cancel_timer2(&mut self, timer: &mut Self::Timer) -> Option<Self::Instant> {
            self.timers.cancel_timer2(timer)
        }

        fn scheduled_instant2(&self, timer: &mut Self::Timer) -> Option<Self::Instant> {
            self.timers.scheduled_instant2(timer)
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
        type PureIpDeviceState = ();
        type DeviceIdentifier = MonotonicIdentifier;
    }

    impl<D: FakeStrongDeviceId> TracingContext for TcpBindingsCtx<D> {
        type DurationScope = ();

        fn duration(&self, _: &'static CStr) {}
    }

    impl<D: FakeStrongDeviceId> FilterBindingsTypes for TcpBindingsCtx<D> {
        type DeviceClass = ();
    }

    impl<D: FakeStrongDeviceId> RngContext for TcpBindingsCtx<D> {
        type Rng<'a> = &'a mut FakeCryptoRng<XorShiftRng>;
        fn rng(&mut self) -> Self::Rng<'_> {
            &mut self.rng
        }
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
        BC: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
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
        D: FakeFilterDeviceId<BC::DeviceClass>,
        BC: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
        FakeDualStackIpSocketCtx<D, BC>: TransportIpContext<I, BC, DeviceId = Self::DeviceId>,
    {
        type DevicesWithAddrIter<'a> = <FakeDualStackIpSocketCtx<D, BC> as TransportIpContext<I, BC>>::DevicesWithAddrIter<'a>
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
            BC: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
        > IpSocketContext<I, BC> for TcpCoreCtx<D, BC>
    {
        fn lookup_route(
            &mut self,
            bindings_ctx: &mut BC,
            device: Option<&Self::DeviceId>,
            local_ip: Option<IpDeviceAddr<I::Addr>>,
            addr: RoutableIpAddr<I::Addr>,
        ) -> Result<ResolvedRoute<I, Self::DeviceId>, ResolveRouteError> {
            self.inner.get_mut().lookup_route(bindings_ctx, device, local_ip, addr)
        }

        fn send_ip_packet<SS>(
            &mut self,
            bindings_ctx: &mut BC,
            SendIpPacketMeta {  device, src_ip, dst_ip, broadcast, next_hop, proto, ttl, mtu }: SendIpPacketMeta<I, &Self::DeviceId, SpecifiedAddr<I::Addr>>,
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
                broadcast,
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
        D: FakeFilterDeviceId<BC::DeviceClass>,
        BC: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
    {
        type IpTransportCtx<'a> = Self;
        fn with_demux<O, F: FnOnce(&DemuxState<Ipv4, D::Weak, BC>) -> O>(&mut self, cb: F) -> O {
            cb(&self.outer.v4.demux.borrow())
        }

        fn with_demux_mut<O, F: FnOnce(&mut DemuxState<Ipv4, D::Weak, BC>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.outer.v4.demux.borrow_mut())
        }

        fn with_demux_mut_and_ip_transport_ctx<
            O,
            F: FnOnce(&mut DemuxState<Ipv4, D::Weak, BC>, &mut Self::IpTransportCtx<'_>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let demux = Rc::clone(&self.outer.v4.demux);
            let mut demux = demux.borrow_mut();
            cb(&mut *demux, self)
        }
    }

    impl<D, BC> TcpDemuxContext<Ipv6, D::Weak, BC> for TcpCoreCtx<D, BC>
    where
        D: FakeFilterDeviceId<BC::DeviceClass>,
        BC: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
    {
        type IpTransportCtx<'a> = Self;
        fn with_demux<O, F: FnOnce(&DemuxState<Ipv6, D::Weak, BC>) -> O>(&mut self, cb: F) -> O {
            cb(&self.outer.v6.demux.borrow())
        }

        fn with_demux_mut<O, F: FnOnce(&mut DemuxState<Ipv6, D::Weak, BC>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.outer.v6.demux.borrow_mut())
        }

        fn with_demux_mut_and_ip_transport_ctx<
            O,
            F: FnOnce(&mut DemuxState<Ipv6, D::Weak, BC>, &mut Self::IpTransportCtx<'_>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let demux = Rc::clone(&self.outer.v6.demux);
            let mut demux = demux.borrow_mut();
            cb(&mut *demux, self)
        }
    }

    impl<I, D, BT> CoreTimerContext<WeakTcpSocketId<I, D::Weak, BT>, BT> for TcpCoreCtx<D, BT>
    where
        I: DualStackIpExt,
        D: FakeStrongDeviceId,
        BT: TcpTestBindingsTypes<D>,
    {
        fn convert_timer(dispatch_id: WeakTcpSocketId<I, D::Weak, BT>) -> BT::DispatchId {
            dispatch_id.into()
        }
    }

    impl<
            D: FakeFilterDeviceId<BC::DeviceClass>,
            BC: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
        > TcpContext<Ipv6, BC> for TcpCoreCtx<D, BC>
    {
        type ThisStackIpTransportAndDemuxCtx<'a> = Self;
        type SingleStackIpTransportAndDemuxCtx<'a> = UninstantiableWrapper<Self>;
        type SingleStackConverter = Uninstantiable;
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
                &TcpSocketId<Ipv6, Self::WeakDeviceId, BC>,
                &TcpSocketState<Ipv6, Self::WeakDeviceId, BC>,
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

    impl<
            D: FakeFilterDeviceId<BC::DeviceClass>,
            BC: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
        > TcpContext<Ipv4, BC> for TcpCoreCtx<D, BC>
    {
        type ThisStackIpTransportAndDemuxCtx<'a> = Self;
        type SingleStackIpTransportAndDemuxCtx<'a> = Self;
        type SingleStackConverter = ();
        type DualStackIpTransportAndDemuxCtx<'a> = UninstantiableWrapper<Self>;
        type DualStackConverter = Uninstantiable;
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
                &TcpSocketId<Ipv4, Self::WeakDeviceId, BC>,
                &TcpSocketState<Ipv4, Self::WeakDeviceId, BC>,
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

    impl<
            D: FakeFilterDeviceId<BT::DeviceClass>,
            BT: TcpTestBindingsTypes<D> + IpSocketBindingsContext,
        > TcpDualStackContext<Ipv6, FakeWeakDeviceId<D>, BT> for TcpCoreCtx<D, BT>
    {
        type Converter = Ipv6SocketIdToIpv4DemuxIdConverter;
        type DualStackIpTransportCtx<'a> = Self;
        fn other_demux_id_converter(&self) -> Ipv6SocketIdToIpv4DemuxIdConverter {
            Ipv6SocketIdToIpv4DemuxIdConverter
        }
        fn dual_stack_enabled(&self, ip_options: &Ipv6Options) -> bool {
            ip_options.dual_stack_enabled
        }
        fn set_dual_stack_enabled(&self, ip_options: &mut Ipv6Options, value: bool) {
            ip_options.dual_stack_enabled = value;
        }
        fn with_both_demux_mut<
            O,
            F: FnOnce(
                &mut DemuxState<Ipv6, FakeWeakDeviceId<D>, BT>,
                &mut DemuxState<Ipv4, FakeWeakDeviceId<D>, BT>,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.outer.v6.demux.borrow_mut(), &mut self.outer.v4.demux.borrow_mut())
        }
        fn with_both_demux_mut_and_ip_transport_ctx<
            O,
            F: FnOnce(
                &mut DemuxState<Ipv6, FakeWeakDeviceId<D>, BT>,
                &mut DemuxState<Ipv4, FakeWeakDeviceId<D>, BT>,
                &mut Self::DualStackIpTransportCtx<'_>,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let demux_v6 = Rc::clone(&self.outer.v6.demux);
            let demux_v4 = Rc::clone(&self.outer.v4.demux);
            let mut demux_v6 = demux_v6.borrow_mut();
            let mut demux_v4 = demux_v4.borrow_mut();
            cb(&mut demux_v6, &mut demux_v4, self)
        }
    }

    impl<I: Ip, D: FakeStrongDeviceId, BT: TcpTestBindingsTypes<D>> CounterContext<TcpCounters<I>>
        for TcpCoreCtx<D, BT>
    {
        fn with_counters<O, F: FnOnce(&TcpCounters<I>) -> O>(&self, cb: F) -> O {
            let counters =
                I::map_ip((), |()| &self.outer.v4.counters, |()| &self.outer.v6.counters);
            cb(counters)
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
        type DualStackConverter = Uninstantiable;
        fn converter() -> MaybeDualStack<Self::DualStackConverter, Self::SingleStackConverter> {
            MaybeDualStack::NotDualStack(())
        }
        fn recv_src_addr(addr: Self::Addr) -> Self::RecvSrcAddr {
            addr
        }

        fn new_device_state(
            addrs: impl IntoIterator<Item = Self::Addr>,
            prefix: u8,
        ) -> DualStackIpDeviceState<TcpBindingsCtx<FakeDeviceId>> {
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
            DualStackIpDeviceState {
                ipv4,
                ipv6: Default::default(),
                metric: DEFAULT_INTERFACE_METRIC,
            }
        }
    }

    impl TcpTestIpExt for Ipv6 {
        type SingleStackConverter = Uninstantiable;
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
        ) -> DualStackIpDeviceState<TcpBindingsCtx<FakeDeviceId>> {
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
            DualStackIpDeviceState {
                ipv4: Default::default(),
                ipv6,
                metric: DEFAULT_INTERFACE_METRIC,
            }
        }
    }

    type TcpTestNetwork = FakeNetwork<
        &'static str,
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

    fn assert_this_stack_conn<
        'a,
        I: DualStackIpExt,
        BC: TcpBindingsContext<I, CC::WeakDeviceId>,
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

    /// A trait providing a shortcut to instantiate a [`TcpApi`] from a context.
    trait TcpApiExt: crate::context::ContextPair + Sized {
        fn tcp_api<I: Ip>(&mut self) -> TcpApi<I, &mut Self> {
            TcpApi::new(self)
        }
    }

    impl<O> TcpApiExt for O where O: crate::context::ContextPair + Sized {}

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
            |_: &mut TcpCtx<_>, meta: DualStackSendIpPacketMeta<_>, buffer: Buf<Vec<u8>>| {
                let x: f64 = rng.gen();
                (x > drop_rate).then_some((meta, buffer))
            };

        let backlog = NonZeroUsize::new(1).unwrap();
        let server = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let server = api.create(Default::default());
            api.bind(
                &server,
                SpecifiedAddr::new(listen_addr).map(|a| ZonedAddr::Unzoned(a)),
                Some(server_port),
            )
            .expect("failed to bind the server socket");
            api.listen(&server, backlog).expect("can listen");
            server
        });

        let client_ends = WriteBackClientBuffers::default();
        let client = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(ProvidedBuffers::Buffers(client_ends.clone()));
            if client_reuse_addr {
                api.set_reuseaddr(&socket, true).expect("can set");
            }
            if let Some(port) = client_port {
                api.bind(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), Some(port))
                    .expect("failed to bind the client socket")
            }
            api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), server_port)
                .expect("failed to connect");
            socket
        });
        // If drop rate is 0, the SYN is guaranteed to be delivered, so we can
        // look at the SYN queue deterministically.
        if drop_rate == 0.0 {
            // Step once for the SYN packet to be sent.
            let _: StepResult = net.step();
            // The listener should create a pending socket.
            assert_matches!(
                &server.get().deref().socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Listener((
                    MaybeListener::Listener(Listener {
                        accept_queue,
                        ..
                    }), ..))) => {
                    assert_eq!(accept_queue.ready_len(), 0);
                    assert_eq!(accept_queue.pending_len(), 1);
                }
            );
            // The handshake is not done, calling accept here should not succeed.
            net.with_context(REMOTE, |ctx| {
                let mut api = ctx.tcp_api::<I>();
                assert_matches!(api.accept(&server), Err(AcceptError::WouldBlock));
            });
        }

        // Step the test network until the handshake is done.
        net.run_until_idle_with(&mut maybe_drop_frame);
        let (accepted, addr, accepted_ends) = net.with_context(REMOTE, |ctx| {
            ctx.tcp_api::<I>().accept(&server).expect("failed to accept")
        });
        if let Some(port) = client_port {
            assert_eq!(
                addr,
                SocketAddr { ip: ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip), port: port }
            );
        } else {
            assert_eq!(addr.ip, ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip));
        }

        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            assert_eq!(
                api.connect(
                    &client,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)),
                    server_port,
                ),
                Ok(())
            );
        });

        let assert_connected = |conn_id: &TcpSocketId<I, _, _>| {
            assert_matches!(
            &conn_id.get().deref().socket_state,
            TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
            net.with_context(c, |ctx| ctx.tcp_api::<I>().do_send(id))
        }
        net.run_until_idle_with(&mut maybe_drop_frame);

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
            &server.get().deref().socket_state,
            TcpSocketStateInner::Bound(BoundSocketState::Listener((MaybeListener::Listener(l),..))) => {
                assert_eq!(l, &Listener::new(
                    backlog,
                    BufferSizes::default(),
                    SocketOptions::default(),
                    Default::default()
                ));
            }
        );

        net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            assert_eq!(api.shutdown(&server, ShutdownType::Receive), Ok(false));
            api.close(server);
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
        let (mut net, _client, _client_snd_end, _accepted) =
            bind_listen_connect_accept_inner::<I>(listen_addr, bind_config, 0, 0.0);

        struct ExpectedCounters {
            tx: u64,
            rx: u64,
            passive_open: u64,
            active_open: u64,
        }
        let mut assert_counters =
            |context_name: &'static str, ExpectedCounters { tx, rx, passive_open, active_open }| {
                net.with_context(context_name, |ctx| {
                    ctx.core_ctx.with_counters(|counters: &TcpCounters<I>| {
                        let c = counters.as_ref();
                        assert_eq!(c.segment_send_errors.get(), 0, "{}", context_name);
                        assert_eq!(c.segments_sent.get(), tx, "{}", context_name);
                        assert_eq!(c.invalid_segments_received.get(), 0, "{}", context_name);
                        assert_eq!(c.valid_segments_received.get(), rx, "{}", context_name);
                        assert_eq!(c.received_segments_dispatched.get(), rx, "{}", context_name);
                        assert_eq!(
                            c.active_connection_openings.get(),
                            active_open,
                            "{}",
                            context_name
                        );
                        assert_eq!(
                            c.passive_connection_openings.get(),
                            passive_open,
                            "{}",
                            context_name
                        );
                        assert_eq!(c.failed_connection_attempts.get(), 0, "{}", context_name);
                        // Each side of the connection sends and receives a,
                        // SYN, regardless of it initiated the connection.
                        assert_eq!(c.syns_sent.get(), 1);
                        assert_eq!(c.syns_received.get(), 1);
                    })
                })
            };
        // Communication done by `bind_listen_connect_accept_inner`:
        //   LOCAL -> REMOTE: SYN to initiate the connection.
        //   LOCAL <- REMOTE: ACK the connection.
        //   LOCAL -> REMOTE: ACK the ACK.
        //   LOCAL -> REMOTE: Send "hello".
        //   LOCAL <- REMOTE: ACK "hello".
        //   LOCAL <- REMOTE: Send "hello".
        //   LOCAL -> REMOTE: ACK "hello".
        assert_counters(LOCAL, ExpectedCounters { tx: 4, rx: 3, passive_open: 0, active_open: 1 });
        assert_counters(REMOTE, ExpectedCounters { tx: 3, rx: 4, passive_open: 1, active_open: 0 });
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        let s1 = api.create(Default::default());
        let s2 = api.create(Default::default());

        api.bind(&s1, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), Some(PORT_1))
            .expect("first bind should succeed");
        assert_matches!(
            api.bind(&s2, SpecifiedAddr::new(conflict_addr).map(ZonedAddr::Unzoned), Some(PORT_1)),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );
        api.bind(&s2, SpecifiedAddr::new(conflict_addr).map(ZonedAddr::Unzoned), Some(PORT_2))
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        for port in 1..=u16::MAX {
            let port = NonZeroU16::new(port).unwrap();
            if port == available_port {
                continue;
            }
            let socket = api.create(Default::default());

            api.bind(&socket, None, Some(port)).expect("uncontested bind");
            api.listen(&socket, const_unwrap_option(NonZeroUsize::new(1))).expect("can listen");
        }

        // Now that all but the LOCAL_PORT are occupied, ask the stack to
        // select a port.
        let socket = api.create(Default::default());
        let result = api.bind(&socket, None, None).map(|()| {
            assert_matches!(
                api.get_info(&socket),
                SocketInfo::Bound(bound) => bound.port
            )
        });
        assert_eq!(result, expected_result.map_err(From::from));

        // Now close the socket and try a connect call to ourselves on the
        // available port. Self-connection protection should always prevent us
        // from doing that even when the port is in the ephemeral range.
        api.close(socket);
        let socket = api.create(Default::default());
        let result =
            api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), available_port);
        assert_eq!(result, Err(ConnectError::NoPort));
    }

    #[ip_test]
    fn bind_to_non_existent_address<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        let unbound = api.create(Default::default());
        assert_matches!(
            api.bind(&unbound, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), None),
            Err(BindError::LocalAddressError(LocalAddressError::AddressMismatch))
        );

        assert_matches!(unbound.get().deref().socket_state, TcpSocketStateInner::Unbound(_));
    }

    #[test]
    fn bind_addr_requires_zone() {
        let local_ip = LinkLocalAddr::new(net_ip_v6!("fe80::1")).unwrap().into_specified();

        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(
            Ipv6::FAKE_CONFIG.local_ip,
            Ipv6::FAKE_CONFIG.remote_ip,
            Ipv6::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<Ipv6>();
        let unbound = api.create(Default::default());
        assert_matches!(
            api.bind(&unbound, Some(ZonedAddr::Unzoned(local_ip)), None),
            Err(BindError::LocalAddressError(LocalAddressError::Zone(
                ZonedAddressError::RequiredZoneNotProvided
            )))
        );

        assert_matches!(unbound.get().deref().socket_state, TcpSocketStateInner::Unbound(_));
    }

    #[test]
    fn connect_bound_requires_zone() {
        let ll_ip = LinkLocalAddr::new(net_ip_v6!("fe80::1")).unwrap().into_specified();

        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(
            Ipv6::FAKE_CONFIG.local_ip,
            Ipv6::FAKE_CONFIG.remote_ip,
            Ipv6::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<Ipv6>();
        let socket = api.create(Default::default());
        api.bind(&socket, None, None).expect("bind succeeds");
        assert_matches!(
            api.connect(&socket, Some(ZonedAddr::Unzoned(ll_ip)), PORT_1,),
            Err(ConnectError::Zone(ZonedAddressError::RequiredZoneNotProvided))
        );

        assert_matches!(socket.get().deref().socket_state, TcpSocketStateInner::Bound(_));
    }

    #[test]
    fn connect_unbound_picks_link_local_source_addr() {
        set_logger_for_test();
        let client_ip = SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap();
        let server_ip = SpecifiedAddr::new(net_ip_v6!("1:2:3:4::")).unwrap();
        let mut net = FakeNetwork::new(
            [
                (LOCAL, TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(client_ip, server_ip, 0))),
                (REMOTE, TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(server_ip, client_ip, 0))),
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
        let client_connection = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api();
            let socket: TcpSocketId<Ipv6, _, _> = api.create(Default::default());
            api.connect(&socket, Some(ZonedAddr::Unzoned(server_ip)), PORT).expect("can connect");
            socket
        });
        net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let socket = api.create(Default::default());
            api.bind(&socket, None, Some(PORT)).expect("failed to bind the client socket");
            let _listener = api.listen(&socket, NonZeroUsize::MIN).expect("can listen");
        });

        // Advance until the connection is established.
        net.run_until_idle();

        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api();
            assert_eq!(
                api.connect(&client_connection, Some(ZonedAddr::Unzoned(server_ip)), PORT),
                Ok(())
            );

            let info = assert_matches!(
                api.get_info(&client_connection),
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
                local_ip,
                ZonedAddr::Zoned(
                    AddrAndZone::new(client_ip, FakeWeakDeviceId(FakeDeviceId)).unwrap()
                )
            );
            assert_eq!(remote_ip, ZonedAddr::Unzoned(server_ip));

            // Double-check that the bound device can't be changed after being set
            // implicitly.
            assert_matches!(
                api.set_device(&client_connection, None),
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
                (LOCAL, TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(server_ip, client_ip, 0))),
                (REMOTE, TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(client_ip, server_ip, 0))),
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
        let server_listener = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let socket: TcpSocketId<Ipv6, _, _> = api.create(Default::default());
            api.bind(&socket, None, Some(PORT)).expect("failed to bind the client socket");
            api.listen(&socket, NonZeroUsize::MIN).expect("can listen");
            socket
        });
        let client_connection = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let socket = api.create(Default::default());
            api.connect(
                &socket,
                Some(ZonedAddr::Zoned(AddrAndZone::new(server_ip, FakeDeviceId).unwrap())),
                PORT,
            )
            .expect("failed to open a connection");
            socket
        });

        // Advance until the connection is established.
        net.run_until_idle();

        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api();
            let (server_connection, _addr, _buffers) =
                api.accept(&server_listener).expect("connection is waiting");

            let info = assert_matches!(
                api.get_info(&server_connection),
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
                local_ip,
                ZonedAddr::Zoned(
                    AddrAndZone::new(server_ip, FakeWeakDeviceId(FakeDeviceId)).unwrap()
                )
            );
            assert_eq!(remote_ip, ZonedAddr::Unzoned(client_ip));

            // Double-check that the bound device can't be changed after being set
            // implicitly.
            assert_matches!(
                api.set_device(&server_connection, None),
                Err(SetDeviceError::ZoneChange)
            );
        });
        net.with_context(REMOTE, |ctx| {
            assert_eq!(
                ctx.tcp_api().connect(
                    &client_connection,
                    Some(ZonedAddr::Zoned(AddrAndZone::new(server_ip, FakeDeviceId).unwrap())),
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

        let client = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let conn = api.create(Default::default());
            api.bind(&conn, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), Some(PORT_1))
                .expect("failed to bind the client socket");
            api.connect(&conn, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), PORT_1)
                .expect("failed to connect");
            conn
        });

        // Step one time for SYN packet to be delivered.
        let _: StepResult = net.step();
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

        net.run_until_idle();
        // Finally, the connection should be reset and bindings should have been
        // signaled.
        assert_matches!(
        &client.get().deref().socket_state,
        TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
        net.with_context(LOCAL, |ctx| {
            assert_matches!(
                ctx.tcp_api().connect(
                    &client,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)),
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new_multiple_devices());
        let mut api = ctx.tcp_api::<I>();
        let sock_a = api.create(Default::default());
        assert_matches!(api.set_device(&sock_a, Some(MultipleDevicesId::A),), Ok(()));
        api.bind(&sock_a, None, Some(LOCAL_PORT)).expect("bind should succeed");
        api.listen(&sock_a, const_unwrap_option(NonZeroUsize::new(10))).expect("can listen");

        let socket = api.create(Default::default());
        // Binding `socket` to the unspecified address should fail since the address
        // is shadowed by `sock_a`.
        assert_matches!(
            api.bind(&socket, None, Some(LOCAL_PORT)),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );

        // Once `socket` is bound to a different device, though, it no longer
        // conflicts.
        assert_matches!(api.set_device(&socket, Some(MultipleDevicesId::B),), Ok(()));
        api.bind(&socket, None, Some(LOCAL_PORT)).expect("no conflict");
    }

    #[test_case(None)]
    #[test_case(Some(MultipleDevicesId::B); "other")]
    fn set_bound_device_listener_on_zoned_addr(set_device: Option<MultipleDevicesId>) {
        set_logger_for_test();
        let ll_addr = LinkLocalAddr::new(Ipv6::LINK_LOCAL_UNICAST_SUBNET.network()).unwrap();

        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::with_inner_and_outer_state(
            FakeDualStackIpSocketCtx::new(MultipleDevicesId::all().into_iter().map(|device| {
                FakeDeviceConfig {
                    device,
                    local_ips: vec![ll_addr.into_specified()],
                    remote_ips: vec![ll_addr.into_specified()],
                }
            })),
            Default::default(),
        ));
        let mut api = ctx.tcp_api::<Ipv6>();
        let socket = api.create(Default::default());
        api.bind(
            &socket,
            Some(ZonedAddr::Zoned(
                AddrAndZone::new(ll_addr.into_specified(), MultipleDevicesId::A).unwrap(),
            )),
            Some(LOCAL_PORT),
        )
        .expect("bind should succeed");

        assert_matches!(api.set_device(&socket, set_device), Err(SetDeviceError::ZoneChange));
    }

    #[test_case(None)]
    #[test_case(Some(MultipleDevicesId::B); "other")]
    fn set_bound_device_connected_to_zoned_addr(set_device: Option<MultipleDevicesId>) {
        set_logger_for_test();
        let ll_addr = LinkLocalAddr::new(Ipv6::LINK_LOCAL_UNICAST_SUBNET.network()).unwrap();

        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::with_inner_and_outer_state(
            FakeDualStackIpSocketCtx::new(MultipleDevicesId::all().into_iter().map(|device| {
                FakeDeviceConfig {
                    device,
                    local_ips: vec![ll_addr.into_specified()],
                    remote_ips: vec![ll_addr.into_specified()],
                }
            })),
            Default::default(),
        ));
        let mut api = ctx.tcp_api::<Ipv6>();
        let socket = api.create(Default::default());
        api.connect(
            &socket,
            Some(ZonedAddr::Zoned(
                AddrAndZone::new(ll_addr.into_specified(), MultipleDevicesId::A).unwrap(),
            )),
            LOCAL_PORT,
        )
        .expect("connect should succeed");

        assert_matches!(api.set_device(&socket, set_device), Err(SetDeviceError::ZoneChange));
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        let socket = api.create(Default::default());

        let (addr, port) = (SpecifiedAddr::new(ip_addr).map(ZonedAddr::Unzoned), PORT_1);

        api.bind(&socket, addr, Some(port)).expect("bind should succeed");
        if listen {
            api.listen(&socket, const_unwrap_option(NonZeroUsize::new(25))).expect("can listen");
        }
        let info = api.get_info(&socket);
        assert_eq!(
            info,
            SocketInfo::Bound(BoundInfo {
                addr: addr.map(|a| a.map_zone(FakeWeakDeviceId)),
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        let local = SocketAddr { ip: ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip), port: PORT_1 };
        let remote = SocketAddr { ip: ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip), port: PORT_2 };

        let socket = api.create(Default::default());
        api.bind(&socket, Some(local.ip), Some(local.port)).expect("bind should succeed");

        api.connect(&socket, Some(remote.ip), remote.port).expect("connect should succeed");

        assert_eq!(
            api.get_info(&socket),
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
                    TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(
                        server_ip,
                        client_ip,
                        Ipv6::LINK_LOCAL_UNICAST_SUBNET.prefix(),
                    )),
                ),
                (
                    REMOTE,
                    TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(
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

        let local_server = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let socket = api.create(Default::default());
            let device = FakeDeviceId;
            let bind_addr = match listen_any {
                true => None,
                false => Some(ZonedAddr::Zoned(AddrAndZone::new(server_ip, device).unwrap())),
            };

            api.bind(&socket, bind_addr, Some(PORT_1)).expect("failed to bind the client socket");
            api.listen(&socket, const_unwrap_option(NonZeroUsize::new(1))).expect("can listen");
            socket
        });

        let _remote_client = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let socket = api.create(Default::default());
            let device = FakeDeviceId;
            api.connect(
                &socket,
                Some(ZonedAddr::Zoned(AddrAndZone::new(server_ip, device).unwrap())),
                PORT_1,
            )
            .expect("failed to connect");
            socket
        });

        net.run_until_idle();

        let ConnectionInfo { remote_addr, local_addr, device } = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api();
            let (server_conn, _addr, _buffers) =
                api.accept(&local_server).expect("connection is available");
            assert_matches!(
                api.get_info(&server_conn),
                SocketInfo::Connection(info) => info
            )
        });

        let device = assert_matches!(device, Some(device) => device);
        assert_eq!(
            local_addr,
            SocketAddr {
                ip: ZonedAddr::Zoned(AddrAndZone::new(server_ip, device).unwrap()),
                port: PORT_1
            }
        );
        let SocketAddr { ip: remote_ip, port: _ } = remote_addr;
        assert_eq!(remote_ip, ZonedAddr::Zoned(AddrAndZone::new(client_ip, device).unwrap()));
    }

    #[test]
    fn bound_connection_info_zoned_addrs() {
        let local_ip = LinkLocalAddr::new(net_ip_v6!("fe80::1")).unwrap().into_specified();
        let remote_ip = LinkLocalAddr::new(net_ip_v6!("fe80::2")).unwrap().into_specified();
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<Ipv6>(
            local_ip,
            remote_ip,
            Ipv6::LINK_LOCAL_UNICAST_SUBNET.prefix(),
        ));

        let local_addr = SocketAddr {
            ip: ZonedAddr::Zoned(AddrAndZone::new(local_ip, FakeDeviceId).unwrap()),
            port: PORT_1,
        };
        let remote_addr = SocketAddr {
            ip: ZonedAddr::Zoned(AddrAndZone::new(remote_ip, FakeDeviceId).unwrap()),
            port: PORT_2,
        };
        let mut api = ctx.tcp_api::<Ipv6>();

        let socket = api.create(Default::default());
        api.bind(&socket, Some(local_addr.ip), Some(local_addr.port)).expect("bind should succeed");

        assert_eq!(
            api.get_info(&socket),
            SocketInfo::Bound(BoundInfo {
                addr: Some(local_addr.ip.map_zone(FakeWeakDeviceId)),
                port: local_addr.port,
                device: Some(FakeWeakDeviceId(FakeDeviceId))
            })
        );

        api.connect(&socket, Some(remote_addr.ip), remote_addr.port)
            .expect("connect should succeed");

        assert_eq!(
            api.get_info(&socket),
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
        let close_called = net.with_context(LOCAL, |ctx| {
            ctx.tcp_api().close(local);
            ctx.bindings_ctx.now()
        });

        while {
            assert!(!net.step().is_idle());
            let is_fin_wait_2 = {
                let local = weak_local.upgrade().unwrap();
                let state = local.get();
                let state = assert_matches!(
                    &state.deref().socket_state,
                    TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
            net.with_context(REMOTE, |ctx| {
                ctx.tcp_api().close(remote);
            });
        }

        net.run_until_idle();

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
        net.with_context(LOCAL, |ctx| {
            assert_eq!(ctx.tcp_api().shutdown(&local, ShutdownType::Send), Ok(true));
        });
        loop {
            assert!(!net.step().is_idle());
            let is_fin_wait_2 = {
                let state = local.get();
                let state = assert_matches!(
                &state.deref().socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
        net.with_context(LOCAL, |ctx| {
            ctx.tcp_api().close(local);
        });
        net.run_until_idle();
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
            net.with_context(name, |ctx| {
                let mut api = ctx.tcp_api();
                assert_eq!(
                    api.shutdown(id,ShutdownType::Send),
                    Ok(true)
                );
                assert_matches!(
                    &id.get().deref().socket_state,
                    TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
                    api.shutdown(id,ShutdownType::Send),
                    Ok(true)
                );
            });
        }
        net.run_until_idle();
        for (name, id) in [(LOCAL, local), (REMOTE, remote)] {
            net.with_context(name, |ctx| {
                assert_matches!(
                    &id.get().deref().socket_state,
                    TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
                ctx.tcp_api().close(id);
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        let unbound = api.create(Default::default());
        let weak_unbound = unbound.downgrade();
        api.close(unbound);
        assert_eq!(weak_unbound.upgrade(), None);
    }

    #[ip_test]
    fn remove_bound<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        let socket = api.create(Default::default());
        api.bind(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), None)
            .expect("bind should succeed");
        let weak_socket = socket.downgrade();
        api.close(socket);
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
        let local_listener = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(Default::default());
            api.bind(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), Some(PORT_1))
                .expect("bind should succeed");
            api.listen(&socket, NonZeroUsize::new(5).unwrap()).expect("can listen");
            socket
        });

        let remote_connection = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(Default::default());
            api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), PORT_1)
                .expect("connect should succeed");
            socket
        });

        // After the following step, we should have one established connection
        // in the listener's accept queue, which ought to be aborted during
        // shutdown.
        net.run_until_idle();

        // The incoming connection was signaled, and the remote end was notified
        // of connection establishment.
        net.with_context(REMOTE, |ctx| {
            assert_eq!(
                ctx.tcp_api().connect(
                    &remote_connection,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
                    PORT_1
                ),
                Ok(())
            );
        });

        // Create a second half-open connection so that we have one entry in the
        // pending queue.
        let second_connection = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(Default::default());
            api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), PORT_1)
                .expect("connect should succeed");
            socket
        });

        let _: StepResult = net.step();

        // We have a timer scheduled for the pending connection.
        net.with_context(LOCAL, |TcpCtx { core_ctx: _, bindings_ctx }| {
            assert_matches!(bindings_ctx.timers.timers().len(), 1);
        });

        net.with_context(LOCAL, |ctx| {
            assert_eq!(ctx.tcp_api().shutdown(&local_listener, ShutdownType::Receive,), Ok(false));
        });

        // The timer for the pending connection should be cancelled.
        net.with_context(LOCAL, |TcpCtx { core_ctx: _, bindings_ctx }| {
            assert_eq!(bindings_ctx.timers.timers().len(), 0);
        });

        net.run_until_idle();

        // Both remote sockets should now be reset to Closed state.
        net.with_context(REMOTE, |ctx| {
            for conn in [&remote_connection, &second_connection] {
                assert_eq!(
                    ctx.tcp_api().get_socket_error(conn),
                    Some(ConnectionError::ConnectionReset),
                )
            }

            assert_matches!(
                &remote_connection.get().deref().socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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

        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let new_unbound = api.create(Default::default());
            assert_matches!(
                api.bind(
                    &new_unbound,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip,)),
                    Some(PORT_1),
                ),
                Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
            );
            // Bring the already-shutdown listener back to listener again.
            api.listen(&local_listener, NonZeroUsize::new(5).unwrap()).expect("can listen again");
        });

        let new_remote_connection = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(Default::default());
            api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), PORT_1)
                .expect("connect should succeed");
            socket
        });

        net.run_until_idle();

        net.with_context(REMOTE, |ctx| {
            assert_matches!(
                &new_remote_connection.get().deref().socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
                ctx.tcp_api().connect(
                    &new_remote_connection,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
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

        let local_listener = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let local_listener = api.create(Default::default());
            api.bind(
                &local_listener,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
                Some(PORT_1),
            )
            .expect("bind should succeed");
            api.set_send_buffer_size(&local_listener, local_sizes.send);
            api.set_receive_buffer_size(&local_listener, local_sizes.receive);
            api.listen(&local_listener, NonZeroUsize::new(5).unwrap()).expect("can listen");
            local_listener
        });

        let remote_connection = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let remote_connection = api.create(Default::default());
            api.set_send_buffer_size(&remote_connection, remote_sizes.send);
            api.set_receive_buffer_size(&remote_connection, local_sizes.receive);
            api.connect(
                &remote_connection,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
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
                net.with_context(LOCAL, |ctx| {
                    let mut api = ctx.tcp_api();
                    api.set_send_buffer_size(local, local_sizes.send);
                    if let Some(size) = api.send_buffer_size(local) {
                        assert_eq!(size, local_sizes.send);
                    }
                    api.set_receive_buffer_size(local, local_sizes.receive);
                    if let Some(size) = api.receive_buffer_size(local) {
                        assert_eq!(size, local_sizes.receive);
                    }
                });
                net.with_context(REMOTE, |ctx| {
                    let mut api = ctx.tcp_api();
                    api.set_send_buffer_size(remote, remote_sizes.send);
                    if let Some(size) = api.send_buffer_size(remote) {
                        assert_eq!(size, remote_sizes.send);
                    }
                    api.set_receive_buffer_size(remote, remote_sizes.receive);
                    if let Some(size) = api.receive_buffer_size(remote) {
                        assert_eq!(size, remote_sizes.receive);
                    }
                });
                if net.step().is_idle() {
                    break;
                }
            };

        // Set the send buffer size at each stage of sockets on both ends of the
        // handshake process just to make sure it doesn't break.
        step_and_increment_buffer_sizes_until_idle(&mut net, &local_listener, &remote_connection);

        let local_connection = net.with_context(LOCAL, |ctx| {
            let (conn, _, _) = ctx.tcp_api().accept(&local_listener).expect("received connection");
            conn
        });

        step_and_increment_buffer_sizes_until_idle(&mut net, &local_connection, &remote_connection);

        net.with_context(LOCAL, |ctx| {
            assert_eq!(ctx.tcp_api().shutdown(&local_connection, ShutdownType::Send,), Ok(true));
        });

        step_and_increment_buffer_sizes_until_idle(&mut net, &local_connection, &remote_connection);

        net.with_context(REMOTE, |ctx| {
            assert_eq!(ctx.tcp_api().shutdown(&remote_connection, ShutdownType::Send,), Ok(true),);
        });

        step_and_increment_buffer_sizes_until_idle(&mut net, &local_connection, &remote_connection);
    }

    #[ip_test]
    fn set_reuseaddr_unbound<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();

        let first_bound = {
            let socket = api.create(Default::default());
            api.set_reuseaddr(&socket, true).expect("can set");
            api.bind(&socket, None, None).expect("bind succeeds");
            socket
        };
        let _second_bound = {
            let socket = api.create(Default::default());
            api.set_reuseaddr(&socket, true).expect("can set");
            api.bind(&socket, None, None).expect("bind succeeds");
            socket
        };

        api.listen(&first_bound, const_unwrap_option(NonZeroUsize::new(10))).expect("can listen");
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();

        let first = api.create(Default::default());
        api.set_reuseaddr(&first, set_reuseaddr[0]).expect("can set");
        api.bind(&first, None, Some(PORT_1)).expect("bind succeeds");

        let second = api.create(Default::default());
        api.set_reuseaddr(&second, set_reuseaddr[1]).expect("can set");
        let second_bind_result = api.bind(&second, None, Some(PORT_1));

        assert_eq!(second_bind_result, expected.map_err(From::from));
    }

    #[ip_test]
    fn toggle_reuseaddr_bound_different_addrs<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let addrs = [1, 2].map(|i| I::get_other_ip_address(i));
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::with_inner_and_outer_state(
            FakeDualStackIpSocketCtx::<_, _>::with_devices_state(core::iter::once::<(
                _,
                _,
                Vec<SpecifiedAddr<IpAddr>>,
            )>((
                FakeDeviceId,
                I::new_device_state(addrs.iter().map(Witness::get), I::FAKE_CONFIG.subnet.prefix()),
                vec![],
            ))),
            FakeDualStackTcpState::default(),
        ));
        let mut api = ctx.tcp_api::<I>();

        let first = api.create(Default::default());
        api.bind(&first, Some(ZonedAddr::Unzoned(addrs[0])), Some(PORT_1)).unwrap();

        let second = api.create(Default::default());
        api.bind(&second, Some(ZonedAddr::Unzoned(addrs[1])), Some(PORT_1)).unwrap();
        // Setting and un-setting ReuseAddr should be fine since these sockets
        // don't conflict.
        api.set_reuseaddr(&first, true).expect("can set");
        api.set_reuseaddr(&first, false).expect("can un-set");
    }

    #[ip_test]
    fn unset_reuseaddr_bound_unspecified_specified<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();
        let first = api.create(Default::default());
        api.set_reuseaddr(&first, true).expect("can set");
        api.bind(&first, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), Some(PORT_1)).unwrap();

        let second = api.create(Default::default());
        api.set_reuseaddr(&second, true).expect("can set");
        api.bind(&second, None, Some(PORT_1)).unwrap();

        // Both sockets can be bound because they have ReuseAddr set. Since
        // removing it would introduce inconsistent state, that's not allowed.
        assert_matches!(api.set_reuseaddr(&first, false), Err(SetReuseAddrError::AddrInUse));
        assert_matches!(api.set_reuseaddr(&second, false), Err(SetReuseAddrError::AddrInUse));
    }

    #[ip_test]
    fn reuseaddr_allows_binding_under_connection<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        set_logger_for_test();
        let mut net = new_test_net::<I>();

        let server = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let server = api.create(Default::default());
            api.set_reuseaddr(&server, true).expect("can set");
            api.bind(&server, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), Some(PORT_1))
                .expect("failed to bind the client socket");
            api.listen(&server, const_unwrap_option(NonZeroUsize::new(10))).expect("can listen");
            server
        });

        let client = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let client = api.create(Default::default());
            api.connect(&client, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), PORT_1)
                .expect("connect should succeed");
            client
        });
        // Finish the connection establishment.
        net.run_until_idle();
        net.with_context(REMOTE, |ctx| {
            assert_eq!(
                ctx.tcp_api().connect(
                    &client,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
                    PORT_1
                ),
                Ok(())
            );
        });

        // Now accept the connection and close the listening socket. Then
        // binding a new socket on the same local address should fail unless the
        // socket has SO_REUSEADDR set.
        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api();
            let (_server_conn, _, _): (_, SocketAddr<_, _>, ClientBuffers) =
                api.accept(&server).expect("pending connection");

            assert_eq!(api.shutdown(&server, ShutdownType::Receive), Ok(false));
            api.close(server);

            let unbound = api.create(Default::default());
            assert_eq!(
                api.bind(&unbound, None, Some(PORT_1)),
                Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
            );

            // Binding should succeed after setting ReuseAddr.
            api.set_reuseaddr(&unbound, true).expect("can set");
            api.bind(&unbound, None, Some(PORT_1)).expect("bind succeeds");
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();

        let [first_addr, second_addr] =
            bind_specified.map(|b| b.then_some(I::FAKE_CONFIG.local_ip).map(ZonedAddr::Unzoned));
        let first_bound = {
            let socket = api.create(Default::default());
            api.bind(&socket, first_addr, Some(PORT_1)).expect("bind succeeds");
            socket
        };

        let second = api.create(Default::default());

        // Binding the second socket will fail because the first doesn't have
        // SO_REUSEADDR set.
        assert_matches!(
            api.bind(&second, second_addr, Some(PORT_1)),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );

        // Setting SO_REUSEADDR for the second socket isn't enough.
        api.set_reuseaddr(&second, true).expect("can set");
        assert_matches!(
            api.bind(&second, second_addr, Some(PORT_1)),
            Err(BindError::LocalAddressError(LocalAddressError::AddressInUse))
        );

        // Setting SO_REUSEADDR for the first socket lets the second bind.
        api.set_reuseaddr(&first_bound, true).expect("only socket");
        api.bind(&second, second_addr, Some(PORT_1)).expect("can bind");
    }

    #[ip_test]
    fn clear_reuseaddr_listener<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();

        let bound = {
            let socket = api.create(Default::default());
            api.set_reuseaddr(&socket, true).expect("can set");
            api.bind(&socket, None, Some(PORT_1)).expect("bind succeeds");
            socket
        };

        let listener = {
            let socket = api.create(Default::default());
            api.set_reuseaddr(&socket, true).expect("can set");

            api.bind(&socket, None, Some(PORT_1)).expect("bind succeeds");
            api.listen(&socket, const_unwrap_option(NonZeroUsize::new(5))).expect("can listen");
            socket
        };

        // We can't clear SO_REUSEADDR on the listener because it's sharing with
        // the bound socket.
        assert_matches!(api.set_reuseaddr(&listener, false), Err(SetReuseAddrError::AddrInUse));

        // We can, however, connect to the listener with the bound socket. Then
        // the unencumbered listener can clear SO_REUSEADDR.
        api.connect(&bound, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), PORT_1)
            .expect("can connect");
        api.set_reuseaddr(&listener, false).expect("can unset")
    }

    fn deliver_icmp_error<
        I: TcpTestIpExt + IcmpIpExt,
        CC: TcpContext<I, BC, DeviceId = FakeDeviceId>
            + TcpContext<I::OtherVersion, BC, DeviceId = FakeDeviceId>
            + CounterContext<TcpCounters<I>>
            + CounterContext<TcpCounters<I::OtherVersion>>,
        BC: TcpBindingsContext<I, CC::WeakDeviceId>
            + TcpBindingsContext<I::OtherVersion, CC::WeakDeviceId>,
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
    #[test_case(Icmpv6DestUnreachableCode::AddrUnreachable => ConnectionError::HostUnreachable)]
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
        let mut ctx = TcpCtx::with_core_ctx(TcpCoreCtx::new::<I>(
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            I::FAKE_CONFIG.subnet.prefix(),
        ));
        let mut api = ctx.tcp_api::<I>();

        let connection = api.create(Default::default());
        api.connect(&connection, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), PORT_1)
            .expect("failed to create a connection socket");

        let (core_ctx, bindings_ctx) = api.contexts();
        let frames = core_ctx.inner.take_frames();
        let frame = assert_matches!(&frames[..], [(_meta, frame)] => frame);

        deliver_icmp_error::<I, _, _>(
            core_ctx,
            bindings_ctx,
            I::FAKE_CONFIG.local_ip,
            I::FAKE_CONFIG.remote_ip,
            &frame[0..8],
            icmp_error,
        );
        // The TCP handshake should be aborted.
        assert_eq!(
            api.connect(&connection, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), PORT_1),
            Err(ConnectError::Aborted)
        );
        api.get_socket_error(&connection).unwrap()
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
    #[test_case(Icmpv6DestUnreachableCode::AddrUnreachable => ConnectionError::HostUnreachable)]
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
        net.with_context(LOCAL, |ctx| {
            ctx.tcp_api().do_send(&local);
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
        net.with_context(LOCAL, |ctx| {
            let TcpCtx { core_ctx, bindings_ctx } = ctx;
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
                ctx.tcp_api().get_socket_error(&local),
                Some(error) => error
            );
            // But it should stay established.
            assert_matches!(
                &local.get().deref().socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
            + TcpContext<I::OtherVersion, TcpBindingsCtx<FakeDeviceId>>
            + CounterContext<TcpCounters<I>>,
    {
        let mut net = new_test_net::<I>();

        let backlog = NonZeroUsize::new(1).unwrap();
        let server = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let server = api.create(Default::default());
            api.bind(&server, None, Some(PORT_1)).expect("failed to bind the server socket");
            api.listen(&server, backlog).expect("can listen");
            server
        });

        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let conn = api.create(Default::default());
            api.connect(&conn, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), PORT_1)
                .expect("failed to connect");
        });

        assert!(!net.step().is_idle());

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
                    &state.deref().socket_state,
                    TcpSocketStateInner::Bound(BoundSocketState::Listener((
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
                    &state.deref().socket_state,
                    TcpSocketStateInner::Bound(BoundSocketState::Listener((
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
        let listener = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let listener = api.create(Default::default());
            api.set_reuseaddr(&listener, true).expect("can set");
            api.bind(
                &listener,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
                Some(CLIENT_PORT),
            )
            .expect("failed to bind");
            api.listen(&listener, NonZeroUsize::new(1).unwrap()).expect("failed to listen");
            listener
        });
        // This connection is never used, just to keep accept queue full.
        let extra_conn = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let extra_conn = api.create(Default::default());
            api.connect(
                &extra_conn,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
                CLIENT_PORT,
            )
            .expect("failed to connect");
            extra_conn
        });
        net.run_until_idle();

        net.with_context(REMOTE, |ctx| {
            assert_eq!(
                ctx.tcp_api().connect(
                    &extra_conn,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
                    CLIENT_PORT,
                ),
                Ok(())
            );
        });

        // Now we shutdown the sockets and try to bring the local socket to
        // TIME-WAIT.
        let weak_local = local.downgrade();
        net.with_context(LOCAL, |ctx| {
            ctx.tcp_api().close(local);
        });
        assert!(!net.step().is_idle());
        assert!(!net.step().is_idle());
        net.with_context(REMOTE, |ctx| {
            ctx.tcp_api().close(remote);
        });
        assert!(!net.step().is_idle());
        assert!(!net.step().is_idle());
        // The connection should go to TIME-WAIT.
        let (tw_last_seq, tw_last_ack, tw_expiry) = {
            assert_matches!(
                &weak_local.upgrade().unwrap().get().deref().socket_state,
                TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
        let conn = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let conn = api.create(Default::default());
            api.connect(&conn, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), CLIENT_PORT)
                .expect("failed to connect");
            conn
        });
        while net.next_step() != Some(tw_expiry) {
            assert!(!net.step().is_idle());
        }
        // This attempt should fail due the full accept queue at the listener.
        assert_matches!(
        &conn.get().deref().socket_state,
        TcpSocketStateInner::Bound(BoundSocketState::Connected { conn, .. }) => {
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
        net.with_context(LOCAL, |ctx| {
            let _accepted =
                ctx.tcp_api().accept(&listener).expect("failed to accept a new connection");
        });
        let conn = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(Default::default());
            api.bind(
                &socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)),
                Some(SERVER_PORT),
            )
            .expect("failed to bind");
            api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), CLIENT_PORT)
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
        net.run_until_idle();
        net.with_context(REMOTE, |ctx| {
            assert_eq!(
                ctx.tcp_api().connect(
                    &conn,
                    Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)),
                    CLIENT_PORT
                ),
                Ok(())
            );
        });
    }

    #[ip_test]
    fn conn_addr_not_available<I: Ip + TcpTestIpExt + IcmpIpExt>()
    where
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
        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(Default::default());
            api.set_reuseaddr(&socket, true).expect("can set");
            api.bind(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip)), Some(PORT_1))
                .expect("failed to bind");
            assert_eq!(
                api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), PORT_1),
                Err(ConnectError::ConnectionExists),
            )
        });
    }

    #[test_case::test_matrix(
        [None, Some(ZonedAddr::Unzoned((*Ipv4::FAKE_CONFIG.remote_ip).to_ipv6_mapped()))],
        [None, Some(PORT_1)],
        [true, false]
    )]
    fn dual_stack_connect(
        server_bind_ip: Option<ZonedAddr<SpecifiedAddr<Ipv6Addr>, FakeDeviceId>>,
        server_bind_port: Option<NonZeroU16>,
        bind_client: bool,
    ) {
        set_logger_for_test();
        let mut net = new_test_net::<Ipv4>();
        let backlog = NonZeroUsize::new(1).unwrap();
        let (server, listen_port) = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let server = api.create(Default::default());
            api.bind(&server, server_bind_ip, server_bind_port)
                .expect("failed to bind the server socket");
            api.listen(&server, backlog).expect("can listen");
            let port = assert_matches!(
                api.get_info(&server),
                SocketInfo::Bound(info) => info.port
            );
            (server, port)
        });

        let client_ends = WriteBackClientBuffers::default();
        let client = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let socket = api.create(ProvidedBuffers::Buffers(client_ends.clone()));
            if bind_client {
                api.bind(&socket, None, None).expect("failed to bind");
            }
            api.connect(
                &socket,
                Some(ZonedAddr::Unzoned((*Ipv4::FAKE_CONFIG.remote_ip).to_ipv6_mapped())),
                listen_port,
            )
            .expect("failed to connect");
            socket
        });

        // Step the test network until the handshake is done.
        net.run_until_idle();
        let (accepted, addr, accepted_ends) = net
            .with_context(REMOTE, |ctx| ctx.tcp_api().accept(&server).expect("failed to accept"));
        assert_eq!(addr.ip, ZonedAddr::Unzoned((*Ipv4::FAKE_CONFIG.local_ip).to_ipv6_mapped()));

        let ClientBuffers { send: client_snd_end, receive: client_rcv_end } =
            client_ends.0.as_ref().lock().take().unwrap();
        let ClientBuffers { send: accepted_snd_end, receive: accepted_rcv_end } = accepted_ends;
        for snd_end in [client_snd_end, accepted_snd_end] {
            snd_end.lock().extend_from_slice(b"Hello");
        }
        net.with_context(LOCAL, |ctx| ctx.tcp_api().do_send(&client));
        net.with_context(REMOTE, |ctx| ctx.tcp_api().do_send(&accepted));
        net.run_until_idle();

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
            net.with_context(LOCAL, |ctx| ctx.tcp_api().get_info(&client)),
            SocketInfo::Connection(info) => info
        );
        let (local_ip, remote_ip, port) = assert_matches!(
            info,
            ConnectionInfo {
                local_addr: SocketAddr { ip: local_ip, port: _ },
                remote_addr: SocketAddr { ip: remote_ip, port },
                device: _
            } => (local_ip.addr(), remote_ip.addr(), port)
        );
        assert_eq!(remote_ip, Ipv4::FAKE_CONFIG.remote_ip.to_ipv6_mapped());
        assert_matches!(local_ip.to_ipv4_mapped(), Some(_));
        assert_eq!(port, listen_port);
    }

    #[test]
    fn ipv6_dual_stack_enabled() {
        set_logger_for_test();
        let mut net = new_test_net::<Ipv4>();
        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<Ipv6>();
            let socket = api.create(Default::default());
            assert_eq!(api.dual_stack_enabled(&socket), Ok(true));
            api.set_dual_stack_enabled(&socket, false).expect("failed to disable dual stack");
            assert_eq!(api.dual_stack_enabled(&socket), Ok(false));
            assert_eq!(
                api.bind(
                    &socket,
                    Some(ZonedAddr::Unzoned((*Ipv4::FAKE_CONFIG.local_ip).to_ipv6_mapped())),
                    Some(PORT_1),
                ),
                Err(BindError::LocalAddressError(LocalAddressError::CannotBindToAddress))
            );
            assert_eq!(
                api.connect(
                    &socket,
                    Some(ZonedAddr::Unzoned((*Ipv4::FAKE_CONFIG.remote_ip).to_ipv6_mapped())),
                    PORT_1,
                ),
                Err(ConnectError::NoRoute)
            );
        });
    }

    #[test]
    fn ipv4_dual_stack_enabled() {
        set_logger_for_test();
        let mut net = new_test_net::<Ipv4>();
        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<Ipv4>();
            let socket = api.create(Default::default());
            assert_eq!(api.dual_stack_enabled(&socket), Err(NotDualStackCapableError));
            assert_eq!(
                api.set_dual_stack_enabled(&socket, true),
                Err(SetDualStackEnabledError::NotCapable)
            );
        });
    }

    #[ip_test]
    fn closed_not_in_demux<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>: TcpContext<
            I,
            TcpBindingsCtx<FakeDeviceId>,
            SingleStackConverter = I::SingleStackConverter,
            DualStackConverter = I::DualStackConverter,
        >,
    {
        let (mut net, local, _local_snd_end, remote) = bind_listen_connect_accept_inner::<I>(
            I::UNSPECIFIED_ADDRESS,
            BindConfig { client_port: None, server_port: PORT_1, client_reuse_addr: false },
            0,
            0.0,
        );
        // Assert that the sockets are bound in the socketmap.
        for ctx_name in [LOCAL, REMOTE] {
            net.with_context(ctx_name, |ContextPair { core_ctx, bindings_ctx: _ }| {
                TcpDemuxContext::<I, _, _>::with_demux(core_ctx, |DemuxState { socketmap }| {
                    assert_eq!(socketmap.len(), 1);
                })
            });
        }
        for (ctx_name, socket) in [(LOCAL, &local), (REMOTE, &remote)] {
            net.with_context(ctx_name, |ctx| {
                assert_eq!(ctx.tcp_api().shutdown(socket, ShutdownType::SendAndReceive), Ok(true));
            });
        }
        net.run_until_idle();
        // Both sockets are closed by now, but they are not defunct because we
        // never called `close` on them, but they should not be in the demuxer
        // regardless.
        for ctx_name in [LOCAL, REMOTE] {
            net.with_context(ctx_name, |ContextPair { core_ctx, bindings_ctx: _ }| {
                TcpDemuxContext::<I, _, _>::with_demux(core_ctx, |DemuxState { socketmap }| {
                    assert_eq!(socketmap.len(), 0);
                })
            });
        }
    }

    #[ip_test]
    fn tcp_accept_queue_clean_up_closed<I: Ip + TcpTestIpExt>()
    where
        TcpCoreCtx<FakeDeviceId, TcpBindingsCtx<FakeDeviceId>>:
            TcpContext<I, TcpBindingsCtx<FakeDeviceId>>,
    {
        let mut net = new_test_net::<I>();
        let backlog = NonZeroUsize::new(1).unwrap();
        let server_port = NonZeroU16::new(1024).unwrap();
        let server = net.with_context(REMOTE, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let server = api.create(Default::default());
            api.bind(&server, None, Some(server_port)).expect("failed to bind the server socket");
            api.listen(&server, backlog).expect("can listen");
            server
        });

        let client = net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            let socket = api.create(ProvidedBuffers::Buffers(WriteBackClientBuffers::default()));
            api.connect(&socket, Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip)), server_port)
                .expect("failed to connect");
            socket
        });
        // Step so that SYN is received by the server.
        assert!(!net.step().is_idle());
        // Make sure the server now has a pending socket in the accept queue.
        assert_matches!(
            &server.get().deref().socket_state,
            TcpSocketStateInner::Bound(BoundSocketState::Listener((
                MaybeListener::Listener(Listener {
                    accept_queue,
                    ..
                }), ..))) => {
                assert_eq!(accept_queue.ready_len(), 0);
                assert_eq!(accept_queue.pending_len(), 1);
            }
        );
        // Now close the client socket.
        net.with_context(LOCAL, |ctx| {
            let mut api = ctx.tcp_api::<I>();
            api.close(client);
        });
        // Server's SYN-ACK will get a RST response because the connection is
        // no longer there.
        net.run_until_idle();
        // We verify that no lingering socket in the accept_queue.
        assert_matches!(
            &server.get().deref().socket_state,
            TcpSocketStateInner::Bound(BoundSocketState::Listener((
                MaybeListener::Listener(Listener {
                    accept_queue,
                    ..
                }), ..))) => {
                assert_eq!(accept_queue.ready_len(), 0);
                assert_eq!(accept_queue.pending_len(), 0);
            }
        );
        // Server should be the only socket in `all_sockets`.
        net.with_context(REMOTE, |ctx| {
            ctx.core_ctx.with_all_sockets_mut(|all_sockets| {
                assert_eq!(all_sockets.keys().collect::<Vec<_>>(), [&server]);
            })
        })
    }
}
