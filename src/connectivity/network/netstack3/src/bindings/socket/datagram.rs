// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Datagram socket bindings.

use std::{
    convert::TryInto as _,
    fmt::Debug,
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU64, NonZeroU8, TryFromIntError},
    ops::ControlFlow,
    sync::Arc,
};

use either::Either;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_posix_socket as fposix_socket;

use assert_matches::assert_matches;
use derivative::Derivative;
use explicit::ResultExt as _;
use fidl::endpoints::RequestStream as _;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, prelude::HandleBased as _, Peered as _};
use net_types::{
    ip::{Ip, IpAddress, IpVersion, Ipv4, Ipv6},
    MulticastAddr, SpecifiedAddr, ZonedAddr,
};
use netstack3_core::{
    data_structures::id_map_collection::{IdMapCollection, IdMapCollectionKey},
    device::{DeviceId, WeakDeviceId},
    error::{LocalAddressError, SocketError},
    ip::{icmp, IpExt},
    socket::datagram::{
        ConnectError, MulticastInterfaceSelector, MulticastMembershipInterfaceSelector,
        SetMulticastMembershipError, ShutdownType,
    },
    sync::{Mutex as CoreMutex, RwLock as CoreRwLock},
    transport::udp::{self, ExpectedConnError, ExpectedUnboundError},
    BufferNonSyncContext, NonSyncContext, SyncCtx,
};
use packet::{Buf, BufferMut};
use tracing::{error, trace, warn};

use crate::bindings::{
    socket::{
        queue::{BodyLen, MessageQueue},
        worker::{self, SocketWorker},
    },
    trace_duration,
    util::{
        DeviceNotFoundError, IntoCore as _, IntoFidl, TryFromFidlWithContext, TryIntoCore,
        TryIntoCoreWithContext, TryIntoFidl, TryIntoFidlWithContext,
    },
    BindingsNonSyncCtxImpl, Ctx, DeviceIdExt as _, StaticCommonInfo,
};

use super::{
    IntoErrno, IpSockAddrExt, SockAddr, SocketWorkerProperties, ZXSIO_SIGNAL_INCOMING,
    ZXSIO_SIGNAL_OUTGOING,
};

/// The types of supported datagram protocols.
#[derive(Debug)]
pub(crate) enum DatagramProtocol {
    Udp,
}

/// A minimal abstraction over transport protocols that allows bindings-side state to be stored.
pub(crate) trait Transport<I>: Debug + Sized + Send + Sync + 'static {
    const PROTOCOL: DatagramProtocol;
    type SocketId: Debug + Copy + IdMapCollectionKey + Send + Sync;
}

/// Mapping from socket IDs to their receive queues.
///
/// Receive queues are shared between the collections here and the tasks
/// handling socket requests. Since `SocketCollection` implements [`udp::NonSyncContext`]
/// and [`udp::BufferNonSyncContext`], whose trait methods may be called from
/// within Core in a locked context, once one of the [`MessageQueue`]s is
/// locked, no calls may be made into [`netstack3_core`]. This prevents
/// a potential deadlock where Core is waiting for a `MessageQueue` to be
/// available and some bindings code here holds the `MessageQueue` and
/// attempts to lock Core state via a `netstack3_core` call.
pub(crate) struct SocketCollection<I: Ip, T: Transport<I>> {
    received: IdMapCollection<T::SocketId, Arc<CoreMutex<MessageQueue<AvailableMessage<I, T>>>>>,
}

impl<I: Ip, T: Transport<I>> Default for SocketCollection<I, T> {
    fn default() -> Self {
        Self { received: IdMapCollection::default() }
    }
}

pub(crate) struct SocketCollectionPair<T>
where
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
{
    v4: CoreRwLock<SocketCollection<Ipv4, T>>,
    v6: CoreRwLock<SocketCollection<Ipv6, T>>,
}

impl<T> Default for SocketCollectionPair<T>
where
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
{
    fn default() -> Self {
        Self { v4: Default::default(), v6: Default::default() }
    }
}

/// An extension trait that allows generic access to IP-specific state.
pub(crate) trait SocketCollectionIpExt<T>: Ip
where
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
    T: Transport<Self>,
{
    fn with_collection<
        D: AsRef<SocketCollectionPair<T>>,
        O,
        F: FnOnce(&SocketCollection<Self, T>) -> O,
    >(
        dispatcher: &D,
        cb: F,
    ) -> O;

    fn with_collection_mut<
        D: AsRef<SocketCollectionPair<T>>,
        O,
        F: FnOnce(&mut SocketCollection<Self, T>) -> O,
    >(
        dispatcher: &mut D,
        cb: F,
    ) -> O;
}

impl<T> SocketCollectionIpExt<T> for Ipv4
where
    T: 'static,
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
{
    fn with_collection<
        D: AsRef<SocketCollectionPair<T>>,
        O,
        F: FnOnce(&SocketCollection<Ipv4, T>) -> O,
    >(
        dispatcher: &D,
        cb: F,
    ) -> O {
        cb(&dispatcher.as_ref().v4.read())
    }

    fn with_collection_mut<
        D: AsRef<SocketCollectionPair<T>>,
        O,
        F: FnOnce(&mut SocketCollection<Ipv4, T>) -> O,
    >(
        dispatcher: &mut D,
        cb: F,
    ) -> O {
        cb(&mut dispatcher.as_ref().v4.write())
    }
}

impl<T> SocketCollectionIpExt<T> for Ipv6
where
    T: 'static,
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
{
    fn with_collection<
        D: AsRef<SocketCollectionPair<T>>,
        O,
        F: FnOnce(&SocketCollection<Ipv6, T>) -> O,
    >(
        dispatcher: &D,
        cb: F,
    ) -> O {
        cb(&dispatcher.as_ref().v6.read())
    }

    fn with_collection_mut<
        D: AsRef<SocketCollectionPair<T>>,
        O,
        F: FnOnce(&mut SocketCollection<Ipv6, T>) -> O,
    >(
        dispatcher: &mut D,
        cb: F,
    ) -> O {
        cb(&mut dispatcher.as_ref().v6.write())
    }
}

/// A special case of TryFrom that avoids the associated error type in generic contexts.
pub(crate) trait OptionFromU16: Sized {
    fn from_u16(_: u16) -> Option<Self>;
}

pub(crate) struct LocalAddress<I: Ip, D, L> {
    address: Option<ZonedAddr<I::Addr, D>>,
    identifier: Option<L>,
}
pub(crate) struct RemoteAddress<I: Ip, D, R> {
    address: ZonedAddr<I::Addr, D>,
    identifier: R,
}

/// An abstraction over transport protocols that allows generic manipulation of Core state.
pub(crate) trait TransportState<I: Ip>: Transport<I> + Send + Sync + 'static {
    type ConnectError: IntoErrno;
    type ListenError: IntoErrno;
    type DisconnectError: IntoErrno;
    type SetSocketDeviceError: IntoErrno;
    type SetMulticastMembershipError: IntoErrno;
    type SetReusePortError: IntoErrno;
    type ShutdownError: IntoErrno;
    type LocalIdentifier: OptionFromU16 + Into<u16> + Send;
    type RemoteIdentifier: OptionFromU16 + Into<u16> + Send;
    type SocketInfo<C: NonSyncContext>: IntoFidl<LocalAddress<I, WeakDeviceId<C>, Self::LocalIdentifier>>
        + TryIntoFidl<
            RemoteAddress<I, WeakDeviceId<C>, Self::RemoteIdentifier>,
            Error = fposix::Errno,
        >;

    fn create_unbound<C: NonSyncContext>(ctx: &SyncCtx<C>) -> Self::SocketId;

    fn connect<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        remote_ip: ZonedAddr<I::Addr, DeviceId<C>>,
        remote_id: Self::RemoteIdentifier,
    ) -> Result<Self::SocketId, Self::ConnectError>;

    fn bind<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        addr: Option<ZonedAddr<I::Addr, DeviceId<C>>>,
        port: Option<Self::LocalIdentifier>,
    ) -> Result<Self::SocketId, Self::ListenError>;

    fn disconnect<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
    ) -> Result<Self::SocketId, Self::DisconnectError>;

    fn shutdown<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
        which: ShutdownType,
    ) -> Result<(), Self::ShutdownError>;

    fn get_shutdown<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> Option<ShutdownType>;

    fn get_socket_info<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
    ) -> Self::SocketInfo<C>;

    fn remove<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: Self::SocketId,
    ) -> Self::SocketInfo<C>;

    fn set_socket_device<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        device: Option<&DeviceId<C>>,
    ) -> Result<(), Self::SetSocketDeviceError>;

    fn get_bound_device<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> Option<WeakDeviceId<C>>;

    fn set_reuse_port<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        reuse_port: bool,
    ) -> Result<(), Self::SetReusePortError>;

    fn get_reuse_port<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> bool;

    fn set_multicast_membership<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        multicast_group: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, DeviceId<C>>,
        want_membership: bool,
    ) -> Result<(), Self::SetMulticastMembershipError>;

    fn set_unicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        hop_limit: Option<NonZeroU8>,
    );

    fn set_multicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        hop_limit: Option<NonZeroU8>,
    );

    fn get_unicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> NonZeroU8;

    fn get_multicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> NonZeroU8;
}

/// An abstraction over transport protocols that allows data to be sent via the Core.
pub(crate) trait BufferTransportState<I: Ip, B: BufferMut>: TransportState<I> {
    type SendError: IntoErrno;
    type SendToError: IntoErrno;

    fn send<C: BufferNonSyncContext<B>>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        body: B,
    ) -> Result<(), (B, Self::SendError)>;

    fn send_to<C: BufferNonSyncContext<B>>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        remote: (ZonedAddr<I::Addr, DeviceId<C>>, Self::RemoteIdentifier),
        body: B,
    ) -> (Option<Self::SocketId>, Result<(), (B, Self::SendToError)>);
}

#[derive(Debug)]
pub(crate) enum Udp {}

impl<I: Ip> Transport<I> for Udp {
    const PROTOCOL: DatagramProtocol = DatagramProtocol::Udp;
    type SocketId = udp::SocketId<I>;
}

impl OptionFromU16 for NonZeroU16 {
    fn from_u16(t: u16) -> Option<Self> {
        Self::new(t)
    }
}

impl<I: IpExt> TransportState<I> for Udp {
    type ConnectError = ConnectError;
    type ListenError = Either<ExpectedUnboundError, LocalAddressError>;
    type DisconnectError = ExpectedConnError;
    type ShutdownError = ExpectedConnError;
    type SetSocketDeviceError = SocketError;
    type SetMulticastMembershipError = SetMulticastMembershipError;
    type SetReusePortError = ExpectedUnboundError;
    type LocalIdentifier = NonZeroU16;
    type RemoteIdentifier = NonZeroU16;
    type SocketInfo<C: NonSyncContext> = udp::SocketInfo<I::Addr, WeakDeviceId<C>>;

    fn create_unbound<C: NonSyncContext>(ctx: &SyncCtx<C>) -> Self::SocketId {
        udp::create_udp(ctx)
    }

    fn connect<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        remote_ip: ZonedAddr<<I as Ip>::Addr, DeviceId<C>>,
        remote_id: Self::RemoteIdentifier,
    ) -> Result<Self::SocketId, Self::ConnectError> {
        udp::connect(sync_ctx, ctx, id, remote_ip, remote_id)
    }

    fn bind<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        addr: Option<ZonedAddr<<I as Ip>::Addr, DeviceId<C>>>,
        port: Option<Self::LocalIdentifier>,
    ) -> Result<Self::SocketId, Self::ListenError> {
        udp::listen_udp(sync_ctx, ctx, id, addr, port)
    }

    fn disconnect<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
    ) -> Result<Self::SocketId, Self::DisconnectError> {
        udp::disconnect_udp_connected(sync_ctx, ctx, id)
    }

    fn shutdown<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
        which: ShutdownType,
    ) -> Result<(), Self::ShutdownError> {
        udp::shutdown(sync_ctx, ctx, id, which)
    }

    fn get_shutdown<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> Option<ShutdownType> {
        udp::get_shutdown(sync_ctx, ctx, id)
    }

    fn get_socket_info<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
    ) -> Self::SocketInfo<C> {
        udp::get_udp_info(sync_ctx, ctx, id)
    }

    fn remove<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: Self::SocketId,
    ) -> Self::SocketInfo<C> {
        udp::remove_udp(sync_ctx, ctx, id)
    }

    fn set_socket_device<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        device: Option<&DeviceId<C>>,
    ) -> Result<(), Self::SetSocketDeviceError> {
        udp::set_udp_device(sync_ctx, ctx, id, device)
    }

    fn get_bound_device<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> Option<WeakDeviceId<C>> {
        udp::get_udp_bound_device(sync_ctx, ctx, id)
    }

    fn set_reuse_port<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        reuse_port: bool,
    ) -> Result<(), Self::SetReusePortError> {
        match udp::set_udp_posix_reuse_port(sync_ctx, ctx, id, reuse_port) {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!("tried to set SO_REUSEPORT on a bound socket; see https://fxbug.dev/100840");
                Err(e)
            }
        }
    }

    fn get_reuse_port<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> bool {
        udp::get_udp_posix_reuse_port(sync_ctx, ctx, id)
    }

    fn set_multicast_membership<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        multicast_group: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, DeviceId<C>>,
        want_membership: bool,
    ) -> Result<(), Self::SetMulticastMembershipError> {
        udp::set_udp_multicast_membership(
            sync_ctx,
            ctx,
            id,
            multicast_group,
            interface,
            want_membership,
        )
    }

    fn set_unicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        hop_limit: Option<NonZeroU8>,
    ) {
        udp::set_udp_unicast_hop_limit(sync_ctx, ctx, id, hop_limit)
    }

    fn set_multicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        hop_limit: Option<NonZeroU8>,
    ) {
        udp::set_udp_multicast_hop_limit(sync_ctx, ctx, id, hop_limit)
    }

    fn get_unicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> NonZeroU8 {
        udp::get_udp_unicast_hop_limit(sync_ctx, ctx, id)
    }

    fn get_multicast_hop_limit<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        ctx: &C,
        id: &Self::SocketId,
    ) -> NonZeroU8 {
        udp::get_udp_multicast_hop_limit(sync_ctx, ctx, id)
    }
}

impl<I: IpExt + IpSockAddrExt, B: BufferMut> BufferTransportState<I, B> for Udp {
    type SendError = Either<udp::SendError, fposix::Errno>;
    type SendToError = Either<LocalAddressError, udp::SendToError>;

    fn send<C: BufferNonSyncContext<B>>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        body: B,
    ) -> Result<(), (B, Self::SendError)> {
        udp::send_udp(sync_ctx, ctx, id, body)
            .map_err(|(b, e)| (b, e.map_right(|ExpectedConnError| fposix::Errno::Edestaddrreq)))
    }

    fn send_to<C: BufferNonSyncContext<B>>(
        sync_ctx: &SyncCtx<C>,
        ctx: &mut C,
        id: &Self::SocketId,
        (remote_ip, remote_port): (ZonedAddr<<I as Ip>::Addr, DeviceId<C>>, Self::RemoteIdentifier),
        body: B,
    ) -> (Option<Self::SocketId>, Result<(), (B, Self::SendToError)>) {
        match udp::send_udp_to(sync_ctx, ctx, id, remote_ip, remote_port, body) {
            Ok(new_id) => (new_id, Ok(())),
            Err((body, Either::Left(e))) => (None, Err((body, Either::Left(e)))),
            Err((body, Either::Right((new_id, e)))) => (new_id, Err((body, Either::Right(e)))),
        }
    }
}

impl<I: icmp::IcmpIpExt> udp::NonSyncContext<I> for SocketCollection<I, Udp> {
    fn receive_icmp_error(&mut self, id: udp::SocketId<I>, err: I::ErrorCode) {
        warn!("unimplemented receive_icmp_error {:?} on {:?}", err, id)
    }
}

impl<I: IpExt, B: BufferMut> udp::BufferNonSyncContext<I, B> for SocketCollection<I, Udp> {
    fn receive_udp(
        &mut self,
        id: udp::SocketId<I>,
        _dst_ip: <I>::Addr,
        (src_ip, src_port): (<I>::Addr, Option<NonZeroU16>),
        body: &B,
    ) {
        let Self { received } = self;
        let queue = received.get(&id).unwrap();
        queue.lock().receive(IntoAvailableMessage(
            src_ip,
            src_port.map_or(0, NonZeroU16::get),
            body.as_ref(),
        ))
    }
}

#[derive(Debug, Derivative)]
#[derivative(Clone(bound = ""))]
struct AvailableMessage<I: Ip, T> {
    source_addr: I::Addr,
    source_port: u16,
    data: Vec<u8>,
    _marker: PhantomData<T>,
}

impl<I: Ip, T> BodyLen for AvailableMessage<I, T> {
    fn body_len(&self) -> usize {
        self.data.len()
    }
}

struct IntoAvailableMessage<'b, A>(A, u16, &'b [u8]);

impl<A: IpAddress> BodyLen for IntoAvailableMessage<'_, A> {
    fn body_len(&self) -> usize {
        let IntoAvailableMessage(_, _, body) = self;
        body.len()
    }
}

impl<I: Ip, T> From<IntoAvailableMessage<'_, I::Addr>> for AvailableMessage<I, T> {
    fn from(value: IntoAvailableMessage<'_, I::Addr>) -> Self {
        let IntoAvailableMessage(source_addr, source_port, body) = value;
        Self { source_addr, source_port, data: Vec::from(body), _marker: Default::default() }
    }
}

#[derive(Debug)]
struct BindingData<I: Ip, T: Transport<I>> {
    peer_event: zx::EventPair,
    info: SocketControlInfo<I, T>,
    /// The queue for messages received on this socket.
    ///
    /// The message queue is held here and also in the [`SocketCollection`]
    /// to which the socket belongs.
    messages: Arc<CoreMutex<MessageQueue<AvailableMessage<I, T>>>>,
}

impl<I, T> BindingData<I, T>
where
    I: SocketCollectionIpExt<T> + IpExt + IpSockAddrExt,
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
    T: TransportState<I>,
    BindingsNonSyncCtxImpl: RequestHandlerDispatcher<I, T>,
{
    /// Creates a new `BindingData`.
    fn new(
        sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        non_sync_ctx: &mut BindingsNonSyncCtxImpl,
        properties: SocketWorkerProperties,
    ) -> Self {
        let (local_event, peer_event) = zx::EventPair::create();
        // signal peer that OUTGOING is available.
        // TODO(brunodalbo): We're currently not enforcing any sort of
        // flow-control for outgoing datagrams. That'll get fixed once we
        // limit the number of in flight datagrams per socket (i.e. application
        // buffers).
        if let Err(e) = local_event.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_OUTGOING) {
            error!("socket failed to signal peer: {:?}", e);
        }
        let id = T::create_unbound(sync_ctx);
        let messages = Arc::new(CoreMutex::new(MessageQueue::new(local_event)));

        assert_matches!(
            I::with_collection_mut(non_sync_ctx, |c| c.received.insert(&id, messages.clone())),
            None
        );

        Self { peer_event, info: SocketControlInfo { _properties: properties, id }, messages }
    }
}

/// Information on socket control plane.
#[derive(Debug)]
pub(crate) struct SocketControlInfo<I: Ip, T: Transport<I>> {
    _properties: SocketWorkerProperties,
    id: T::SocketId,
}

pub(crate) trait SocketWorkerDispatcher:
    RequestHandlerDispatcher<Ipv4, Udp> + RequestHandlerDispatcher<Ipv6, Udp>
{
}

impl<T> SocketWorkerDispatcher for T
where
    T: RequestHandlerDispatcher<Ipv4, Udp>,
    T: RequestHandlerDispatcher<Ipv6, Udp>,
{
}

pub(super) fn spawn_worker(
    domain: fposix_socket::Domain,
    proto: fposix_socket::DatagramSocketProtocol,
    ctx: crate::bindings::Ctx,
    events: fposix_socket::SynchronousDatagramSocketRequestStream,
    properties: SocketWorkerProperties,
) -> Result<(), fposix::Errno> {
    match (domain, proto) {
        (fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp) => {
            fasync::Task::spawn(SocketWorker::serve_stream_with(
                ctx,
                BindingData::<Ipv4, Udp>::new,
                properties,
                events,
            ))
        }
        (fposix_socket::Domain::Ipv6, fposix_socket::DatagramSocketProtocol::Udp) => {
            fasync::Task::spawn(SocketWorker::serve_stream_with(
                ctx,
                BindingData::<Ipv6, Udp>::new,
                properties,
                events,
            ))
        }
        (
            fposix_socket::Domain::Ipv4 | fposix_socket::Domain::Ipv6,
            fposix_socket::DatagramSocketProtocol::IcmpEcho,
        ) => return Err(fposix::Errno::Enoprotoopt),
    }
    .detach();
    Ok(())
}

impl worker::CloseResponder for fposix_socket::SynchronousDatagramSocketCloseResponder {
    fn send(self, arg: Result<(), i32>) -> Result<(), fidl::Error> {
        fposix_socket::SynchronousDatagramSocketCloseResponder::send(self, arg)
    }
}

impl<I, T> worker::SocketWorkerHandler for BindingData<I, T>
where
    I: SocketCollectionIpExt<T> + IpExt + IpSockAddrExt,
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
    T: TransportState<I>,
    T: BufferTransportState<I, Buf<Vec<u8>>>,
    T: Send + Sync + 'static,
    DeviceId<BindingsNonSyncCtxImpl>:
        TryFromFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
    WeakDeviceId<BindingsNonSyncCtxImpl>:
        TryIntoFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
    BindingsNonSyncCtxImpl: RequestHandlerDispatcher<I, T>,
{
    type Request = fposix_socket::SynchronousDatagramSocketRequest;
    type RequestStream = fposix_socket::SynchronousDatagramSocketRequestStream;
    type CloseResponder = fposix_socket::SynchronousDatagramSocketCloseResponder;

    fn handle_request(
        &mut self,
        ctx: &Ctx,
        request: Self::Request,
    ) -> ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request)
    }

    fn close(
        self,
        sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    ) {
        let Self { info: SocketControlInfo { _properties, id }, messages: _, peer_event: _ } = self;
        let _: Option<_> = I::with_collection_mut(non_sync_ctx, |c| c.received.remove(&id));
        let _: T::SocketInfo<_> = T::remove(sync_ctx, non_sync_ctx, id);
    }
}

pub(crate) trait RequestHandlerDispatcher<I, T>: AsRef<SocketCollectionPair<T>>
where
    I: IpExt,
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
    T: Transport<I>,
{
}

impl<I, T, D> RequestHandlerDispatcher<I, T> for D
where
    I: IpExt,
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
    T: Transport<I>,
    D: AsRef<SocketCollectionPair<T>>,
{
}
/// A borrow into a [`SocketWorker`]'s state.
struct RequestHandler<'a, I: Ip, T: Transport<I>> {
    ctx: &'a crate::bindings::Ctx,
    data: &'a mut BindingData<I, T>,
}

impl<'a, I, T> RequestHandler<'a, I, T>
where
    I: SocketCollectionIpExt<T> + IpExt + IpSockAddrExt,
    T: Transport<Ipv4>,
    T: Transport<Ipv6>,
    T: TransportState<I>,
    T: BufferTransportState<I, Buf<Vec<u8>>>,
    T: Send + Sync + 'static,
    DeviceId<BindingsNonSyncCtxImpl>:
        TryFromFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
    WeakDeviceId<BindingsNonSyncCtxImpl>:
        TryIntoFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
    BindingsNonSyncCtxImpl: RequestHandlerDispatcher<I, T>,
{
    fn handle_request(
        mut self,
        request: fposix_socket::SynchronousDatagramSocketRequest,
    ) -> ControlFlow<
        fposix_socket::SynchronousDatagramSocketCloseResponder,
        Option<fposix_socket::SynchronousDatagramSocketRequestStream>,
    > {
        match request {
            fposix_socket::SynchronousDatagramSocketRequest::Describe { responder } => {
                responder_send!(responder, self.describe())
            }
            fposix_socket::SynchronousDatagramSocketRequest::Connect { addr, responder } => {
                responder_send!(responder, self.connect(addr));
            }
            fposix_socket::SynchronousDatagramSocketRequest::Disconnect { responder } => {
                responder_send!(responder, self.disconnect());
            }
            fposix_socket::SynchronousDatagramSocketRequest::Clone2 {
                request,
                control_handle: _,
            } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel())
                    .expect("failed to create async channel");
                let stream =
                    fposix_socket::SynchronousDatagramSocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(stream));
            }
            fposix_socket::SynchronousDatagramSocketRequest::Close { responder } => {
                return ControlFlow::Break(responder);
            }
            fposix_socket::SynchronousDatagramSocketRequest::Bind { addr, responder } => {
                responder_send!(responder, self.bind(addr));
            }
            fposix_socket::SynchronousDatagramSocketRequest::Query { responder } => {
                responder_send!(
                    responder,
                    fposix_socket::SYNCHRONOUS_DATAGRAM_SOCKET_PROTOCOL_NAME.as_bytes()
                );
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetSockName { responder } => {
                responder_send!(responder, self.get_sock_name().as_ref().map_err(|e| *e));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetPeerName { responder } => {
                responder_send!(responder, self.get_peer_name().as_ref().map_err(|e| *e));
            }
            fposix_socket::SynchronousDatagramSocketRequest::Shutdown { mode, responder } => {
                responder_send!(responder, self.shutdown(mode))
            }
            fposix_socket::SynchronousDatagramSocketRequest::RecvMsg {
                want_addr,
                data_len,
                // TODO(brunodalbo) handle control
                want_control: _,
                flags,
                responder,
            } => responder_send!(
                responder,
                match self.recv_msg(want_addr, data_len as usize, flags) {
                    Ok((ref addr, ref data, ref control, truncated)) =>
                        Ok((addr.as_ref(), data.as_slice(), control, truncated)),
                    Err(err) => Err(err),
                }
            ),
            fposix_socket::SynchronousDatagramSocketRequest::SendMsg {
                addr,
                data,
                control: _,
                flags: _,
                responder,
            } => {
                // TODO(https://fxbug.dev/21106): handle control.
                responder_send!(responder, self.send_msg(addr.map(|addr| *addr), data));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetInfo { responder } => {
                responder_send!(responder, self.get_sock_info())
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetTimestamp { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetTimestamp {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetError { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetSendBuffer {
                value_bytes: _,
                responder,
            } => {
                // TODO(https://fxbug.dev/123057): Actually implement SetSendBuffer.
                //
                // Currently, UDP sending in Netstack3 is synchronous, so it's not clear what a
                // sensible implementation would look like.
                responder_send!(responder, Ok(()));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetSendBuffer { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetReceiveBuffer {
                value_bytes,
                responder,
            } => {
                responder_send!(responder, {
                    self.set_max_receive_buffer_size(value_bytes);
                    Ok(())
                });
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetReceiveBuffer { responder } => {
                responder_send!(responder, Ok(self.get_max_receive_buffer_size()));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetReuseAddress {
                value: _,
                responder,
            } => {
                // ANVL's UDP test stub requires that setting SO_REUSEADDR succeeds.
                // Blindly return success here to unblock test coverage (possible since
                // the network test realm is restarted before each test case).
                // TODO(https://fxbug.dev/97823): Actually implement SetReuseAddress.
                responder_send!(responder, Ok(()));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetReuseAddress { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetReusePort { value, responder } => {
                responder_send!(responder, self.set_reuse_port(value));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetReusePort { responder } => {
                responder_send!(responder, Ok(self.get_reuse_port()));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetAcceptConn { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetBindToDevice {
                value,
                responder,
            } => {
                let identifier = (!value.is_empty()).then_some(value.as_str());
                responder_send!(responder, self.bind_to_device(identifier));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetBindToDevice { responder } => {
                responder_send!(
                    responder,
                    match self.get_bound_device() {
                        Ok(ref d) => Ok(d.as_deref().unwrap_or("")),
                        Err(e) => Err(e),
                    }
                )
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetBroadcast { value, responder } => {
                // We allow a no-op since the core does not yet support limiting
                // broadcast packets. Until we implement this, we leave this as a
                // no-op so that applications needing to send broadcast packets may
                // make progress.
                //
                // TODO(https://fxbug.dev/126299): Actually implement SO_BROADCAST.
                let response = if value { Ok(()) } else { Err(fposix::Errno::Eopnotsupp) };
                responder_send!(responder, response);
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetBroadcast { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetKeepAlive {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetKeepAlive { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetLinger {
                linger: _,
                length_secs: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetLinger { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetOutOfBandInline {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetOutOfBandInline { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetNoCheck { value: _, responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetNoCheck { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6Only { value, responder } => {
                // TODO(https://fxbug.dev/21198): support dual-stack sockets.
                responder_send!(
                    responder,
                    match I::VERSION {
                        IpVersion::V6 => value,
                        IpVersion::V4 => false,
                    }
                    .then_some(())
                    .ok_or(fposix::Errno::Eopnotsupp)
                );
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6Only { responder } => {
                // TODO(https://fxbug.dev/21198): support dual-stack
                // sockets.
                responder_send!(responder, Ok(true));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6TrafficClass {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6TrafficClass { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6MulticastInterface {
                value: _,
                responder,
            } => {
                warn!("TODO(https://fxbug.dev/107644): implement IPV6_MULTICAST_IF socket option");
                responder_send!(responder, Ok(()));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6MulticastInterface {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6UnicastHops {
                value,
                responder,
            } => {
                responder_send!(responder, self.set_unicast_hop_limit(Ipv6::VERSION, value))
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6UnicastHops { responder } => {
                responder_send!(responder, self.get_unicast_hop_limit(Ipv6::VERSION))
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6MulticastHops {
                value,
                responder,
            } => {
                responder_send!(responder, self.set_multicast_hop_limit(Ipv6::VERSION, value))
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6MulticastHops { responder } => {
                responder_send!(responder, self.get_multicast_hop_limit(Ipv6::VERSION))
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6MulticastLoopback {
                value,
                responder,
            } => {
                // TODO(https://fxbug.dev/106865): add support for
                // looping back sent packets.
                responder_send!(
                    responder,
                    (!value).then_some(()).ok_or(fposix::Errno::Enoprotoopt)
                );
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6MulticastLoopback {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpTtl { value, responder } => {
                responder_send!(responder, self.set_unicast_hop_limit(Ipv4::VERSION, value))
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpTtl { responder } => {
                responder_send!(responder, self.get_unicast_hop_limit(Ipv4::VERSION))
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpMulticastTtl {
                value,
                responder,
            } => {
                responder_send!(responder, self.set_multicast_hop_limit(Ipv4::VERSION, value))
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpMulticastTtl { responder } => {
                responder_send!(responder, self.get_multicast_hop_limit(Ipv4::VERSION))
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpMulticastInterface {
                iface: _,
                address: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpMulticastInterface {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpMulticastLoopback {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpMulticastLoopback {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpTypeOfService {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpTypeOfService { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::AddIpMembership {
                membership,
                responder,
            } => {
                responder_send!(responder, self.set_multicast_membership(membership, true));
            }
            fposix_socket::SynchronousDatagramSocketRequest::DropIpMembership {
                membership,
                responder,
            } => {
                responder_send!(responder, self.set_multicast_membership(membership, false));
            }
            fposix_socket::SynchronousDatagramSocketRequest::AddIpv6Membership {
                membership,
                responder,
            } => {
                responder_send!(responder, self.set_multicast_membership(membership, true));
            }
            fposix_socket::SynchronousDatagramSocketRequest::DropIpv6Membership {
                membership,
                responder,
            } => {
                responder_send!(responder, self.set_multicast_membership(membership, false));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6ReceiveTrafficClass {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6ReceiveTrafficClass {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6ReceiveHopLimit {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6ReceiveHopLimit {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpReceiveTypeOfService {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpReceiveTypeOfService {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpv6ReceivePacketInfo {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpv6ReceivePacketInfo {
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpReceiveTtl {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpReceiveTtl { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::SetIpPacketInfo {
                value: _,
                responder,
            } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
            fposix_socket::SynchronousDatagramSocketRequest::GetIpPacketInfo { responder } => {
                responder_send!(responder, Err(fposix::Errno::Eopnotsupp));
            }
        }
        ControlFlow::Continue(None)
    }

    fn describe(&self) -> fposix_socket::SynchronousDatagramSocketDescribeResponse {
        let Self { ctx: _, data: BindingData { peer_event, info: _, messages: _ } } = self;
        let peer = peer_event
            .duplicate_handle(
                // The peer doesn't need to be able to signal, just receive signals,
                // so attenuate that right when duplicating.
                zx::Rights::BASIC,
            )
            .expect("failed to duplicate");

        fposix_socket::SynchronousDatagramSocketDescribeResponse {
            event: Some(peer),
            ..Default::default()
        }
    }

    fn get_max_receive_buffer_size(&self) -> u64 {
        let Self { ctx: _, data: BindingData { peer_event: _, info: _, messages } } = self;
        messages.lock().max_available_messages_size().try_into().unwrap_or(u64::MAX)
    }

    fn set_max_receive_buffer_size(&mut self, max_bytes: u64) {
        let max_bytes = max_bytes.try_into().ok_checked::<TryFromIntError>().unwrap_or(usize::MAX);
        let Self { ctx: _, data: BindingData { peer_event: _, info: _, messages } } = self;
        messages.lock().set_max_available_messages_size(max_bytes)
    }

    /// Handles a [POSIX socket connect request].
    ///
    /// [POSIX socket connect request]: fposix_socket::SynchronousDatagramSocketRequest::Connect
    fn connect(self, addr: fnet::SocketAddress) -> Result<(), fposix::Errno> {
        let Self {
            ctx,
            data:
                BindingData {
                    peer_event: _,
                    messages: _,
                    info: SocketControlInfo { _properties: _, id },
                },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let sockaddr = I::SocketAddress::from_sock_addr(addr)?;
        trace!("connect sockaddr: {:?}", sockaddr);
        let (remote_addr, remote_port) =
            sockaddr.try_into_core_with_ctx(&non_sync_ctx).map_err(IntoErrno::into_errno)?;
        let remote_port =
            T::RemoteIdentifier::from_u16(remote_port).ok_or(fposix::Errno::Econnrefused)?;
        // Emulate Linux, which was emulating BSD, by treating the unspecified
        // remote address as localhost.
        let remote_addr = remote_addr.unwrap_or(ZonedAddr::Unzoned(I::LOOPBACK_ADDRESS));

        let new_id = T::connect(sync_ctx, non_sync_ctx, id, remote_addr, remote_port)
            .map_err(IntoErrno::into_errno)?;
        I::with_collection_mut(non_sync_ctx, |c| {
            let messages = c.received.remove(id).expect("had message queue");
            assert_matches!(c.received.insert(&new_id, messages), None);
        });
        *id = new_id;

        Ok(())
    }

    /// Handles a [POSIX socket bind request].
    ///
    /// [POSIX socket bind request]: fposix_socket::SynchronousDatagramSocketRequest::Bind
    fn bind(self, addr: fnet::SocketAddress) -> Result<(), fposix::Errno> {
        let sockaddr = I::SocketAddress::from_sock_addr(addr)?;
        trace!("bind sockaddr: {:?}", sockaddr);

        let Self {
            ctx,
            data:
                BindingData {
                    peer_event: _,
                    messages: _,
                    info: SocketControlInfo { _properties: _, id },
                },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        let (sockaddr, port) =
            TryFromFidlWithContext::try_from_fidl_with_ctx(&non_sync_ctx, sockaddr)
                .map_err(IntoErrno::into_errno)?;
        let local_port = T::LocalIdentifier::from_u16(port);

        let new_id = T::bind(sync_ctx, non_sync_ctx, id, sockaddr, local_port)
            .map_err(IntoErrno::into_errno)?;
        assert_matches!(
            I::with_collection_mut(non_sync_ctx, |c| {
                let messages = c.received.remove(&id).expect("has message queue");
                c.received.insert(&new_id, messages)
            }),
            None
        );
        *id = new_id;
        Ok(())
    }

    /// Handles a [POSIX socket disconnect request].
    ///
    /// [POSIX socket connect request]: fposix_socket::SynchronousDatagramSocketRequest::Disconnect
    fn disconnect(self) -> Result<(), fposix::Errno> {
        trace!("disconnect socket");

        let Self {
            ctx,
            data:
                BindingData {
                    peer_event: _,
                    messages: _,
                    info: SocketControlInfo { _properties: _, id },
                },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        let new_id = T::disconnect(sync_ctx, non_sync_ctx, id).map_err(IntoErrno::into_errno)?;
        I::with_collection_mut(non_sync_ctx, |c| {
            let messages = c.received.remove(&id).expect("has message queue");
            assert_matches!(c.received.insert(&new_id, messages), None);
        });
        *id = new_id;
        Ok(())
    }

    /// Handles a [POSIX socket get_sock_name request].
    ///
    /// [POSIX socket get_sock_name request]: fposix_socket::SynchronousDatagramSocketRequest::GetSockName
    fn get_sock_name(self) -> Result<fnet::SocketAddress, fposix::Errno> {
        let Self {
            ctx,
            data:
                BindingData {
                    peer_event: _,
                    messages: _,
                    info: SocketControlInfo { _properties: _, id },
                },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        let l: LocalAddress<_, _, _> = T::get_socket_info(sync_ctx, non_sync_ctx, id).into_fidl();
        l.try_into_fidl_with_ctx(non_sync_ctx).map(SockAddr::into_sock_addr)
    }

    /// Handles a [POSIX socket get_info request].
    ///
    /// [POSIX socket get_info request]: fposix_socket::SynchronousDatagramSocketRequest::GetInfo
    fn get_sock_info(
        self,
    ) -> Result<(fposix_socket::Domain, fposix_socket::DatagramSocketProtocol), fposix::Errno> {
        let domain = match I::VERSION {
            IpVersion::V4 => fposix_socket::Domain::Ipv4,
            IpVersion::V6 => fposix_socket::Domain::Ipv6,
        };
        let protocol = match <T as Transport<I>>::PROTOCOL {
            DatagramProtocol::Udp => fposix_socket::DatagramSocketProtocol::Udp,
        };

        Ok((domain, protocol))
    }

    /// Handles a [POSIX socket get_peer_name request].
    ///
    /// [POSIX socket get_peer_name request]: fposix_socket::SynchronousDatagramSocketRequest::GetPeerName
    fn get_peer_name(self) -> Result<fnet::SocketAddress, fposix::Errno> {
        let Self {
            ctx,
            data:
                BindingData {
                    peer_event: _,
                    messages: _,
                    info: SocketControlInfo { _properties: _, id },
                },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        T::get_socket_info(sync_ctx, non_sync_ctx, id).try_into_fidl().and_then(
            |r: RemoteAddress<_, _, _>| {
                r.try_into_fidl_with_ctx(non_sync_ctx).map(SockAddr::into_sock_addr)
            },
        )
    }

    fn recv_msg(
        self,
        want_addr: bool,
        data_len: usize,
        recv_flags: fposix_socket::RecvMsgFlags,
    ) -> Result<
        (Option<fnet::SocketAddress>, Vec<u8>, fposix_socket::DatagramSocketRecvControlData, u32),
        fposix::Errno,
    > {
        trace_duration!("datagram::recv_msg");

        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let Ctx { sync_ctx, non_sync_ctx } = ctx;
        let front = I::with_collection(non_sync_ctx, |c| {
            let messages = c.received.get(&id).expect("has queue");
            let mut messages = messages.lock();
            if recv_flags.contains(fposix_socket::RecvMsgFlags::PEEK) {
                messages.peek().cloned()
            } else {
                messages.pop()
            }
        });

        let available = match front {
            None => {
                // This is safe from races only because the setting of the
                // shutdown flag can only be done by the worker executing this
                // code. Otherwise, a packet being delivered, followed by
                // another thread setting the shutdown flag, then this check
                // executing, could result in a race that causes this this code
                // to signal EOF with a packet still waiting.
                let shutdown = T::get_shutdown(sync_ctx, non_sync_ctx, id);
                return match shutdown {
                    Some(ShutdownType::Receive | ShutdownType::SendAndReceive) => {
                        // Return empty data to signal EOF.
                        Ok((
                            None,
                            Vec::new(),
                            fposix_socket::DatagramSocketRecvControlData::default(),
                            0,
                        ))
                    }
                    None | Some(ShutdownType::Send) => Err(fposix::Errno::Eagain),
                };
            }
            Some(front) => front,
        };
        let addr = want_addr.then(|| {
            I::SocketAddress::new(
                SpecifiedAddr::new(available.source_addr).map(ZonedAddr::Unzoned),
                available.source_port,
            )
            .into_sock_addr()
        });
        let mut data = available.data;
        let truncated = data.len().saturating_sub(data_len);
        data.truncate(data_len);

        Ok((addr, data, Default::default(), truncated.try_into().unwrap_or(u32::MAX)))
    }

    fn send_msg(
        self,
        addr: Option<fnet::SocketAddress>,
        data: Vec<u8>,
    ) -> Result<i64, fposix::Errno> {
        trace_duration!("datagram::send_msg");

        let remote_addr = addr.map(I::SocketAddress::from_sock_addr).transpose()?;
        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let remote = remote_addr
            .map(|remote_addr| {
                let (remote_addr, port) =
                    TryFromFidlWithContext::try_from_fidl_with_ctx(&non_sync_ctx, remote_addr)
                        .map_err(IntoErrno::into_errno)?;
                // Emulate Linux, which was emulating BSD, by treating the
                // unspecified remote address as localhost.
                Ok((
                    remote_addr.unwrap_or(ZonedAddr::Unzoned(I::LOOPBACK_ADDRESS)),
                    T::RemoteIdentifier::from_u16(port).ok_or(fposix::Errno::Einval)?,
                ))
            })
            .transpose()?;
        let len = data.len() as i64;
        let body = Buf::new(data, ..);
        match remote {
            Some(remote) => {
                let (new_id, r) = T::send_to(sync_ctx, non_sync_ctx, id, remote, body);
                // T::send_to can implicitly bind the socket, in which case the
                // old ID is no longer valid. Handle the update even if an error
                // was returned.
                if let Some(new_id) = new_id {
                    I::with_collection_mut(non_sync_ctx, |c| {
                        let messages = c.received.remove(&id).expect("has queue");
                        assert_matches!(c.received.insert(&new_id, messages), None);
                    });
                    *id = new_id;
                }
                r.map_err(|(_body, e)| e.into_errno())
            }
            None => T::send(sync_ctx, non_sync_ctx, id, body).map_err(|(_body, e)| e.into_errno()),
        }
        .map(|()| len)
    }

    fn bind_to_device(self, device: Option<&str>) -> Result<(), fposix::Errno> {
        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let device = device
            .map(|name| {
                non_sync_ctx
                    .devices
                    .get_device_by_name(name)
                    .map(|d| d.clone())
                    .ok_or(fposix::Errno::Enodev)
            })
            .transpose()?;

        T::set_socket_device(sync_ctx, non_sync_ctx, id, device.as_ref())
            .map_err(IntoErrno::into_errno)
    }

    fn get_bound_device(self) -> Result<Option<String>, fposix::Errno> {
        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let Ctx { sync_ctx, non_sync_ctx } = &ctx;

        let device = match T::get_bound_device(sync_ctx, non_sync_ctx, id) {
            None => return Ok(None),
            Some(d) => d,
        };
        device
            .upgrade()
            .map(|core_id| {
                let state = core_id.external_state();
                let StaticCommonInfo { binding_id: _, name } = state.static_common_info();
                Some(name.to_string())
            })
            .ok_or(fposix::Errno::Enodev)
    }

    fn set_reuse_port(self, reuse_port: bool) -> Result<(), fposix::Errno> {
        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        T::set_reuse_port(sync_ctx, non_sync_ctx, id, reuse_port).map_err(IntoErrno::into_errno)
    }

    fn get_reuse_port(self) -> bool {
        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let Ctx { sync_ctx, non_sync_ctx } = &ctx;

        T::get_reuse_port(sync_ctx, non_sync_ctx, id)
    }

    fn shutdown(self, how: fposix_socket::ShutdownMode) -> Result<(), fposix::Errno> {
        let Self {
            data:
                BindingData { peer_event: _, info: SocketControlInfo { id, _properties }, messages },
            ctx,
        } = self;
        let Ctx { sync_ctx, non_sync_ctx } = ctx;
        let how = match (
            how.contains(fposix_socket::ShutdownMode::READ),
            how.contains(fposix_socket::ShutdownMode::WRITE),
        ) {
            (true, true) => ShutdownType::SendAndReceive,
            (false, true) => ShutdownType::Send,
            (true, false) => ShutdownType::Receive,
            (false, false) => return Err(fposix::Errno::Einval),
        };
        T::shutdown(sync_ctx, non_sync_ctx, id, how).map_err(IntoErrno::into_errno)?;
        match how {
            ShutdownType::Receive | ShutdownType::SendAndReceive => {
                // Make sure to signal the peer so any ongoing call to
                // receive that is waiting for a signal will poll again.
                if let Err(e) = messages
                    .lock()
                    .local_event()
                    .signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_INCOMING)
                {
                    error!("Failed to signal peer when shutting down: {:?}", e);
                }
            }
            ShutdownType::Send => (),
        }

        Ok(())
    }

    fn set_multicast_membership<
        M: TryIntoCore<(
            MulticastAddr<I::Addr>,
            Option<MulticastInterfaceSelector<I::Addr, NonZeroU64>>,
        )>,
    >(
        self,
        membership: M,
        want_membership: bool,
    ) -> Result<(), fposix::Errno>
    where
        M::Error: IntoErrno,
    {
        let (multicast_group, interface) =
            membership.try_into_core().map_err(IntoErrno::into_errno)?;
        let interface = interface
            .map_or(MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute, Into::into);

        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        let interface =
            interface.try_into_core_with_ctx(non_sync_ctx).map_err(IntoErrno::into_errno)?;

        T::set_multicast_membership(
            sync_ctx,
            non_sync_ctx,
            id,
            multicast_group,
            interface,
            want_membership,
        )
        .map_err(IntoErrno::into_errno)
    }

    fn set_unicast_hop_limit(
        self,
        ip_version: IpVersion,
        hop_limit: fposix_socket::OptionalUint8,
    ) -> Result<(), fposix::Errno> {
        // TODO(https://fxbug.dev/21198): Allow setting hop limits for
        // dual-stack sockets.
        if ip_version != I::VERSION {
            return Err(fposix::Errno::Enoprotoopt);
        }

        let hop_limit: Option<u8> = hop_limit.into_core();
        let hop_limit =
            hop_limit.map(|u| NonZeroU8::new(u).ok_or(fposix::Errno::Einval)).transpose()?;

        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        T::set_unicast_hop_limit(sync_ctx, non_sync_ctx, id, hop_limit);
        Ok(())
    }

    fn set_multicast_hop_limit(
        self,
        ip_version: IpVersion,
        hop_limit: fposix_socket::OptionalUint8,
    ) -> Result<(), fposix::Errno> {
        // TODO(https://fxbug.dev/21198): Allow setting hop limits for
        // dual-stack sockets.
        if ip_version != I::VERSION {
            return Err(fposix::Errno::Enoprotoopt);
        }

        let hop_limit: Option<u8> = hop_limit.into_core();
        // TODO(https://fxbug.dev/108323): Support setting a multicast hop limit
        // of 0.
        let hop_limit =
            hop_limit.map(|u| NonZeroU8::new(u).ok_or(fposix::Errno::Einval)).transpose()?;

        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        T::set_multicast_hop_limit(sync_ctx, non_sync_ctx, id, hop_limit);
        Ok(())
    }

    fn get_unicast_hop_limit(self, ip_version: IpVersion) -> Result<u8, fposix::Errno> {
        // TODO(https://fxbug.dev/21198): Allow reading hop limits for
        // dual-stack sockets.
        if ip_version != I::VERSION {
            return Err(fposix::Errno::Enoprotoopt);
        }

        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let Ctx { sync_ctx, non_sync_ctx } = &ctx;
        Ok(T::get_unicast_hop_limit(sync_ctx, non_sync_ctx, id).get())
    }

    fn get_multicast_hop_limit(self, ip_version: IpVersion) -> Result<u8, fposix::Errno> {
        // TODO(https://fxbug.dev/21198): Allow reading hop limits for
        // dual-stack sockets.
        if ip_version != I::VERSION {
            return Err(fposix::Errno::Enoprotoopt);
        }

        let Self {
            ctx,
            data:
                BindingData { peer_event: _, info: SocketControlInfo { _properties, id }, messages: _ },
        } = self;
        let Ctx { sync_ctx, non_sync_ctx } = &ctx;

        Ok(T::get_multicast_hop_limit(sync_ctx, non_sync_ctx, id).get())
    }
}
impl IntoErrno for ExpectedUnboundError {
    fn into_errno(self) -> fposix::Errno {
        let ExpectedUnboundError = self;
        fposix::Errno::Einval
    }
}

impl IntoErrno for ExpectedConnError {
    fn into_errno(self) -> fposix::Errno {
        let ExpectedConnError = self;
        fposix::Errno::Enotconn
    }
}

impl<I: Ip, D> IntoFidl<LocalAddress<I, D, NonZeroU16>> for udp::SocketInfo<I::Addr, D> {
    fn into_fidl(self) -> LocalAddress<I, D, NonZeroU16> {
        let (local_ip, local_port) = match self {
            Self::Unbound => (None, None),
            Self::Listener(udp::ListenerInfo { local_ip, local_port }) => {
                (local_ip, Some(local_port))
            }
            Self::Connected(udp::ConnInfo {
                local_ip,
                local_port,
                remote_ip: _,
                remote_port: _,
            }) => (Some(local_ip), Some(local_port)),
        };
        LocalAddress { address: local_ip, identifier: local_port }
    }
}

impl<I: Ip, D> TryIntoFidl<RemoteAddress<I, D, NonZeroU16>> for udp::SocketInfo<I::Addr, D> {
    type Error = fposix::Errno;
    fn try_into_fidl(self) -> Result<RemoteAddress<I, D, NonZeroU16>, Self::Error> {
        match self {
            Self::Unbound | Self::Listener(_) => Err(fposix::Errno::Enotconn),
            Self::Connected(udp::ConnInfo {
                local_ip: _,
                local_port: _,
                remote_ip,
                remote_port,
            }) => Ok(RemoteAddress { address: remote_ip, identifier: remote_port }),
        }
    }
}

impl<I: IpSockAddrExt, D, L: Into<u16>> TryIntoFidlWithContext<I::SocketAddress>
    for LocalAddress<I, D, L>
where
    D: TryIntoFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
{
    type Error = fposix::Errno;

    fn try_into_fidl_with_ctx<Ctx: crate::bindings::util::ConversionContext>(
        self,
        ctx: &Ctx,
    ) -> Result<I::SocketAddress, Self::Error> {
        let Self { address, identifier } = self;
        (address, identifier.map_or(0, Into::into))
            .try_into_fidl_with_ctx(ctx)
            .map_err(IntoErrno::into_errno)
    }
}

impl<I: IpSockAddrExt, D, R: Into<u16>> TryIntoFidlWithContext<I::SocketAddress>
    for RemoteAddress<I, D, R>
where
    D: TryIntoFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
{
    type Error = fposix::Errno;

    fn try_into_fidl_with_ctx<Ctx: crate::bindings::util::ConversionContext>(
        self,
        ctx: &Ctx,
    ) -> Result<I::SocketAddress, Self::Error> {
        let Self { address, identifier } = self;
        (Some(address), identifier.into())
            .try_into_fidl_with_ctx(ctx)
            .map_err(IntoErrno::into_errno)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use anyhow::Error;
    use fidl::endpoints::{Proxy, ServerEnd};
    use fuchsia_async as fasync;
    use fuchsia_zircon::{self as zx, AsHandleRef};
    use futures::StreamExt;

    use crate::bindings::socket::{
        queue::MIN_OUTSTANDING_APPLICATION_MESSAGES_SIZE, testutil::TestSockAddr,
    };
    use crate::bindings::{
        integration_tests::{
            test_ep_name, StackSetupBuilder, TestSetup, TestSetupBuilder, TestStack,
        },
        util::IntoFidl,
    };
    use net_types::{
        ip::{Ip, IpAddr, IpAddress},
        Witness as _,
    };

    async fn prepare_test<A: TestSockAddr>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) -> (TestSetup, fposix_socket::SynchronousDatagramSocketProxy, zx::EventPair) {
        let mut t = TestSetupBuilder::new()
            .add_endpoint()
            .add_stack(
                StackSetupBuilder::new()
                    .add_named_endpoint(test_ep_name(1), Some(A::config_addr_subnet())),
            )
            .build()
            .await
            .unwrap();
        let (proxy, event) = get_socket_and_event::<A>(t.get(0), proto).await;
        (t, proxy, event)
    }

    async fn get_socket<A: TestSockAddr>(
        test_stack: &mut TestStack,
        proto: fposix_socket::DatagramSocketProtocol,
    ) -> fposix_socket::SynchronousDatagramSocketProxy {
        let socket_provider = test_stack.connect_socket_provider().unwrap();
        let response = socket_provider
            .datagram_socket(A::DOMAIN, proto)
            .await
            .unwrap()
            .expect("Socket succeeds");
        match response {
            fposix_socket::ProviderDatagramSocketResponse::SynchronousDatagramSocket(sock) => {
                fposix_socket::SynchronousDatagramSocketProxy::new(
                    fasync::Channel::from_channel(sock.into_channel()).unwrap(),
                )
            }
            // TODO(https://fxrev.dev/99905): Implement Fast UDP sockets in Netstack3.
            fposix_socket::ProviderDatagramSocketResponse::DatagramSocket(sock) => {
                let _: fidl::endpoints::ClientEnd<fposix_socket::DatagramSocketMarker> = sock;
                panic!("expected SynchronousDatagramSocket, found DatagramSocket")
            }
        }
    }

    async fn get_socket_and_event<A: TestSockAddr>(
        test_stack: &mut TestStack,
        proto: fposix_socket::DatagramSocketProtocol,
    ) -> (fposix_socket::SynchronousDatagramSocketProxy, zx::EventPair) {
        let ctlr = get_socket::<A>(test_stack, proto).await;
        let fposix_socket::SynchronousDatagramSocketDescribeResponse { event, .. } =
            ctlr.describe().await.expect("describe succeeds");
        (ctlr, event.expect("Socket describe contains event"))
    }

    macro_rules! declare_tests {
        ($test_fn:ident, icmp $(#[$icmp_attributes:meta])*) => {
            mod $test_fn {
                use super::*;

                #[fasync::run_singlethreaded(test)]
                async fn udp_v4() {
                    $test_fn::<fnet::Ipv4SocketAddress, Udp>(
                        fposix_socket::DatagramSocketProtocol::Udp,
                    )
                    .await
                }

                #[fasync::run_singlethreaded(test)]
                async fn udp_v6() {
                    $test_fn::<fnet::Ipv6SocketAddress, Udp>(
                        fposix_socket::DatagramSocketProtocol::Udp,
                    )
                    .await
                }
            }
        };
        ($test_fn:ident) => {
            declare_tests!($test_fn, icmp);
        };
    }

    async fn connect_failure<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;

        // Pass a bad domain.
        let res = proxy
            .connect(&A::DifferentDomain::create(A::DifferentDomain::LOCAL_ADDR, 1010))
            .await
            .unwrap()
            .expect_err("connect fails");
        assert_eq!(res, fposix::Errno::Eafnosupport);

        // Pass a zero port. UDP disallows it, ICMP allows it.
        let res = proxy.connect(&A::create(A::LOCAL_ADDR, 0)).await.unwrap();
        match proto {
            fposix_socket::DatagramSocketProtocol::Udp => {
                assert_eq!(res, Err(fposix::Errno::Econnrefused));
            }
            fposix_socket::DatagramSocketProtocol::IcmpEcho => {
                todo!("https://fxbug.dev/125482: implement ICMP sockets")
            }
        };

        // Pass an unreachable address (tests error forwarding from `create_connection`).
        let res = proxy
            .connect(&A::create(A::UNREACHABLE_ADDR, 1010))
            .await
            .unwrap()
            .expect_err("connect fails");
        assert_eq!(res, fposix::Errno::Enetunreach);
    }

    declare_tests!(
        connect_failure,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn connect<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;
        let () = proxy
            .connect(&A::create(A::REMOTE_ADDR, 200))
            .await
            .unwrap()
            .expect("connect succeeds");

        // Can connect again to a different remote should succeed.
        let () = proxy
            .connect(&A::create(A::REMOTE_ADDR_2, 200))
            .await
            .unwrap()
            .expect("connect suceeds");
    }

    declare_tests!(
        connect,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn connect_loopback<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;
        let () = proxy
            .connect(&A::create(
                <<A::AddrType as IpAddress>::Version as Ip>::LOOPBACK_ADDRESS.get(),
                200,
            ))
            .await
            .unwrap()
            .expect("connect succeeds");
    }

    declare_tests!(connect_loopback);

    async fn connect_any<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        // Pass an unspecified remote address. This should be treated as the
        // loopback address.
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;

        const PORT: u16 = 1010;
        let () = proxy
            .connect(&A::create(<A::AddrType as IpAddress>::Version::UNSPECIFIED_ADDRESS, PORT))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            proxy.get_peer_name().await.unwrap().unwrap(),
            A::create(<A::AddrType as IpAddress>::Version::LOOPBACK_ADDRESS.get(), PORT)
        );
    }

    declare_tests!(
        connect_any,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn bind<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        let (mut t, socket, _event) = prepare_test::<A>(proto).await;
        let stack = t.get(0);
        // Can bind to local address.
        let () = socket.bind(&A::create(A::LOCAL_ADDR, 200)).await.unwrap().expect("bind succeeds");

        // Can't bind again (to another port).
        let res =
            socket.bind(&A::create(A::LOCAL_ADDR, 201)).await.unwrap().expect_err("bind fails");
        assert_eq!(res, fposix::Errno::Einval);

        // Can bind another socket to a different port.
        let socket = get_socket::<A>(stack, proto).await;
        let () = socket.bind(&A::create(A::LOCAL_ADDR, 201)).await.unwrap().expect("bind succeeds");

        // Can bind to unspecified address in a different port.
        let socket = get_socket::<A>(stack, proto).await;
        let () = socket
            .bind(&A::create(<A::AddrType as IpAddress>::Version::UNSPECIFIED_ADDRESS, 202))
            .await
            .unwrap()
            .expect("bind succeeds");
    }

    declare_tests!(bind,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn bind_then_connect<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        let (_t, socket, _event) = prepare_test::<A>(proto).await;
        // Can bind to local address.
        let () = socket.bind(&A::create(A::LOCAL_ADDR, 200)).await.unwrap().expect("bind suceeds");

        let () = socket
            .connect(&A::create(A::REMOTE_ADDR, 1010))
            .await
            .unwrap()
            .expect("connect succeeds");
    }

    declare_tests!(
        bind_then_connect,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn connect_then_disconnect<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let (_t, socket, _event) = prepare_test::<A>(proto).await;

        let remote_addr = A::create(A::REMOTE_ADDR, 1010);
        let () = socket.connect(&remote_addr).await.unwrap().expect("connect succeeds");

        assert_eq!(
            socket.get_peer_name().await.unwrap().expect("get_peer_name should suceed"),
            remote_addr
        );
        let () = socket.disconnect().await.unwrap().expect("disconnect succeeds");

        assert_eq!(
            socket.get_peer_name().await.unwrap().expect_err("alice getpeername fails"),
            fposix::Errno::Enotconn
        );
    }

    declare_tests!(connect_then_disconnect,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    /// Tests a simple UDP setup with a client and a server, where the client
    /// can send data to the server and the server receives it.
    // TODO(https://fxbug.dev/47321): this test is incorrect for ICMP sockets. At the time of this
    // writing it crashes before reaching the wrong parts, but we will need to specialize the body
    // of this test for ICMP before calling the feature complete.
    async fn hello<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        // We create two stacks, Alice (server listening on LOCAL_ADDR:200), and
        // Bob (client, bound on REMOTE_ADDR:300). After setup, Bob connects to
        // Alice and sends a datagram. Finally, we verify that Alice receives
        // the datagram.
        let mut t = TestSetupBuilder::new()
            .add_endpoint()
            .add_endpoint()
            .add_stack(
                StackSetupBuilder::new()
                    .add_named_endpoint(test_ep_name(1), Some(A::config_addr_subnet())),
            )
            .add_stack(
                StackSetupBuilder::new()
                    .add_named_endpoint(test_ep_name(2), Some(A::config_addr_subnet_remote())),
            )
            .build()
            .await
            .unwrap();
        let alice = t.get(0);
        let (alice_socket, alice_events) = get_socket_and_event::<A>(alice, proto).await;

        // Verify that Alice has no local or peer addresses bound
        assert_eq!(
            alice_socket.get_sock_name().await.unwrap().unwrap(),
            A::new(None, 0).into_sock_addr(),
        );
        assert_eq!(
            alice_socket.get_peer_name().await.unwrap().expect_err("alice getpeername fails"),
            fposix::Errno::Enotconn
        );

        // Setup Alice as a server, bound to LOCAL_ADDR:200
        println!("Configuring alice...");
        let () = alice_socket
            .bind(&A::create(A::LOCAL_ADDR, 200))
            .await
            .unwrap()
            .expect("alice bind suceeds");

        // Verify that Alice is listening on the local socket, but still has no
        // peer socket
        assert_eq!(
            alice_socket.get_sock_name().await.unwrap().expect("alice getsockname succeeds"),
            A::create(A::LOCAL_ADDR, 200)
        );
        assert_eq!(
            alice_socket.get_peer_name().await.unwrap().expect_err("alice getpeername should fail"),
            fposix::Errno::Enotconn
        );

        // check that alice has no data to read, and it'd block waiting for
        // events:
        assert_eq!(
            alice_socket
                .recv_msg(false, 2048, false, fposix_socket::RecvMsgFlags::empty())
                .await
                .unwrap()
                .expect_err("Reading from alice should fail"),
            fposix::Errno::Eagain
        );
        assert_eq!(
            alice_events
                .wait_handle(ZXSIO_SIGNAL_INCOMING, zx::Time::from_nanos(0))
                .expect_err("Alice incoming event should not be signaled"),
            zx::Status::TIMED_OUT
        );

        // Setup Bob as a client, bound to REMOTE_ADDR:300
        println!("Configuring bob...");
        let bob = t.get(1);
        let (bob_socket, bob_events) = get_socket_and_event::<A>(bob, proto).await;
        let () = bob_socket
            .bind(&A::create(A::REMOTE_ADDR, 300))
            .await
            .unwrap()
            .expect("bob bind suceeds");

        // Verify that Bob is listening on the local socket, but has no peer
        // socket
        assert_eq!(
            bob_socket.get_sock_name().await.unwrap().expect("bob getsockname suceeds"),
            A::create(A::REMOTE_ADDR, 300)
        );
        assert_eq!(
            bob_socket
                .get_peer_name()
                .await
                .unwrap()
                .expect_err("get peer name should fail before connected"),
            fposix::Errno::Enotconn
        );

        // Connect Bob to Alice on LOCAL_ADDR:200
        println!("Connecting bob to alice...");
        let () = bob_socket
            .connect(&A::create(A::LOCAL_ADDR, 200))
            .await
            .unwrap()
            .expect("Connect succeeds");

        // Verify that Bob has the peer socket set correctly
        assert_eq!(
            bob_socket.get_peer_name().await.unwrap().expect("bob getpeername suceeds"),
            A::create(A::LOCAL_ADDR, 200)
        );

        // We don't care which signals are on, only that SIGNAL_OUTGOING is, we
        // can ignore the return value.
        let _signals = bob_events
            .wait_handle(ZXSIO_SIGNAL_OUTGOING, zx::Time::from_nanos(0))
            .expect("Bob outgoing event should be signaled");

        // Send datagram from Bob's socket.
        println!("Writing datagram to bob");
        let body = "Hello".as_bytes();
        assert_eq!(
            bob_socket
                .send_msg(
                    None,
                    &body,
                    &fposix_socket::DatagramSocketSendControlData::default(),
                    fposix_socket::SendMsgFlags::empty()
                )
                .await
                .unwrap()
                .expect("sendmsg suceeds"),
            body.len() as i64
        );

        // Wait for datagram to arrive on Alice's socket:

        println!("Waiting for signals");
        assert_eq!(
            fasync::OnSignals::new(&alice_events, ZXSIO_SIGNAL_INCOMING).await,
            Ok(ZXSIO_SIGNAL_INCOMING | ZXSIO_SIGNAL_OUTGOING)
        );

        let (from, data, _, truncated) = alice_socket
            .recv_msg(true, 2048, false, fposix_socket::RecvMsgFlags::empty())
            .await
            .unwrap()
            .expect("recvmsg suceeeds");
        let source = A::from_sock_addr(*from.expect("socket address returned"))
            .expect("bad socket address return");
        assert_eq!(source.addr(), A::REMOTE_ADDR);
        assert_eq!(source.port(), 300);
        assert_eq!(truncated, 0);
        assert_eq!(&data[..], body);
    }

    declare_tests!(
        hello,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn socket_describe(
        domain: fposix_socket::Domain,
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let mut t = TestSetupBuilder::new().add_endpoint().add_empty_stack().build().await.unwrap();
        let test_stack = t.get(0);
        let socket_provider = test_stack.connect_socket_provider().unwrap();
        let response = socket_provider
            .datagram_socket(domain, proto)
            .await
            .unwrap()
            .expect("Socket call succeeds");
        let socket = match response {
            fposix_socket::ProviderDatagramSocketResponse::SynchronousDatagramSocket(sock) => sock,
            // TODO(https://fxrev.dev/99905): Implement Fast UDP sockets in Netstack3.
            fposix_socket::ProviderDatagramSocketResponse::DatagramSocket(sock) => {
                let _: fidl::endpoints::ClientEnd<fposix_socket::DatagramSocketMarker> = sock;
                panic!("expected SynchronousDatagramSocket, found DatagramSocket")
            }
        };
        let fposix_socket::SynchronousDatagramSocketDescribeResponse { event, .. } =
            socket.into_proxy().unwrap().describe().await.expect("Describe call succeeds");
        let _: zx::EventPair = event.expect("Describe call returns event");
    }

    #[fasync::run_singlethreaded(test)]
    async fn udp_v4_socket_describe() {
        socket_describe(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
            .await
    }

    #[fasync::run_singlethreaded(test)]
    async fn udp_v6_socket_describe() {
        socket_describe(fposix_socket::Domain::Ipv6, fposix_socket::DatagramSocketProtocol::Udp)
            .await
    }

    async fn socket_get_info(
        domain: fposix_socket::Domain,
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let mut t = TestSetupBuilder::new().add_endpoint().add_empty_stack().build().await.unwrap();
        let test_stack = t.get(0);
        let socket_provider = test_stack.connect_socket_provider().unwrap();
        let response = socket_provider
            .datagram_socket(domain, proto)
            .await
            .unwrap()
            .expect("Socket call succeeds");
        let socket = match response {
            fposix_socket::ProviderDatagramSocketResponse::SynchronousDatagramSocket(sock) => sock,
            // TODO(https://fxrev.dev/99905): Implement Fast UDP sockets in Netstack3.
            fposix_socket::ProviderDatagramSocketResponse::DatagramSocket(sock) => {
                let _: fidl::endpoints::ClientEnd<fposix_socket::DatagramSocketMarker> = sock;
                panic!("expected SynchronousDatagramSocket, found DatagramSocket")
            }
        };
        let info = socket.into_proxy().unwrap().get_info().await.expect("get_info call succeeds");
        assert_eq!(info, Ok((domain, proto)));
    }

    #[fasync::run_singlethreaded(test)]
    async fn udp_v4_socket_get_info() {
        socket_get_info(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
            .await
    }

    #[fasync::run_singlethreaded(test)]
    async fn udp_v6_socket_get_info() {
        socket_get_info(fposix_socket::Domain::Ipv6, fposix_socket::DatagramSocketProtocol::Udp)
            .await
    }

    fn socket_clone(
        socket: &fposix_socket::SynchronousDatagramSocketProxy,
    ) -> Result<fposix_socket::SynchronousDatagramSocketProxy, Error> {
        let (client, server) =
            fidl::endpoints::create_proxy::<fposix_socket::SynchronousDatagramSocketMarker>()?;
        let server = ServerEnd::new(server.into_channel());
        let () = socket.clone2(server)?;
        Ok(client)
    }

    async fn clone<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol)
    where
        <A::AddrType as IpAddress>::Version: SocketCollectionIpExt<T>,
        T: Transport<Ipv4>,
        T: Transport<Ipv6>,
        T: Transport<<A::AddrType as IpAddress>::Version>,
        crate::bindings::BindingsNonSyncCtxImpl: AsRef<SocketCollectionPair<T>>,
    {
        let mut t = TestSetupBuilder::new()
            .add_endpoint()
            .add_endpoint()
            .add_stack(
                StackSetupBuilder::new()
                    .add_named_endpoint(test_ep_name(1), Some(A::config_addr_subnet())),
            )
            .add_stack(
                StackSetupBuilder::new()
                    .add_named_endpoint(test_ep_name(2), Some(A::config_addr_subnet_remote())),
            )
            .build()
            .await
            .unwrap();
        let (alice_socket, alice_events) = get_socket_and_event::<A>(t.get(0), proto).await;
        let alice_cloned = socket_clone(&alice_socket).expect("cannot clone socket");
        let fposix_socket::SynchronousDatagramSocketDescribeResponse { event: alice_event, .. } =
            alice_cloned.describe().await.expect("Describe call succeeds");
        let _: zx::EventPair = alice_event.expect("Describe call returns event");

        let () = alice_socket
            .bind(&A::create(A::LOCAL_ADDR, 200))
            .await
            .unwrap()
            .expect("failed to bind for alice");
        // We should be able to read that back from the cloned socket.
        assert_eq!(
            alice_cloned.get_sock_name().await.unwrap().expect("failed to getsockname for alice"),
            A::create(A::LOCAL_ADDR, 200)
        );

        let (bob_socket, bob_events) = get_socket_and_event::<A>(t.get(1), proto).await;
        let bob_cloned = socket_clone(&bob_socket).expect("failed to clone socket");
        let () = bob_cloned
            .bind(&A::create(A::REMOTE_ADDR, 200))
            .await
            .unwrap()
            .expect("failed to bind for bob");
        // We should be able to read that back from the original socket.
        assert_eq!(
            bob_socket.get_sock_name().await.unwrap().expect("failed to getsockname for bob"),
            A::create(A::REMOTE_ADDR, 200)
        );

        let body = "Hello".as_bytes();
        assert_eq!(
            alice_socket
                .send_msg(
                    Some(&A::create(A::REMOTE_ADDR, 200)),
                    &body,
                    &fposix_socket::DatagramSocketSendControlData::default(),
                    fposix_socket::SendMsgFlags::empty()
                )
                .await
                .unwrap()
                .expect("failed to send_msg"),
            body.len() as i64
        );

        assert_eq!(
            fasync::OnSignals::new(&bob_events, ZXSIO_SIGNAL_INCOMING).await,
            Ok(ZXSIO_SIGNAL_INCOMING | ZXSIO_SIGNAL_OUTGOING)
        );

        // Receive from the cloned socket.
        let (from, data, _, truncated) = bob_cloned
            .recv_msg(true, 2048, false, fposix_socket::RecvMsgFlags::empty())
            .await
            .unwrap()
            .expect("failed to recv_msg");
        assert_eq!(&data[..], body);
        assert_eq!(truncated, 0);
        assert_eq!(from.map(|a| *a), Some(A::create(A::LOCAL_ADDR, 200)));
        // The data have already been received on the cloned socket
        assert_eq!(
            bob_socket
                .recv_msg(false, 2048, false, fposix_socket::RecvMsgFlags::empty())
                .await
                .unwrap()
                .expect_err("Reading from bob should fail"),
            fposix::Errno::Eagain
        );

        // Close the socket should not invalidate the cloned socket.
        let () = bob_socket
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");

        assert_eq!(
            bob_cloned
                .send_msg(
                    Some(&A::create(A::LOCAL_ADDR, 200)),
                    &body,
                    &fposix_socket::DatagramSocketSendControlData::default(),
                    fposix_socket::SendMsgFlags::empty()
                )
                .await
                .unwrap()
                .expect("failed to send_msg"),
            body.len() as i64
        );

        let () = alice_cloned
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");
        assert_eq!(
            fasync::OnSignals::new(&alice_events, ZXSIO_SIGNAL_INCOMING).await,
            Ok(ZXSIO_SIGNAL_INCOMING | ZXSIO_SIGNAL_OUTGOING)
        );

        let (from, data, _, truncated) = alice_socket
            .recv_msg(true, 2048, false, fposix_socket::RecvMsgFlags::empty())
            .await
            .unwrap()
            .expect("failed to recv_msg");
        assert_eq!(&data[..], body);
        assert_eq!(truncated, 0);
        assert_eq!(from.map(|a| *a), Some(A::create(A::REMOTE_ADDR, 200)));

        // Make sure the sockets are still in the stack.
        for i in 0..2 {
            t.get(i).with_ctx(|ctx| {
                <A::AddrType as IpAddress>::Version::with_collection(
                    &ctx.non_sync_ctx,
                    |SocketCollection { received }| {
                        assert_matches!(received.iter().collect::<Vec<_>>()[..], [_]);
                    },
                )
            });
        }

        let () = alice_socket
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");
        let () = bob_cloned
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");

        // But the sockets should have gone here.
        for i in 0..2 {
            t.get(i).with_ctx(|ctx| {
                <A::AddrType as IpAddress>::Version::with_collection(
                    &ctx.non_sync_ctx,
                    |SocketCollection { received }| {
                        assert_matches!(received.iter().collect::<Vec<_>>()[..], []);
                    },
                )
            });
        }
    }

    declare_tests!(
        clone,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn close_twice<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol)
    where
        <A::AddrType as IpAddress>::Version: SocketCollectionIpExt<T>,
        T: Transport<Ipv4>,
        T: Transport<Ipv6>,
        T: Transport<<A::AddrType as IpAddress>::Version>,
        crate::bindings::BindingsNonSyncCtxImpl: AsRef<SocketCollectionPair<T>>,
    {
        // Make sure we cannot close twice from the same channel so that we
        // maintain the correct refcount.
        let mut t = TestSetupBuilder::new().add_endpoint().add_empty_stack().build().await.unwrap();
        let test_stack = t.get(0);
        let socket = get_socket::<A>(test_stack, proto).await;
        let cloned = socket_clone(&socket).unwrap();
        let () = socket
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");
        let _: fidl::Error = socket
            .close()
            .await
            .expect_err("should not be able to close the socket twice on the same channel");
        assert!(socket.into_channel().unwrap().is_closed());
        // Since we still hold the cloned socket, the binding_data shouldn't be
        // empty
        test_stack.with_ctx(|ctx| {
            <A::AddrType as IpAddress>::Version::with_collection(
                &ctx.non_sync_ctx,
                |SocketCollection { received }| {
                    assert_matches!(received.iter().collect::<Vec<_>>()[..], [_]);
                },
            )
        });
        let () = cloned
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");
        // Now it should become empty
        test_stack.with_ctx(|ctx| {
            <A::AddrType as IpAddress>::Version::with_collection(
                &ctx.non_sync_ctx,
                |SocketCollection { received }| {
                    assert_matches!(received.iter().collect::<Vec<_>>()[..], []);
                },
            )
        });
    }

    declare_tests!(close_twice);

    async fn implicit_close<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol)
    where
        <A::AddrType as IpAddress>::Version: SocketCollectionIpExt<T>,
        T: Transport<Ipv4>,
        T: Transport<Ipv6>,
        T: Transport<<A::AddrType as IpAddress>::Version>,
        crate::bindings::BindingsNonSyncCtxImpl: AsRef<SocketCollectionPair<T>>,
    {
        let mut t = TestSetupBuilder::new().add_endpoint().add_empty_stack().build().await.unwrap();
        let test_stack = t.get(0);
        let cloned = {
            let socket = get_socket::<A>(test_stack, proto).await;
            socket_clone(&socket).unwrap()
            // socket goes out of scope indicating an implicit close.
        };
        // Using an explicit close here.
        let () = cloned
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");
        // No socket should be there now.
        test_stack.with_ctx(|ctx| {
            <A::AddrType as IpAddress>::Version::with_collection(
                &ctx.non_sync_ctx,
                |SocketCollection { received }| {
                    assert_matches!(received.iter().collect::<Vec<_>>()[..], []);
                },
            )
        });
    }

    declare_tests!(implicit_close);

    async fn invalid_clone_args<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol)
    where
        <A::AddrType as IpAddress>::Version: SocketCollectionIpExt<T>,
        T: Transport<Ipv4>,
        T: Transport<Ipv6>,
        T: Transport<<A::AddrType as IpAddress>::Version>,
        crate::bindings::BindingsNonSyncCtxImpl: AsRef<SocketCollectionPair<T>>,
    {
        let mut t = TestSetupBuilder::new().add_endpoint().add_empty_stack().build().await.unwrap();
        let test_stack = t.get(0);
        let socket = get_socket::<A>(test_stack, proto).await;
        let () = socket
            .close()
            .await
            .expect("FIDL error")
            .map_err(zx::Status::from_raw)
            .expect("close failed");

        // make sure we don't leak anything.
        test_stack.with_ctx(|ctx| {
            <A::AddrType as IpAddress>::Version::with_collection(
                &ctx.non_sync_ctx,
                |SocketCollection { received }| {
                    assert_matches!(received.iter().collect::<Vec<_>>()[..], []);
                },
            )
        });
    }

    declare_tests!(invalid_clone_args);

    async fn shutdown<A: TestSockAddr, T>(proto: fposix_socket::DatagramSocketProtocol) {
        let mut t = TestSetupBuilder::new()
            .add_endpoint()
            .add_stack(
                StackSetupBuilder::new()
                    .add_named_endpoint(test_ep_name(1), Some(A::config_addr_subnet())),
            )
            .build()
            .await
            .unwrap();
        let (socket, events) = get_socket_and_event::<A>(t.get(0), proto).await;
        let local = A::create(A::LOCAL_ADDR, 200);
        let remote = A::create(A::REMOTE_ADDR, 300);
        assert_eq!(
            socket
                .shutdown(fposix_socket::ShutdownMode::WRITE)
                .await
                .unwrap()
                .expect_err("should not shutdown an unconnected socket"),
            fposix::Errno::Enotconn,
        );
        let () = socket.bind(&local).await.unwrap().expect("failed to bind");
        assert_eq!(
            socket
                .shutdown(fposix_socket::ShutdownMode::WRITE)
                .await
                .unwrap()
                .expect_err("should not shutdown an unconnected socket"),
            fposix::Errno::Enotconn,
        );
        let () = socket.connect(&remote).await.unwrap().expect("failed to connect");
        assert_eq!(
            socket
                .shutdown(fposix_socket::ShutdownMode::empty())
                .await
                .unwrap()
                .expect_err("invalid args"),
            fposix::Errno::Einval
        );

        // Cannot send
        let body = "Hello".as_bytes();
        let () = socket
            .shutdown(fposix_socket::ShutdownMode::WRITE)
            .await
            .unwrap()
            .expect("failed to shutdown");
        assert_eq!(
            socket
                .send_msg(
                    None,
                    &body,
                    &fposix_socket::DatagramSocketSendControlData::default(),
                    fposix_socket::SendMsgFlags::empty()
                )
                .await
                .unwrap()
                .expect_err("writing to an already-shutdown socket should fail"),
            fposix::Errno::Epipe,
        );
        let invalid_addr = A::create(A::REMOTE_ADDR, 0);
        assert_eq!(
            socket.send_msg(Some(&invalid_addr), &body, &fposix_socket::DatagramSocketSendControlData::default(), fposix_socket::SendMsgFlags::empty()).await.unwrap().expect_err(
                "writing to an invalid address (port 0) should fail with EINVAL instead of EPIPE"
            ),
            fposix::Errno::Einval,
        );

        let (e1, e2) = zx::EventPair::create();
        fasync::Task::spawn(async move {
            assert_eq!(
                fasync::OnSignals::new(&events, ZXSIO_SIGNAL_INCOMING).await,
                Ok(ZXSIO_SIGNAL_INCOMING | ZXSIO_SIGNAL_OUTGOING)
            );

            assert_eq!(e1.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_INCOMING), Ok(()));
        })
        .detach();

        let () = socket
            .shutdown(fposix_socket::ShutdownMode::READ)
            .await
            .unwrap()
            .expect("failed to shutdown");
        let (_, data, _, _) = socket
            .recv_msg(false, 2048, false, fposix_socket::RecvMsgFlags::empty())
            .await
            .unwrap()
            .expect("recvmsg should return empty data");
        assert!(data.is_empty());

        assert_eq!(
            fasync::OnSignals::new(&e2, ZXSIO_SIGNAL_INCOMING).await,
            Ok(ZXSIO_SIGNAL_INCOMING | zx::Signals::EVENTPAIR_PEER_CLOSED)
        );

        let () = socket
            .shutdown(fposix_socket::ShutdownMode::READ)
            .await
            .unwrap()
            .expect("failed to shutdown the socket twice");
        let () = socket
            .shutdown(fposix_socket::ShutdownMode::WRITE)
            .await
            .unwrap()
            .expect("failed to shutdown the socket twice");
        let () = socket
            .shutdown(fposix_socket::ShutdownMode::READ | fposix_socket::ShutdownMode::WRITE)
            .await
            .unwrap()
            .expect("failed to shutdown the socket twice");
    }

    declare_tests!(
        shutdown,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn set_receive_buffer_after_delivery<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) where
        <A::AddrType as IpAddress>::Version: SocketCollectionIpExt<Udp>,
    {
        let mut t =
            TestSetupBuilder::new().add_stack(StackSetupBuilder::new()).build().await.unwrap();

        let (socket, _events) = get_socket_and_event::<A>(t.get(0), proto).await;
        let addr =
            A::create(<<A::AddrType as IpAddress>::Version as Ip>::LOOPBACK_ADDRESS.get(), 200);
        socket.bind(&addr).await.unwrap().expect("bind should succeed");

        const SENT_PACKETS: u8 = 10;
        for i in 0..SENT_PACKETS {
            let buf = [i; MIN_OUTSTANDING_APPLICATION_MESSAGES_SIZE];
            let sent: usize = socket
                .send_msg(
                    Some(&addr),
                    &buf,
                    &fposix_socket::DatagramSocketSendControlData::default(),
                    fposix_socket::SendMsgFlags::empty(),
                )
                .await
                .unwrap()
                .expect("send_msg should succeed")
                .try_into()
                .unwrap();
            assert_eq!(sent, MIN_OUTSTANDING_APPLICATION_MESSAGES_SIZE);
        }

        // Wait for all packets to be delivered before changing the buffer size.
        let stack = t.get(0);
        let has_all_delivered = |messages: &MessageQueue<_>| {
            messages.available_messages().len() == usize::from(SENT_PACKETS)
        };
        loop {
            let all_delivered =
                stack.with_ctx(|Ctx { sync_ctx: _, non_sync_ctx }| {
                    <<A::AddrType as IpAddress>::Version as SocketCollectionIpExt<
                        Udp,
                    >>::with_collection(non_sync_ctx, |SocketCollection { received }| {
                        // Check the lone socket to see if the packets were
                        // received.
                        let messages = received.iter().next().unwrap();
                        has_all_delivered(&messages.lock())
                    })
                });
            if all_delivered {
                break;
            }
            // Give other futures on the same executor a chance to run. In a
            // single-threaded context, without the yield, this future would
            // always be able to re-lock the stack after unlocking, and so no
            // other future would make progress.
            futures_lite::future::yield_now().await;
        }

        // Use a buffer size of 0, which will be substituted with the minimum size.
        let () =
            socket.set_receive_buffer(0).await.unwrap().expect("set buffer size should succeed");

        let rx_count = futures::stream::unfold(socket, |socket| async {
            let result = socket
                .recv_msg(false, u32::MAX, false, fposix_socket::RecvMsgFlags::empty())
                .await
                .unwrap();
            match result {
                Ok((addr, data, control, size)) => {
                    let _: (
                        Option<Box<fnet::SocketAddress>>,
                        fposix_socket::DatagramSocketRecvControlData,
                        u32,
                    ) = (addr, control, size);
                    Some((data, socket))
                }
                Err(fposix::Errno::Eagain) => None,
                Err(e) => panic!("unexpected error: {:?}", e),
            }
        })
        .enumerate()
        .map(|(i, data)| {
            assert_eq!(
                &data,
                &[u8::try_from(i).unwrap(); MIN_OUTSTANDING_APPLICATION_MESSAGES_SIZE]
            )
        })
        .count()
        .await;
        assert_eq!(rx_count, usize::from(SENT_PACKETS));
    }

    declare_tests!(
        set_receive_buffer_after_delivery,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn send_recv_loopback_peek<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;
        let addr =
            A::create(<<A::AddrType as IpAddress>::Version as Ip>::LOOPBACK_ADDRESS.get(), 100);

        let () = proxy.bind(&addr).await.unwrap().expect("bind succeeds");
        let () = proxy.connect(&addr).await.unwrap().expect("connect succeeds");

        const DATA: &[u8] = &[1, 2, 3, 4, 5];
        assert_eq!(
            usize::try_from(
                proxy
                    .send_msg(
                        None,
                        DATA,
                        &fposix_socket::DatagramSocketSendControlData::default(),
                        fposix_socket::SendMsgFlags::empty()
                    )
                    .await
                    .unwrap()
                    .expect("send_msg should succeed"),
            )
            .unwrap(),
            DATA.len()
        );

        // First try receiving the message with PEEK set.
        let (_addr, data, _control, truncated) = loop {
            match proxy
                .recv_msg(false, u32::MAX, false, fposix_socket::RecvMsgFlags::PEEK)
                .await
                .unwrap()
            {
                Ok(peek) => break peek,
                Err(fposix::Errno::Eagain) => {
                    // The sent datagram hasn't been received yet, so check for
                    // it again in a moment.
                    continue;
                }
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        };
        assert_eq!(truncated, 0);
        assert_eq!(data.as_slice(), DATA);

        // Now that the message has for sure been received, it can be retrieved
        // without checking for Eagain.
        let (_addr, data, _control, truncated) = proxy
            .recv_msg(false, u32::MAX, false, fposix_socket::RecvMsgFlags::empty())
            .await
            .unwrap()
            .expect("recv should succeed");
        assert_eq!(truncated, 0);
        assert_eq!(data.as_slice(), DATA);
    }

    declare_tests!(
        send_recv_loopback_peek,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    // TODO(https://fxbug.dev/92678): add a syscall test to exercise this
    // behavior.
    async fn multicast_join_receive<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let (mut t, proxy, event) = prepare_test::<A>(proto).await;

        let mcast_addr = <<A::AddrType as IpAddress>::Version as Ip>::MULTICAST_SUBNET.network();
        let id = t.get(0).get_endpoint_id(1);

        match mcast_addr.into() {
            IpAddr::V4(mcast_addr) => {
                proxy.add_ip_membership(&fposix_socket::IpMulticastMembership {
                    mcast_addr: mcast_addr.into_fidl(),
                    iface: id.get(),
                    local_addr: fnet::Ipv4Address { addr: [0; 4] },
                })
            }
            IpAddr::V6(mcast_addr) => {
                proxy.add_ipv6_membership(&fposix_socket::Ipv6MulticastMembership {
                    mcast_addr: mcast_addr.into_fidl(),
                    iface: id.get(),
                })
            }
        }
        .await
        .unwrap()
        .expect("add membership should succeed");

        const PORT: u16 = 100;
        const DATA: &[u8] = &[1, 2, 3, 4, 5];

        let () = proxy
            .bind(&A::create(
                <<A::AddrType as IpAddress>::Version as Ip>::UNSPECIFIED_ADDRESS,
                PORT,
            ))
            .await
            .unwrap()
            .expect("bind succeeds");

        assert_eq!(
            usize::try_from(
                proxy
                    .send_msg(
                        Some(&A::create(mcast_addr, PORT)),
                        DATA,
                        &fposix_socket::DatagramSocketSendControlData::default(),
                        fposix_socket::SendMsgFlags::empty()
                    )
                    .await
                    .unwrap()
                    .expect("send_msg should succeed"),
            )
            .unwrap(),
            DATA.len()
        );

        let _signals = event
            .wait_handle(ZXSIO_SIGNAL_INCOMING, zx::Time::INFINITE)
            .expect("socket should receive");

        let (_addr, data, _control, truncated) = proxy
            .recv_msg(false, u32::MAX, false, fposix_socket::RecvMsgFlags::empty())
            .await
            .unwrap()
            .expect("recv should succeed");
        assert_eq!(truncated, 0);
        assert_eq!(data.as_slice(), DATA);
    }

    declare_tests!(
        multicast_join_receive,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn set_get_hop_limit_unicast<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;

        const HOP_LIMIT: u8 = 200;
        match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
            IpVersion::V4 => proxy.set_ip_multicast_ttl(&Some(HOP_LIMIT).into_fidl()),
            IpVersion::V6 => proxy.set_ipv6_multicast_hops(&Some(HOP_LIMIT).into_fidl()),
        }
        .await
        .unwrap()
        .expect("set hop limit should succeed");

        assert_eq!(
            match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
                IpVersion::V4 => proxy.get_ip_multicast_ttl(),
                IpVersion::V6 => proxy.get_ipv6_multicast_hops(),
            }
            .await
            .unwrap()
            .expect("get hop limit should succeed"),
            HOP_LIMIT
        )
    }

    declare_tests!(
        set_get_hop_limit_unicast,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    async fn set_get_hop_limit_multicast<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;

        const HOP_LIMIT: u8 = 200;
        match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
            IpVersion::V4 => proxy.set_ip_ttl(&Some(HOP_LIMIT).into_fidl()),
            IpVersion::V6 => proxy.set_ipv6_unicast_hops(&Some(HOP_LIMIT).into_fidl()),
        }
        .await
        .unwrap()
        .expect("set hop limit should succeed");

        assert_eq!(
            match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
                IpVersion::V4 => proxy.get_ip_ttl(),
                IpVersion::V6 => proxy.get_ipv6_unicast_hops(),
            }
            .await
            .unwrap()
            .expect("get hop limit should succeed"),
            HOP_LIMIT
        )
    }

    declare_tests!(
        set_get_hop_limit_multicast,
        icmp #[should_panic = "not yet implemented: https://fxbug.dev/47321: needs Core implementation"]
    );

    // TODO(https://fxbug.dev/21198): Change this when dual-stack socket support
    // is added since dual-stack sockets should allow setting options for both
    // IP versions.
    async fn set_hop_limit_wrong_type<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;

        const HOP_LIMIT: u8 = 200;
        assert_matches!(
            match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
                IpVersion::V4 => proxy.set_ipv6_multicast_hops(&Some(HOP_LIMIT).into_fidl()),
                IpVersion::V6 => proxy.set_ip_multicast_ttl(&Some(HOP_LIMIT).into_fidl()),
            }
            .await
            .unwrap(),
            Err(_)
        );

        assert_matches!(
            match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
                IpVersion::V4 => proxy.set_ipv6_unicast_hops(&Some(HOP_LIMIT).into_fidl()),
                IpVersion::V6 => proxy.set_ip_ttl(&Some(HOP_LIMIT).into_fidl()),
            }
            .await
            .unwrap(),
            Err(_)
        );
    }

    declare_tests!(set_hop_limit_wrong_type);

    // TODO(https://fxbug.dev/21198): Change this when dual-stack socket support
    // is added since dual-stack sockets should allow setting options for both
    // IP versions.
    async fn get_hop_limit_wrong_type<A: TestSockAddr, T>(
        proto: fposix_socket::DatagramSocketProtocol,
    ) {
        let (_t, proxy, _event) = prepare_test::<A>(proto).await;

        assert_matches!(
            match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
                IpVersion::V4 => proxy.get_ipv6_unicast_hops(),
                IpVersion::V6 => proxy.get_ip_ttl(),
            }
            .await
            .unwrap(),
            Err(_)
        );

        assert_matches!(
            match <<A::AddrType as IpAddress>::Version as Ip>::VERSION {
                IpVersion::V4 => proxy.get_ipv6_multicast_hops(),
                IpVersion::V6 => proxy.get_ip_multicast_ttl(),
            }
            .await
            .unwrap(),
            Err(_)
        );
    }

    declare_tests!(get_hop_limit_wrong_type);
}
