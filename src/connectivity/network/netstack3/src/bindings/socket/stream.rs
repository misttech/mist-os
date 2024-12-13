// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Stream sockets, primarily TCP sockets.

use std::fmt::Debug;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU8, NonZeroUsize, TryFromIntError};
use std::ops::ControlFlow;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use explicit::ResultExt as _;
use fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker as _, RequestStream as _};
use fidl::{AsHandleRef as _, HandleBased as _};
use futures::channel::{mpsc, oneshot};
use futures::FutureExt as _;
use log::{debug, error};
use net_types::ip::{GenericOverIp, Ip, IpAddress, IpVersion, Ipv4, Ipv6};
use net_types::{NonMappedAddr, SpecifiedAddr, ZonedAddr};
use netstack3_core::device::{DeviceId, WeakDeviceId};
use netstack3_core::socket::ShutdownType;
use netstack3_core::tcp::{
    self, AcceptError, BindError, BoundInfo, BufferSizes, ConnectError, ConnectionError,
    ConnectionInfo, IntoBuffers, ListenError, ListenerNotifier, NoConnection,
    OriginalDestinationError, SetReuseAddrError, SocketAddr, SocketInfo, SocketOptions,
    TcpBindingsTypes, UnboundInfo,
};
use netstack3_core::IpExt;
use once_cell::sync::Lazy;
use packet_formats::utils::NonZeroDuration;
use zx::{self as zx, Peered as _};
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_posix as fposix,
    fidl_fuchsia_posix_socket as fposix_socket,
};

use crate::bindings::socket::worker::{self, CloseResponder, SocketWorker};
use crate::bindings::socket::{
    IntoErrno, IpSockAddrExt, SockAddr, SocketWorkerProperties, ZXSIO_SIGNAL_CONNECTED,
    ZXSIO_SIGNAL_INCOMING,
};
use crate::bindings::util::{
    AllowBindingIdFromWeak, ConversionContext, IntoCore, IntoFidl, IntoFidlWithContext as _,
    ResultExt as _, TryIntoCoreWithContext, TryIntoFidlWithContext,
};
use crate::bindings::{BindingsCtx, Ctx};

mod buffer;
use buffer::{
    CoreReceiveBuffer, CoreSendBuffer, ReceiveBufferReader, SendBufferWriter, TaskStoppedError,
};

/// Maximum values allowed on linux: https://github.com/torvalds/linux/blob/0326074ff4652329f2a1a9c8685104576bd8d131/include/net/tcp.h#L159-L161
const MAX_TCP_KEEPIDLE_SECS: u64 = 32767;
const MAX_TCP_KEEPINTVL_SECS: u64 = 32767;
const MAX_TCP_KEEPCNT: u8 = 127;

type TcpSocketId<I> = tcp::TcpSocketId<I, WeakDeviceId<BindingsCtx>, BindingsCtx>;

#[derive(Debug)]
pub(crate) struct UnconnectedSocketData {
    zx_socket: Arc<zx::Socket>,
    rx_task_sender: mpsc::UnboundedSender<ReceiveBufferReader>,
    tx_task_sender: oneshot::Sender<SendBufferWriter>,
}

impl IntoBuffers<CoreReceiveBuffer, CoreSendBuffer> for UnconnectedSocketData {
    fn into_buffers(self, buffer_sizes: BufferSizes) -> (CoreReceiveBuffer, CoreSendBuffer) {
        let Self { zx_socket, rx_task_sender, tx_task_sender } = self;
        let BufferSizes { send, receive } = buffer_sizes;
        zx_socket
            .signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_CONNECTED)
            .expect("failed to signal connection established");

        // If the tasks are stopped and we can't create buffers, create zero
        // buffers as they'll report a 0 capacity back to TCP.
        //
        // We can't assert here since buffer creation on active opens might race
        // with socket closure which stops the tasks.
        let receive_buffer = CoreReceiveBuffer::new_ready(rx_task_sender, receive)
            .unwrap_or_else(|TaskStoppedError| CoreReceiveBuffer::Zero);
        let (send_buffer, send_writer) = CoreSendBuffer::new_ready(send);
        let send_buffer = match tx_task_sender.send(send_writer) {
            Ok(()) => send_buffer,
            Err(SendBufferWriter { .. }) => CoreSendBuffer::Zero,
        };
        (receive_buffer, send_buffer)
    }
}

/// The peer end of the zircon socket that will later be vended to application,
/// together with objects that are used to receive signals from application.
#[derive(Debug)]
pub(crate) struct PeerZirconSocketAndTaskData {
    peer: zx::Socket,
    spawn_data: TaskSpawnData,
}

#[derive(Debug)]
struct TaskSpawnData {
    rx_task_receiver: mpsc::UnboundedReceiver<ReceiveBufferReader>,
    tx_task_receiver: oneshot::Receiver<SendBufferWriter>,
    socket: Arc<zx::Socket>,
}

impl ListenerNotifier for UnconnectedSocketData {
    fn new_incoming_connections(&mut self, count: usize) {
        let Self { zx_socket, .. } = self;
        let (clear, set) = if count == 0 {
            (ZXSIO_SIGNAL_INCOMING, zx::Signals::NONE)
        } else {
            (zx::Signals::NONE, ZXSIO_SIGNAL_INCOMING)
        };

        zx_socket.signal_peer(clear, set).expect("failed to signal for available connections")
    }
}

impl TcpBindingsTypes for BindingsCtx {
    type ReceiveBuffer = CoreReceiveBuffer;
    type SendBuffer = CoreSendBuffer;
    type ReturnedBuffers = PeerZirconSocketAndTaskData;
    type ListenerNotifierOrProvidedBuffers = UnconnectedSocketData;

    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (Self::ReceiveBuffer, Self::SendBuffer, Self::ReturnedBuffers) {
        let (local, peer) = zx::Socket::create_stream();
        let socket = Arc::new(local);

        let (rx_task_sender, rx_task_receiver) = mpsc::unbounded();
        let (tx_task_sender, tx_task_receiver) = oneshot::channel();
        let (receive_buffer, send_buffer) = UnconnectedSocketData {
            zx_socket: Arc::clone(&socket),
            rx_task_sender,
            tx_task_sender,
        }
        .into_buffers(buffer_sizes);
        let returned_buffers = PeerZirconSocketAndTaskData {
            peer,
            spawn_data: TaskSpawnData { socket, tx_task_receiver, rx_task_receiver },
        };
        (receive_buffer, send_buffer, returned_buffers)
    }

    fn default_buffer_sizes() -> BufferSizes {
        static ZIRCON_SOCKET_BUFFER_SIZE: Lazy<usize> = Lazy::new(|| {
            let (local, _peer) = zx::Socket::create_stream();
            local.info().unwrap().tx_buf_max
        });
        BufferSizes { receive: *ZIRCON_SOCKET_BUFFER_SIZE, send: *ZIRCON_SOCKET_BUFFER_SIZE }
    }
}

struct BindingData<I: IpExt> {
    id: TcpSocketId<I>,
    peer: zx::Socket,
    task_data: Option<TaskSpawnData>,
    task_control: Option<TaskControl>,
}

#[derive(Debug)]
struct TaskControl {
    abort_handle: async_utils::event::Event,
    send_shutdown: Option<oneshot::Sender<oneshot::Sender<()>>>,
}

impl TaskControl {
    /// Shuts down the send task if it's still running, which flushes all the
    /// pending bytes from the zircon socket into the core send buffer.
    ///
    /// Only returns when all the pending bytes are available to core.
    ///
    /// This function is very permissive with errors since shutdown might be
    /// called multiple times and it could be racing with the send task, what
    /// matters is that _when the send task is running_ all the bytes are
    /// flushed properly and we properly synchronize on it.
    async fn shutdown_send(&mut self) {
        let Some(signal) = self.send_shutdown.take() else {
            // Shutdown already called, do nothing.
            return;
        };
        let (sender, receiver) = oneshot::channel();
        match signal.send(sender) {
            Ok(()) => {}
            Err(_sender) => {
                // Send task already dropped its shutdown listener so it must be
                // shutting down already.
                return;
            }
        }
        match receiver.await {
            Ok(()) => {}
            Err(oneshot::Canceled) => {
                // Race with send task finishing for other reasons, zircon
                // socket must've been flushed already or connection was dropped
                // from the peer side.
            }
        }
    }

    async fn shutdown_send_and_stop_tasks(mut self) {
        self.shutdown_send().await;
        let Self { abort_handle, send_shutdown } = self;
        // Must've been handled by shutdown_send.
        assert!(send_shutdown.is_none());
        // Assert that event has not been signaled yet.
        assert!(abort_handle.signal());
    }
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I> BindingData<I>
where
    I: IpExt,
{
    fn new(ctx: &mut Ctx, properties: SocketWorkerProperties) -> Self {
        let (local, peer) = zx::Socket::create_stream();
        let local = Arc::new(local);
        let SocketWorkerProperties {} = properties;

        let (rx_task_sender, rx_task_receiver) = mpsc::unbounded();
        let (tx_task_sender, tx_task_receiver) = oneshot::channel();

        let id = ctx.api().tcp::<I>().create(UnconnectedSocketData {
            zx_socket: Arc::clone(&local),
            tx_task_sender,
            rx_task_sender,
        });
        Self {
            id,
            peer,
            task_data: Some(TaskSpawnData { socket: local, tx_task_receiver, rx_task_receiver }),
            task_control: None,
        }
    }
}

impl CloseResponder for fposix_socket::StreamSocketCloseResponder {
    fn send(self, arg: Result<(), i32>) -> Result<(), fidl::Error> {
        fposix_socket::StreamSocketCloseResponder::send(self, arg)
    }
}

enum InitialSocketState {
    Unbound,
    Connected,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I: IpExt + IpSockAddrExt> worker::SocketWorkerHandler for BindingData<I> {
    type Request = fposix_socket::StreamSocketRequest;
    type RequestStream = fposix_socket::StreamSocketRequestStream;
    type CloseResponder = fposix_socket::StreamSocketCloseResponder;
    type SetupArgs = InitialSocketState;
    type Spawner = crate::bindings::util::TaskWaitGroupSpawner;

    fn setup(
        &mut self,
        ctx: &mut Ctx,
        args: InitialSocketState,
        spawners: &worker::TaskSpawnerCollection<crate::bindings::util::TaskWaitGroupSpawner>,
    ) {
        match args {
            InitialSocketState::Unbound => (),
            InitialSocketState::Connected => {
                let Self { id, peer: _, task_data, task_control } = self;
                let task_data =
                    task_data.take().expect("connected socket did not provide socket and watcher");
                let control =
                    spawn_tasks(ctx.clone(), id.clone(), task_data, &spawners.socket_scope);
                assert_matches::assert_matches!(task_control.replace(control), None);
            }
        }
    }

    async fn handle_request(
        &mut self,
        ctx: &mut Ctx,
        request: Self::Request,
        spawners: &worker::TaskSpawnerCollection<crate::bindings::util::TaskWaitGroupSpawner>,
    ) -> ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request, spawners).await
    }

    async fn close(self, ctx: &mut Ctx) {
        let Self { id, peer: _, task_data: _, task_control } = self;
        // We must shutdown the sender side before calling close so all the
        // pending bytes in the zircon socket are flushed and available to core
        // during the close procedure.
        if let Some(task_control) = task_control {
            task_control.shutdown_send_and_stop_tasks().await;
        }
        ctx.api().tcp().close(id);
    }
}

pub(super) fn spawn_worker(
    domain: fposix_socket::Domain,
    proto: fposix_socket::StreamSocketProtocol,
    ctx: crate::bindings::Ctx,
    request_stream: fposix_socket::StreamSocketRequestStream,
    spawner: &worker::ProviderScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
) {
    match (domain, proto) {
        (fposix_socket::Domain::Ipv4, fposix_socket::StreamSocketProtocol::Tcp) => {
            spawner.spawn(SocketWorker::serve_stream_with(
                ctx,
                BindingData::<Ipv4>::new,
                SocketWorkerProperties {},
                request_stream,
                InitialSocketState::Unbound,
                spawner.clone(),
            ))
        }
        (fposix_socket::Domain::Ipv6, fposix_socket::StreamSocketProtocol::Tcp) => {
            spawner.spawn(SocketWorker::serve_stream_with(
                ctx,
                BindingData::<Ipv6>::new,
                SocketWorkerProperties {},
                request_stream,
                InitialSocketState::Unbound,
                spawner.clone(),
            ))
        }
    }
}

impl IntoErrno for AcceptError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            AcceptError::WouldBlock => fposix::Errno::Eagain,
            AcceptError::NotSupported => fposix::Errno::Einval,
        }
    }
}

impl IntoErrno for ConnectError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            ConnectError::NoRoute => fposix::Errno::Enetunreach,
            ConnectError::NoPort | ConnectError::ConnectionExists => fposix::Errno::Eaddrnotavail,
            ConnectError::Zone(z) => z.into_errno(),
            ConnectError::Listener => fposix::Errno::Einval,
            ConnectError::Pending => fposix::Errno::Ealready,
            ConnectError::Completed => fposix::Errno::Eisconn,
            ConnectError::Aborted => fposix::Errno::Econnrefused,
        }
    }
}

impl IntoErrno for BindError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            Self::AlreadyBound => fposix::Errno::Einval,
            Self::LocalAddressError(err) => err.into_errno(),
        }
    }
}

impl IntoErrno for NoConnection {
    fn into_errno(self) -> fidl_fuchsia_posix::Errno {
        fposix::Errno::Enotconn
    }
}

impl IntoErrno for ListenError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            ListenError::ListenerExists => fposix::Errno::Eaddrinuse,
            ListenError::NotSupported => fposix::Errno::Einval,
        }
    }
}

impl IntoErrno for SetReuseAddrError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            SetReuseAddrError::AddrInUse => fposix::Errno::Eaddrinuse,
            SetReuseAddrError::NotSupported => fposix::Errno::Eopnotsupp,
        }
    }
}

// Mapping guided by: https://cs.opensource.google/gvisor/gvisor/+/master:test/packetimpact/tests/tcp_network_unreachable_test.go
impl IntoErrno for ConnectionError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            ConnectionError::ConnectionRefused => fposix::Errno::Econnrefused,
            ConnectionError::ConnectionReset => fposix::Errno::Econnreset,
            ConnectionError::NetworkUnreachable => fposix::Errno::Enetunreach,
            ConnectionError::HostUnreachable => fposix::Errno::Ehostunreach,
            ConnectionError::ProtocolUnreachable => fposix::Errno::Enoprotoopt,
            ConnectionError::PortUnreachable => fposix::Errno::Econnrefused,
            ConnectionError::DestinationHostDown => fposix::Errno::Ehostdown,
            ConnectionError::SourceRouteFailed => fposix::Errno::Eopnotsupp,
            ConnectionError::SourceHostIsolated => fposix::Errno::Enonet,
            ConnectionError::TimedOut => fposix::Errno::Etimedout,
        }
    }
}

impl IntoErrno for OriginalDestinationError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            Self::NotConnected
            | Self::NotFound
            | Self::UnspecifiedDestinationAddr
            | Self::UnspecifiedDestinationPort => fposix::Errno::Enoent,
        }
    }
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn spawn_tasks<I: IpExt>(
    ctx: crate::bindings::Ctx,
    id: TcpSocketId<I>,
    data: TaskSpawnData,
    spawner: &worker::SocketScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
) -> TaskControl {
    let TaskSpawnData { socket, rx_task_receiver, tx_task_receiver } = data;
    let event = async_utils::event::Event::new();

    let event_wait = event.wait();
    let (send_shutdown, send_shutdown_receiver) = oneshot::channel();
    let send_task = buffer::send_task(
        socket.clone(),
        buffer::SendTaskArgs { ctx: ctx.clone(), id: id.clone() },
        send_shutdown_receiver,
        tx_task_receiver,
    );
    spawner.spawn(async move {
        let send_task = pin!(send_task);
        futures::future::select(send_task, event_wait)
            .map(|_: futures::future::Either<((), _), ((), _)>| ())
            .await;
    });

    let event_wait = event.wait();
    let receive_task =
        buffer::receive_task(socket, buffer::ReceiveTaskArgs { ctx, id }, rx_task_receiver);
    spawner.spawn(async move {
        let receive_task = pin!(receive_task);
        futures::future::select(receive_task, event_wait)
            .map(|_: futures::future::Either<((), _), ((), _)>| ())
            .await;
    });
    TaskControl { abort_handle: event, send_shutdown: Some(send_shutdown) }
}

struct RequestHandler<'a, I: IpExt> {
    data: &'a mut BindingData<I>,
    ctx: &'a mut Ctx,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I: IpSockAddrExt + IpExt> RequestHandler<'_, I> {
    fn bind(self, addr: fnet::SocketAddress) -> Result<(), fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        let addr = I::SocketAddress::from_sock_addr(addr)?;
        let (addr, port) =
            addr.try_into_core_with_ctx(ctx.bindings_ctx()).map_err(IntoErrno::into_errno)?;
        ctx.api().tcp().bind(id, addr, NonZeroU16::new(port)).map_err(IntoErrno::into_errno)?;
        Ok(())
    }

    fn connect(
        self,
        addr: fnet::SocketAddress,
        spawner: &worker::SocketScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
    ) -> Result<(), fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control }, ctx } = self;

        let addr = I::SocketAddress::from_sock_addr(addr)?;
        let (ip, remote_port) =
            addr.try_into_core_with_ctx(ctx.bindings_ctx()).map_err(IntoErrno::into_errno)?;
        let port = NonZeroU16::new(remote_port).ok_or(fposix::Errno::Einval)?;
        ctx.api().tcp().connect(id, ip, port).map_err(IntoErrno::into_errno)?;
        if let Some(task_data) = self.data.task_data.take() {
            let control = spawn_tasks::<I>(ctx.clone(), id.clone(), task_data, spawner);
            assert_matches::assert_matches!(task_control.replace(control), None);
            Err(fposix::Errno::Einprogress)
        } else {
            Ok(())
        }
    }

    fn listen(self, backlog: i16) -> Result<(), fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        // The POSIX specification for `listen` [1] says
        //
        //   If listen() is called with a backlog argument value that is
        //   less than 0, the function behaves as if it had been called
        //   with a backlog argument value of 0.
        //
        //   A backlog argument of 0 may allow the socket to accept
        //   connections, in which case the length of the listen queue
        //   may be set to an implementation-defined minimum value.
        //
        // [1]: https://pubs.opengroup.org/onlinepubs/9699919799/functions/listen.html
        //
        // Always accept connections with a minimum backlog size of 1.
        // Use a maximum value of 4096 like Linux.
        const MINIMUM_BACKLOG_SIZE: NonZeroUsize = NonZeroUsize::new(1).unwrap();
        const MAXIMUM_BACKLOG_SIZE: NonZeroUsize = NonZeroUsize::new(4096).unwrap();

        let backlog = usize::try_from(backlog).unwrap_or(0);
        let backlog = NonZeroUsize::new(backlog).map_or(MINIMUM_BACKLOG_SIZE, |b| {
            NonZeroUsize::min(MAXIMUM_BACKLOG_SIZE, NonZeroUsize::max(b, MINIMUM_BACKLOG_SIZE))
        });

        ctx.api().tcp().listen(id, backlog).map_err(IntoErrno::into_errno)?;
        Ok(())
    }

    fn get_sock_name(self) -> Result<fnet::SocketAddress, fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        let fidl = match ctx.api().tcp().get_info(id) {
            SocketInfo::Unbound(UnboundInfo { device: _ }) => {
                Ok(<<I as IpSockAddrExt>::SocketAddress as SockAddr>::UNSPECIFIED)
            }
            SocketInfo::Bound(BoundInfo { addr, port, device: _ }) => {
                (addr, port).try_into_fidl_with_ctx(ctx.bindings_ctx())
            }
            SocketInfo::Connection(ConnectionInfo { local_addr, remote_addr: _, device: _ }) => {
                local_addr.try_into_fidl_with_ctx(ctx.bindings_ctx())
            }
        }
        .map_err(IntoErrno::into_errno)?;
        Ok(fidl.into_sock_addr())
    }

    fn get_peer_name(self) -> Result<fnet::SocketAddress, fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        match ctx.api().tcp().get_info(id) {
            SocketInfo::Unbound(_) | SocketInfo::Bound(_) => Err(fposix::Errno::Enotconn),
            SocketInfo::Connection(info) => Ok({
                info.remote_addr
                    .try_into_fidl_with_ctx(ctx.bindings_ctx())
                    .map_err(IntoErrno::into_errno)?
                    .into_sock_addr()
            }),
        }
    }

    fn accept(
        self,
        want_addr: bool,
        spawner: &worker::ProviderScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
    ) -> Result<
        (Option<fnet::SocketAddress>, ClientEnd<fposix_socket::StreamSocketMarker>),
        fposix::Errno,
    > {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;

        let (accepted, addr, peer) = ctx.api().tcp().accept(id).map_err(IntoErrno::into_errno)?;
        let addr = addr
            .map_zone(AllowBindingIdFromWeak)
            .into_fidl_with_ctx(ctx.bindings_ctx())
            .into_sock_addr();
        let PeerZirconSocketAndTaskData { peer, spawn_data } = peer;
        let (client, request_stream) = crate::bindings::socket::create_request_stream();
        peer.signal_handle(zx::Signals::NONE, ZXSIO_SIGNAL_CONNECTED)
            .expect("failed to signal connection established");
        spawn_connected_socket_task(
            ctx.clone(),
            accepted,
            peer,
            request_stream,
            spawn_data,
            spawner,
        );
        Ok((want_addr.then_some(addr), client))
    }

    fn get_error(self) -> Result<(), fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        match ctx.api().tcp().get_socket_error(id) {
            Some(err) => Err(err.into_errno()),
            None => Ok(()),
        }
    }

    async fn shutdown(self, mode: fposix_socket::ShutdownMode) -> Result<(), fposix::Errno> {
        let Self { data: BindingData { id, peer, task_data: _, task_control }, ctx } = self;
        let shutdown_recv = mode.contains(fposix_socket::ShutdownMode::READ);
        let shutdown_send = mode.contains(fposix_socket::ShutdownMode::WRITE);
        let shutdown_type = ShutdownType::from_send_receive(shutdown_send, shutdown_recv)
            .ok_or(fposix::Errno::Einval)?;

        // If shutdown send is requested and we have spawned tasks, then we must
        // call shutdown send. This is valid because the only error possible
        // here is NoConnection as shown by the match below, in which case the
        // send task would either be done already or never spawned which means
        // we can't get here.
        if let (true, Some(task_control)) = (shutdown_send, task_control.as_mut()) {
            task_control.shutdown_send().await;
        }

        let is_conn = ctx
            .api()
            .tcp()
            .shutdown(id, shutdown_type)
            .map_err(|e @ NoConnection| e.into_errno())?;
        if is_conn {
            let peer_disposition = shutdown_send.then_some(zx::SocketWriteDisposition::Disabled);
            let my_disposition = shutdown_recv.then_some(zx::SocketWriteDisposition::Disabled);
            peer.set_disposition(peer_disposition, my_disposition)
                .expect("failed to set socket disposition");
        }
        Ok(())
    }

    fn set_bind_to_device(self, device: Option<&str>) -> Result<(), fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        let device = device
            .map(|name| {
                ctx.bindings_ctx().devices.get_device_by_name(name).ok_or(fposix::Errno::Enodev)
            })
            .transpose()?;

        ctx.api().tcp().set_device(id, device).map_err(IntoErrno::into_errno)
    }

    fn bind_to_device_index(self, device: u64) -> Result<(), fposix::Errno> {
        let Self { ctx, data: BindingData { id, peer: _, task_data: _, task_control: _ } } = self;

        // If `device` is 0, then this will clear the bound device.
        let device: Option<DeviceId<_>> = NonZeroU64::new(device)
            .map(|index| ctx.bindings_ctx().devices.get_core_id(index).ok_or(fposix::Errno::Enodev))
            .transpose()?;

        ctx.api().tcp().set_device(id, device).map_err(IntoErrno::into_errno)
    }

    fn set_send_buffer_size(self, new_size: u64) {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        let new_size =
            usize::try_from(new_size).ok_checked::<TryFromIntError>().unwrap_or(usize::MAX);
        ctx.api().tcp().set_send_buffer_size(id, new_size);
    }

    fn send_buffer_size(self) -> u64 {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        ctx.api()
            .tcp()
            .send_buffer_size(id)
            // If the socket doesn't have a send buffer (e.g. because it was shut
            // down for writing and all the data was sent to the peer), return 0.
            .unwrap_or(0)
            .try_into()
            .ok_checked::<TryFromIntError>()
            .unwrap_or(u64::MAX)
    }

    fn set_receive_buffer_size(self, new_size: u64) {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        let new_size =
            usize::try_from(new_size).ok_checked::<TryFromIntError>().unwrap_or(usize::MAX);
        ctx.api().tcp().set_receive_buffer_size(id, new_size);
    }

    fn receive_buffer_size(self) -> u64 {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        ctx.api()
            .tcp()
            .receive_buffer_size(id)
            // If the socket doesn't have a receive buffer (e.g. because the remote
            // end signalled FIN and all data was sent to the client), return 0.
            .unwrap_or(0)
            .try_into()
            .ok_checked::<TryFromIntError>()
            .unwrap_or(u64::MAX)
    }

    fn set_reuse_address(self, value: bool) -> Result<(), fposix::Errno> {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        ctx.api().tcp().set_reuseaddr(id, value).map_err(IntoErrno::into_errno)
    }

    fn reuse_address(self) -> bool {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        ctx.api().tcp().reuseaddr(id)
    }

    fn get_original_destination(
        self,
        ip_version: IpVersion,
    ) -> Result<fnet::SocketAddress, fposix::Errno> {
        let result = self
            .ctx
            .api()
            .tcp()
            .get_original_destination(&self.data.id)
            .map_err(IntoErrno::into_errno);

        fn sockaddr<I: IpSockAddrExt>(
            addr: SpecifiedAddr<I::Addr>,
            port: NonZeroU16,
        ) -> fnet::SocketAddress {
            I::SocketAddress::new(Some(ZonedAddr::Unzoned(addr)), port.get()).into_sock_addr()
        }

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct In<I: Ip>(Result<(SpecifiedAddr<I::Addr>, NonZeroU16), fposix::Errno>);

        I::map_ip_in(
            In(result),
            |In(result)| match ip_version {
                IpVersion::V4 => {
                    let (addr, port) = result?;
                    Ok(sockaddr::<Ipv4>(addr, port))
                }
                IpVersion::V6 => Err(fposix::Errno::Eopnotsupp),
            },
            |In(result)| {
                let (addr, port) = result?;
                match ip_version {
                    IpVersion::V4 => {
                        let addr = addr.to_ipv4_mapped().ok_or(fposix::Errno::Enoent)?;
                        // TCP connections always have a specified destination address, but this
                        // invariant is not upheld in the type system here because we are retrieving
                        // the destination from the connection tracking table.
                        let addr = SpecifiedAddr::new(addr).ok_or_else(|| {
                            error!(
                                "original destination for socket {:?} had unspecified addr \
                                (port {port})",
                                self.data.id
                            );
                            fposix::Errno::Enoent
                        })?;

                        Ok(sockaddr::<Ipv4>(addr, port))
                    }
                    IpVersion::V6 => {
                        let addr = NonMappedAddr::new(addr).ok_or(fposix::Errno::Enoent)?;
                        Ok(sockaddr::<Ipv6>(*addr, port))
                    }
                }
            },
        )
    }

    /// Returns a [`ControlFlow`] to indicate whether the parent stream should
    /// continue being polled or dropped.
    ///
    /// If `Some(stream)` is returned in the `Continue` case, `stream` is a new
    /// stream of events that should be polled concurrently with the parent
    /// stream.
    async fn handle_request(
        self,
        request: fposix_socket::StreamSocketRequest,
        spawners: &worker::TaskSpawnerCollection<crate::bindings::util::TaskWaitGroupSpawner>,
    ) -> ControlFlow<
        fposix_socket::StreamSocketCloseResponder,
        Option<fposix_socket::StreamSocketRequestStream>,
    > {
        let Self { data: BindingData { id: _, peer, task_data: _, task_control: _ }, ctx: _ } =
            self;
        match request {
            fposix_socket::StreamSocketRequest::Bind { addr, responder } => {
                responder.send(self.bind(addr)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Connect { addr, responder } => {
                // Connect always spawns on the socket scope.
                let response = self.connect(addr, &spawners.socket_scope);
                responder.send(response).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Describe { responder } => {
                let socket = peer
                    .duplicate_handle(
                        (zx::Rights::BASIC | zx::Rights::IO)
                        // Don't allow the peer to duplicate the stream.
                        & !zx::Rights::DUPLICATE,
                    )
                    .expect("failed to duplicate the socket handle");
                responder
                    .send(fposix_socket::StreamSocketDescribeResponse {
                        socket: Some(socket),
                        ..Default::default()
                    })
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Listen { backlog, responder } => {
                responder.send(self.listen(backlog)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Accept { want_addr, responder } => {
                // Accept receives the provider scope because it creates a new
                // socket worker for the newly created socket.
                let response = self.accept(want_addr, &spawners.provider_scope);
                responder
                    .send(match response {
                        Ok((ref addr, client)) => Ok((addr.as_ref(), client)),
                        Err(e) => Err(e),
                    })
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Close { responder } => {
                // We don't just close the socket because this socket worker is
                // potentially shared by a bunch of sockets because the client
                // can call `dup` on this socket. We will do the cleanup at the
                // end of this task.
                return ControlFlow::Break(responder);
            }
            fposix_socket::StreamSocketRequest::Clone2 { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel());
                let rs = fposix_socket::StreamSocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(rs));
            }
            fposix_socket::StreamSocketRequest::SetBindToDevice { value, responder } => {
                let identifier = (!value.is_empty()).then_some(value.as_str());
                responder
                    .send(self.set_bind_to_device(identifier))
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetBindToInterfaceIndex { value, responder } => {
                let result =
                    self.bind_to_device_index(value).log_error("tcp::SetBindToInterfaceIndex");
                responder.send(result).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Query { responder } => {
                responder
                    .send(fposix_socket::StreamSocketMarker::PROTOCOL_NAME.as_bytes())
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetReuseAddress { value, responder } => {
                responder.send(self.set_reuse_address(value)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetReuseAddress { responder } => {
                responder.send(Ok(self.reuse_address())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetError { responder } => {
                responder.send(self.get_error()).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetBroadcast { value: _, responder } => {
                respond_not_supported!("stream::SetBroadcast", responder);
            }
            fposix_socket::StreamSocketRequest::GetBroadcast { responder } => {
                respond_not_supported!("stream::GetBroadcast", responder);
            }
            fposix_socket::StreamSocketRequest::SetSendBuffer { value_bytes, responder } => {
                self.set_send_buffer_size(value_bytes);
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetSendBuffer { responder } => {
                responder.send(Ok(self.send_buffer_size())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetReceiveBuffer { value_bytes, responder } => {
                responder
                    .send(Ok(self.set_receive_buffer_size(value_bytes)))
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetReceiveBuffer { responder } => {
                responder.send(Ok(self.receive_buffer_size())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetKeepAlive { value: enabled, responder } => {
                self.with_socket_options_mut(|so| so.keep_alive.enabled = enabled);
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetKeepAlive { responder } => {
                let enabled = self.with_socket_options(|so| so.keep_alive.enabled);
                responder.send(Ok(enabled)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetOutOfBandInline { value: _, responder } => {
                respond_not_supported!("stream::SetOutOfBandInline", responder);
            }
            fposix_socket::StreamSocketRequest::GetOutOfBandInline { responder } => {
                respond_not_supported!("stream::GetOutOfBandInline", responder);
            }
            fposix_socket::StreamSocketRequest::SetNoCheck { value: _, responder } => {
                respond_not_supported!("stream::SetNoCheck", responder);
            }
            fposix_socket::StreamSocketRequest::GetNoCheck { responder } => {
                respond_not_supported!("stream::GetNoCheck", responder);
            }
            fposix_socket::StreamSocketRequest::SetLinger {
                linger: _,
                length_secs: _,
                responder,
            } => {
                respond_not_supported!("stream::SetLinger", responder);
            }
            fposix_socket::StreamSocketRequest::GetLinger { responder } => {
                debug!("stream::GetLinger is not supported, returning Ok((false, 0))");
                responder.send(Ok((false, 0))).unwrap_or_log("failed to respond")
            }
            fposix_socket::StreamSocketRequest::SetReusePort { value: _, responder } => {
                respond_not_supported!("stream::SetReusePort", responder);
            }
            fposix_socket::StreamSocketRequest::GetReusePort { responder } => {
                respond_not_supported!("stream::GetReusePort", responder);
            }
            fposix_socket::StreamSocketRequest::GetAcceptConn { responder } => {
                respond_not_supported!("stream::GetAcceptConn", responder);
            }
            fposix_socket::StreamSocketRequest::GetBindToDevice { responder } => {
                respond_not_supported!("stream::GetBindToDevice", responder);
            }
            fposix_socket::StreamSocketRequest::GetBindToInterfaceIndex { responder } => {
                respond_not_supported!("stream::GetBindToInterfaceIndex", responder);
            }
            fposix_socket::StreamSocketRequest::SetTimestamp { value: _, responder } => {
                respond_not_supported!("stream::SetTimestamp", responder);
            }
            fposix_socket::StreamSocketRequest::GetTimestamp { responder } => {
                respond_not_supported!("stream::GetTimestamp", responder);
            }
            fposix_socket::StreamSocketRequest::GetOriginalDestination { responder } => {
                responder
                    .send(self.get_original_destination(IpVersion::V4).as_ref().map_err(|e| *e))
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Disconnect { responder } => {
                respond_not_supported!("stream::Disconnect", responder);
            }
            fposix_socket::StreamSocketRequest::GetSockName { responder } => {
                responder
                    .send(self.get_sock_name().as_ref().map_err(|e| *e))
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetPeerName { responder } => {
                responder
                    .send(self.get_peer_name().as_ref().map_err(|e| *e))
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::Shutdown { mode, responder } => {
                responder.send(self.shutdown(mode).await).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetIpTypeOfService { value: _, responder } => {
                debug!("stream::SetIpTypeOfService is not supported, returning Ok(())");
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetIpTypeOfService { responder } => {
                respond_not_supported!("stream::GetIpTypeOfService", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpTtl { value: _, responder } => {
                respond_not_supported!("stream::SetIpTtl", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpTtl { responder } => {
                respond_not_supported!("stream::GetIpTtl", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpPacketInfo { value: _, responder } => {
                respond_not_supported!("stream::SetIpPacketInfo", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpPacketInfo { responder } => {
                respond_not_supported!("stream::GetIpPacketInfo", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpReceiveTypeOfService {
                value: _,
                responder,
            } => {
                respond_not_supported!("stream::SetIpReceiveTypeOfService", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpReceiveTypeOfService { responder } => {
                respond_not_supported!("stream::GetIpReceiveTypeOfService", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpReceiveTtl { value: _, responder } => {
                respond_not_supported!("stream::SetIpReceiveTtl", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpReceiveTtl { responder } => {
                respond_not_supported!("stream::GetIpReceiveTtl", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpMulticastInterface {
                iface: _,
                address: _,
                responder,
            } => {
                respond_not_supported!("stream::SetIpMulticastInterface", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpMulticastInterface { responder } => {
                respond_not_supported!("stream::GetIpMulticastInterface", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpMulticastTtl { value: _, responder } => {
                respond_not_supported!("stream::SetIpMulticastTtl", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpMulticastTtl { responder } => {
                respond_not_supported!("stream::GetIpMulticastTtl", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpMulticastLoopback { value: _, responder } => {
                respond_not_supported!("stream::SetIpMulticastLoopback", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpMulticastLoopback { responder } => {
                respond_not_supported!("stream::GetIpMulticastLoopback", responder);
            }
            fposix_socket::StreamSocketRequest::AddIpMembership { membership: _, responder } => {
                respond_not_supported!("stream::AddIpMembership", responder);
            }
            fposix_socket::StreamSocketRequest::DropIpMembership { membership: _, responder } => {
                respond_not_supported!("stream::DropIpMembership", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpTransparent { value: _, responder } => {
                // In theory this can be used on stream sockets, but we don't need it right now.
                respond_not_supported!("stream::SetIpTransparent", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpTransparent { responder } => {
                respond_not_supported!("stream::GetIpTransparent", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpReceiveOriginalDestinationAddress {
                value: _,
                responder,
            } => {
                respond_not_supported!("stream::SetIpReceiveOriginalDestinationAddress", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpReceiveOriginalDestinationAddress {
                responder,
            } => {
                respond_not_supported!("stream::GetIpReceiveOriginalDestinationAddress", responder);
            }
            fposix_socket::StreamSocketRequest::AddIpv6Membership { membership: _, responder } => {
                respond_not_supported!("stream::AddIpv6Membership", responder);
            }
            fposix_socket::StreamSocketRequest::DropIpv6Membership { membership: _, responder } => {
                respond_not_supported!("stream::DropIpv6Membership", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6MulticastInterface {
                value: _,
                responder,
            } => {
                respond_not_supported!("stream::SetIpv6MulticastInterface", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6MulticastInterface { responder } => {
                respond_not_supported!("stream::GetIpv6MulticastInterface", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6UnicastHops { value: _, responder } => {
                respond_not_supported!("stream::SetIpv6UnicastHops", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6UnicastHops { responder } => {
                respond_not_supported!("stream::GetIpv6UnicastHops", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6ReceiveHopLimit { value: _, responder } => {
                respond_not_supported!("stream::SetIpv6ReceiveHopLimit", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6ReceiveHopLimit { responder } => {
                respond_not_supported!("stream::GetIpv6ReceiveHopLimit", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6MulticastHops { value: _, responder } => {
                respond_not_supported!("stream::SetIpv6MulticastHops", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6MulticastHops { responder } => {
                respond_not_supported!("stream::GetIpv6MulticastHops", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6MulticastLoopback {
                value: _,
                responder,
            } => {
                respond_not_supported!("stream::SetIpv6MulticastLoopback", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6MulticastLoopback { responder } => {
                respond_not_supported!("stream::GetIpv6MulticastLoopback", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6Only { value, responder } => {
                let Self { data: BindingData { id, .. }, ctx } = self;
                responder
                    .send(
                        ctx.api()
                            .tcp()
                            .set_dual_stack_enabled(id, !value)
                            .map_err(IntoErrno::into_errno),
                    )
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetIpv6Only { responder } => {
                let Self { data: BindingData { id, .. }, ctx } = self;
                responder
                    .send(
                        ctx.api()
                            .tcp()
                            .dual_stack_enabled(id)
                            .map(|enabled| !enabled)
                            .map_err(IntoErrno::into_errno),
                    )
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetIpv6ReceiveTrafficClass {
                value: _,
                responder,
            } => {
                respond_not_supported!("stream::SetIpv6ReceiveTrafficClass", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6ReceiveTrafficClass { responder } => {
                respond_not_supported!("stream::GetIpv6ReceiveTrafficClass", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6TrafficClass { value: _, responder } => {
                respond_not_supported!("stream::SetIpv6TrafficClass", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6TrafficClass { responder } => {
                respond_not_supported!("stream::GetIpv6TrafficClass", responder);
            }
            fposix_socket::StreamSocketRequest::SetIpv6ReceivePacketInfo {
                value: _,
                responder,
            } => {
                respond_not_supported!("stream::SetIpv6ReceivePacketInfo", responder);
            }
            fposix_socket::StreamSocketRequest::GetIpv6ReceivePacketInfo { responder } => {
                respond_not_supported!("stream::GetIpv6ReceivePacketInfo", responder);
            }
            fposix_socket::StreamSocketRequest::GetInfo { responder } => {
                let domain = match I::VERSION {
                    IpVersion::V4 => fposix_socket::Domain::Ipv4,
                    IpVersion::V6 => fposix_socket::Domain::Ipv6,
                };

                responder
                    .send(Ok((domain, fposix_socket::StreamSocketProtocol::Tcp)))
                    .unwrap_or_log("failed to respond");
            }
            // Note for the following two options:
            // Nagle enabled means TCP delays sending segment, thus meaning
            // TCP_NODELAY is turned off. They have opposite meanings.
            fposix_socket::StreamSocketRequest::SetTcpNoDelay { value, responder } => {
                self.with_socket_options_mut(|so| {
                    so.nagle_enabled = !value;
                });
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetTcpNoDelay { responder } => {
                let nagle_enabled = self.with_socket_options(|so| so.nagle_enabled);
                responder.send(Ok(!nagle_enabled)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpMaxSegment { value_bytes: _, responder } => {
                debug!("stream::SetTcpMaxSegment is not supported, returning Ok(())");
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetTcpMaxSegment { responder } => {
                respond_not_supported!("stream::GetTcpMaxSegment", responder);
            }
            fposix_socket::StreamSocketRequest::SetTcpCork { value: _, responder } => {
                respond_not_supported!("stream::SetTcpCork", responder);
            }
            fposix_socket::StreamSocketRequest::GetTcpCork { responder } => {
                respond_not_supported!("stream::GetTcpCork", responder);
            }
            fposix_socket::StreamSocketRequest::SetTcpKeepAliveIdle { value_secs, responder } => {
                match NonZeroU64::new(value_secs.into())
                    .filter(|value_secs| value_secs.get() <= MAX_TCP_KEEPIDLE_SECS)
                {
                    Some(secs) => {
                        self.with_socket_options_mut(|so| {
                            so.keep_alive.idle = NonZeroDuration::from_nonzero_secs(secs)
                        });
                        responder.send(Ok(())).unwrap_or_log("failed to respond");
                    }
                    None => {
                        responder
                            .send(Err(fposix::Errno::Einval))
                            .unwrap_or_log("failed to respond");
                    }
                }
            }
            fposix_socket::StreamSocketRequest::GetTcpKeepAliveIdle { responder } => {
                let secs =
                    self.with_socket_options(|so| Duration::from(so.keep_alive.idle).as_secs());
                responder.send(Ok(u32::try_from(secs).unwrap())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpKeepAliveInterval {
                value_secs,
                responder,
            } => {
                match NonZeroDuration::from_secs(value_secs.into())
                    .filter(|value_dur| value_dur.get().as_secs() <= MAX_TCP_KEEPINTVL_SECS)
                {
                    Some(dur) => {
                        self.with_socket_options_mut(|so| so.keep_alive.interval = dur);
                        responder.send(Ok(())).unwrap_or_log("failed to respond");
                    }
                    None => {
                        responder
                            .send(Err(fposix::Errno::Einval))
                            .unwrap_or_log("failed to respond");
                    }
                }
            }
            fposix_socket::StreamSocketRequest::GetTcpKeepAliveInterval { responder } => {
                let secs =
                    self.with_socket_options(|so| Duration::from(so.keep_alive.interval).as_secs());
                responder.send(Ok(u32::try_from(secs).unwrap())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpKeepAliveCount { value, responder } => {
                match u8::try_from(value)
                    .ok_checked::<TryFromIntError>()
                    .and_then(NonZeroU8::new)
                    .filter(|count| count.get() <= MAX_TCP_KEEPCNT)
                {
                    Some(count) => {
                        self.with_socket_options_mut(|so| {
                            so.keep_alive.count = count;
                        });
                        responder.send(Ok(())).unwrap_or_log("failed to respond");
                    }
                    None => {
                        responder
                            .send(Err(fposix::Errno::Einval))
                            .unwrap_or_log("failed to respond");
                    }
                };
            }
            fposix_socket::StreamSocketRequest::GetTcpKeepAliveCount { responder } => {
                let count = self.with_socket_options(|so| so.keep_alive.count);
                responder.send(Ok(u32::from(u8::from(count)))).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpSynCount { value, responder } => {
                responder
                    .send(self.with_socket_options_mut(|so| {
                        so.max_syn_retries = u8::try_from(value)
                            .ok_checked::<TryFromIntError>()
                            .and_then(NonZeroU8::new)
                            .ok_or(fposix::Errno::Einval)?;
                        Ok(())
                    }))
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetTcpSynCount { responder } => {
                let syn_cnt = self.with_socket_options(|so| u32::from(so.max_syn_retries.get()));
                responder.send(Ok(syn_cnt)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpLinger { value_secs, responder } => {
                const MAX_FIN_WAIT2_TIMEOUT_SECS: u32 = 120;
                let fin_wait2_timeout =
                    IntoCore::<Option<u32>>::into_core(value_secs).map(|value_secs| {
                        NonZeroU32::new(value_secs.min(MAX_FIN_WAIT2_TIMEOUT_SECS))
                            .map_or(tcp::DEFAULT_FIN_WAIT2_TIMEOUT, |secs| {
                                Duration::from_secs(u64::from(secs.get()))
                            })
                    });
                self.with_socket_options_mut(|so| {
                    so.fin_wait2_timeout = fin_wait2_timeout;
                });
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetTcpLinger { responder } => {
                let linger_secs =
                    self.with_socket_options(|so| so.fin_wait2_timeout.map(|d| d.as_secs()));
                let respond_value = linger_secs.map(|x| u32::try_from(x).unwrap()).into_fidl();
                responder.send(Ok(&respond_value)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpDeferAccept { value_secs: _, responder } => {
                respond_not_supported!("stream::SetTcpDeferAccept", responder);
            }
            fposix_socket::StreamSocketRequest::GetTcpDeferAccept { responder } => {
                respond_not_supported!("stream::GetTcpDeferAccept", responder);
            }
            fposix_socket::StreamSocketRequest::SetTcpWindowClamp { value: _, responder } => {
                respond_not_supported!("stream::SetTcpWindowClamp", responder);
            }
            fposix_socket::StreamSocketRequest::GetTcpWindowClamp { responder } => {
                respond_not_supported!("stream::GetTcpWindowClamp", responder);
            }
            fposix_socket::StreamSocketRequest::GetTcpInfo { responder } => {
                debug!(
                    "stream::GetTcpInfo is not supported, \
                     returning fposix_socket::TcpInfo::default()"
                );
                responder
                    .send(Ok(&fposix_socket::TcpInfo::default()))
                    .unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpQuickAck { value, responder } => {
                self.with_socket_options_mut(|so| so.delayed_ack = !value);
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetTcpQuickAck { responder } => {
                let quick_ack = self.with_socket_options(|so| !so.delayed_ack);
                responder.send(Ok(quick_ack)).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetTcpCongestion { value: _, responder } => {
                respond_not_supported!("stream::SetTcpCongestion", responder);
            }
            fposix_socket::StreamSocketRequest::GetTcpCongestion { responder } => {
                respond_not_supported!("stream::GetTcpCongestion", responder);
            }
            fposix_socket::StreamSocketRequest::SetTcpUserTimeout { value_millis, responder } => {
                let user_timeout =
                    NonZeroU64::new(value_millis.into()).map(NonZeroDuration::from_nonzero_millis);
                self.with_socket_options_mut(|so| {
                    so.user_timeout = user_timeout;
                });
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::GetTcpUserTimeout { responder } => {
                let millis = self.with_socket_options(|so| {
                    so.user_timeout.map(|d| d.get().as_millis()).unwrap_or(0)
                });
                let result =
                    u32::try_from(millis).map_err(|_: TryFromIntError| fposix::Errno::Einval);
                responder.send(result).unwrap_or_log("failed to respond");
            }
            fposix_socket::StreamSocketRequest::SetMark { domain, mark, responder } => {
                self.ctx.api().tcp().set_mark(&self.data.id, domain.into_core(), mark.into_core());
                responder.send(Ok(())).unwrap_or_log("failed to respond")
            }
            fposix_socket::StreamSocketRequest::GetMark { domain, responder } => {
                let mark = self.ctx.api().tcp().get_mark(&self.data.id, domain.into_core());
                responder.send(Ok(&mark.into_fidl())).unwrap_or_log("failed to respond")
            }
        }
        ControlFlow::Continue(None)
    }

    fn with_socket_options_mut<R, F: FnOnce(&mut SocketOptions) -> R>(self, f: F) -> R {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        ctx.api().tcp().with_socket_options_mut(id, f)
    }

    fn with_socket_options<R, F: FnOnce(&SocketOptions) -> R>(self, f: F) -> R {
        let Self { data: BindingData { id, peer: _, task_data: _, task_control: _ }, ctx } = self;
        ctx.api().tcp().with_socket_options(id, f)
    }
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn spawn_connected_socket_task<I: IpExt + IpSockAddrExt>(
    ctx: Ctx,
    accepted: TcpSocketId<I>,
    peer: zx::Socket,
    request_stream: fposix_socket::StreamSocketRequestStream,
    task_data: TaskSpawnData,
    spawner: &worker::ProviderScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
) {
    spawner.spawn(SocketWorker::<BindingData<I>>::serve_stream_with(
        ctx,
        move |_: &mut Ctx, SocketWorkerProperties {}| BindingData {
            id: accepted,
            peer,
            task_data: Some(task_data),
            task_control: None,
        },
        SocketWorkerProperties {},
        request_stream,
        InitialSocketState::Connected,
        spawner.clone(),
    ))
}

impl<A: IpAddress, D> TryIntoFidlWithContext<<A::Version as IpSockAddrExt>::SocketAddress>
    for SocketAddr<A, D>
where
    A::Version: IpSockAddrExt,
    D: TryIntoFidlWithContext<NonZeroU64>,
{
    type Error = D::Error;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<<A::Version as IpSockAddrExt>::SocketAddress, Self::Error> {
        let Self { ip, port } = self;
        Ok((ip, port).try_into_fidl_with_ctx(ctx)?)
    }
}
