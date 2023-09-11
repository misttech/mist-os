// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Stream sockets, primarily TCP sockets.

use std::{
    convert::Infallible as Never,
    num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU8, NonZeroUsize, TryFromIntError},
    ops::ControlFlow,
    sync::Arc,
    time::Duration,
};

use const_unwrap::const_unwrap_option;
use explicit::ResultExt as _;
use fidl::{
    endpoints::{ClientEnd, RequestStream as _},
    AsHandleRef as _, HandleBased as _,
};
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_posix_socket as fposix_socket;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, Peered as _};
use futures::{future::FusedFuture as _, FutureExt as _, StreamExt as _};
use net_types::{
    ip::{IpAddress, IpVersion, Ipv4, Ipv6},
    ZonedAddr,
};
use netstack3_core::{
    device::{DeviceId, WeakDeviceId},
    ip::IpExt,
    socket::Shutdown,
    transport::tcp::{
        self,
        buffer::{
            Buffer, BufferLimits, IntoBuffers, ReceiveBuffer, RingBuffer, SendBuffer, SendPayload,
        },
        segment::Payload,
        socket::{
            accept, bind, close, connect, create_socket, get_info, get_socket_error, listen,
            receive_buffer_size, reuseaddr, send_buffer_size, set_device, set_receive_buffer_size,
            set_reuseaddr, set_send_buffer_size, shutdown, with_socket_options,
            with_socket_options_mut, AcceptError, BindError, BoundInfo, ConnectError,
            ConnectionInfo, ListenError, ListenerNotifier, NoConnection, SetReuseAddrError,
            SocketAddr, SocketId, SocketInfo,
        },
        state::Takeable,
        BufferSizes, ConnectionError, SocketOptions,
    },
    SyncCtx,
};
use once_cell::sync::Lazy;
use packet_formats::utils::NonZeroDuration;

use crate::bindings::{
    socket::{
        worker::{self, CloseResponder, SocketWorker},
        IntoErrno, IpSockAddrExt, SockAddr, SocketWorkerProperties, ZXSIO_SIGNAL_CONNECTED,
        ZXSIO_SIGNAL_INCOMING,
    },
    trace_duration,
    util::{
        ConversionContext, DeviceNotFoundError, IntoCore, IntoFidl, NeedsDataNotifier,
        NeedsDataWatcher, TryFromFidlWithContext, TryIntoCoreWithContext, TryIntoFidlWithContext,
    },
    BindingsNonSyncCtxImpl, Ctx,
};

/// Maximum values allowed on linux: https://github.com/torvalds/linux/blob/0326074ff4652329f2a1a9c8685104576bd8d131/include/net/tcp.h#L159-L161
const MAX_TCP_KEEPIDLE_SECS: u64 = 32767;
const MAX_TCP_KEEPINTVL_SECS: u64 = 32767;
const MAX_TCP_KEEPCNT: u8 = 127;

#[derive(Debug)]
pub(crate) struct ListenerState(zx::Socket);

/// Local end of a zircon socket pair which will be later provided to state
/// machine inside Core.
#[derive(Debug, Clone)]
pub(crate) struct LocalZirconSocketAndNotifier(Arc<zx::Socket>, NeedsDataNotifier);

impl IntoBuffers<ReceiveBufferWithZirconSocket, SendBufferWithZirconSocket>
    for LocalZirconSocketAndNotifier
{
    fn into_buffers(
        self,
        buffer_sizes: BufferSizes,
    ) -> (ReceiveBufferWithZirconSocket, SendBufferWithZirconSocket) {
        let Self(socket, notifier) = self;
        let BufferSizes { send, receive } = buffer_sizes;
        notifier.schedule();
        socket
            .signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_CONNECTED)
            .expect("failed to signal connection established");
        (
            ReceiveBufferWithZirconSocket::new(Arc::clone(&socket), receive),
            SendBufferWithZirconSocket::new(socket, notifier, send),
        )
    }
}

impl Takeable for LocalZirconSocketAndNotifier {
    fn take(&mut self) -> Self {
        let Self(socket, notifier) = self;
        Self(Arc::clone(&socket), notifier.clone())
    }
}

/// The peer end of the zircon socket that will later be vended to application,
/// together with objects that are used to receive signals from application.
#[derive(Debug)]
pub(crate) struct PeerZirconSocketAndWatcher {
    peer: zx::Socket,
    watcher: NeedsDataWatcher,
    socket: Arc<zx::Socket>,
}

impl ListenerNotifier for LocalZirconSocketAndNotifier {
    fn new_incoming_connections(&mut self, count: usize) {
        let Self(socket, _needs_data) = self;
        let (clear, set) = if count == 0 {
            (ZXSIO_SIGNAL_INCOMING, zx::Signals::NONE)
        } else {
            (zx::Signals::NONE, ZXSIO_SIGNAL_INCOMING)
        };

        socket.signal_peer(clear, set).expect("failed to signal for available connections")
    }
}

impl tcp::socket::NonSyncContext for BindingsNonSyncCtxImpl {
    type ReceiveBuffer = ReceiveBufferWithZirconSocket;
    type SendBuffer = SendBufferWithZirconSocket;
    type ReturnedBuffers = PeerZirconSocketAndWatcher;
    type ListenerNotifierOrProvidedBuffers = LocalZirconSocketAndNotifier;

    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (Self::ReceiveBuffer, Self::SendBuffer, Self::ReturnedBuffers) {
        let (local, peer) = zx::Socket::create_stream();
        let socket = Arc::new(local);
        let notifier = NeedsDataNotifier::default();
        let watcher = notifier.watcher();
        let (rbuf, sbuf) =
            LocalZirconSocketAndNotifier(Arc::clone(&socket), notifier).into_buffers(buffer_sizes);
        (rbuf, sbuf, PeerZirconSocketAndWatcher { peer, socket, watcher })
    }

    fn default_buffer_sizes() -> BufferSizes {
        static ZIRCON_SOCKET_BUFFER_SIZE: Lazy<usize> = Lazy::new(|| {
            let (local, _peer) = zx::Socket::create_stream();
            local.info().unwrap().tx_buf_max
        });
        static RING_BUFFER_DEFAULT_SIZE: Lazy<usize> =
            Lazy::new(|| RingBuffer::default().target_capacity());

        BufferSizes { receive: *ZIRCON_SOCKET_BUFFER_SIZE, send: *RING_BUFFER_DEFAULT_SIZE }
    }
}

#[derive(Debug)]
pub(crate) struct ReceiveBufferWithZirconSocket {
    /// The Zircon socket whose other end is held by the peer.
    ///
    /// This is an Option so that [`Takeable::take`] can leave a sentinel value
    /// in its place. Otherwise it should always have a value.
    socket: Option<Arc<zx::Socket>>,
    zx_socket_capacity: usize,
    // Invariant: `out_of_order` can never hold more bytes than
    // `zx_socket_capacity`.
    out_of_order: RingBuffer,
}

impl ReceiveBufferWithZirconSocket {
    /// The minimum receive buffer size, in bytes.
    ///
    /// Borrowed from Linux: https://man7.org/linux/man-pages/man7/socket.7.html
    const MIN_CAPACITY: usize = 256;

    fn new(socket: Arc<zx::Socket>, target_capacity: usize) -> Self {
        let info = socket.info().expect("failed to get socket info");
        let zx_socket_capacity = info.tx_buf_max;
        assert!(
            zx_socket_capacity >= Self::MIN_CAPACITY,
            "Zircon socket buffer is too small, {} < {}",
            zx_socket_capacity,
            Self::MIN_CAPACITY
        );

        let ring_buffer_size =
            usize::min(usize::max(target_capacity, Self::MIN_CAPACITY), zx_socket_capacity);
        let out_of_order = RingBuffer::new(ring_buffer_size);
        Self { zx_socket_capacity, socket: Some(socket), out_of_order }
    }
}

impl Takeable for ReceiveBufferWithZirconSocket {
    fn take(&mut self) -> Self {
        core::mem::replace(
            self,
            Self {
                zx_socket_capacity: self.zx_socket_capacity,
                socket: None,
                out_of_order: RingBuffer::new(0),
            },
        )
    }
}

impl Buffer for ReceiveBufferWithZirconSocket {
    fn limits(&self) -> BufferLimits {
        let Self { socket, out_of_order, zx_socket_capacity } = self;
        let BufferLimits { len: _, capacity: out_of_order_capacity } = out_of_order.limits();

        debug_assert!(
            *zx_socket_capacity >= out_of_order_capacity,
            "ring buffer should never be this large; {} > {}",
            out_of_order_capacity,
            *zx_socket_capacity
        );

        let info = socket.as_ref().expect("is valid").info().expect("failed to get socket info");
        let len = info.tx_buf_size;
        // Ensure that capacity is always at least as large as the length, but
        // also reflects the requested capacity.
        let capacity = usize::max(len, out_of_order_capacity);
        BufferLimits { len, capacity }
    }

    fn target_capacity(&self) -> usize {
        let Self { socket: _, zx_socket_capacity: _, out_of_order } = self;
        out_of_order.target_capacity()
    }

    fn request_capacity(&mut self, size: usize) {
        let Self { zx_socket_capacity, socket: _, out_of_order } = self;

        let ring_buffer_size =
            usize::min(usize::max(size, Self::MIN_CAPACITY), *zx_socket_capacity);

        out_of_order.set_target_size(ring_buffer_size);
    }
}

impl ReceiveBuffer for ReceiveBufferWithZirconSocket {
    fn write_at<P: Payload>(&mut self, offset: usize, data: &P) -> usize {
        self.out_of_order.write_at(offset, data)
    }

    fn make_readable(&mut self, count: usize) {
        self.out_of_order.make_readable(count);
        let mut shut_rd = false;
        let nread = self.out_of_order.read_with(|avail| {
            let mut total = 0;
            for chunk in avail {
                trace_duration!("zx::Socket::write");
                let written = match self.socket.as_ref().expect("is valid").write(*chunk) {
                    Ok(n) => n,
                    Err(zx::Status::BAD_STATE | zx::Status::PEER_CLOSED) => {
                        // These two status codes correspond two possible cases
                        // where the socket has been shutdown for read:
                        //   - BAD_STATE, the application has called `shutdown`,
                        //     but fido is still holding onto the peer socket.
                        //   - PEER_CLOSED, the application has called `close`,
                        //     or the socket is implicitly closed because the
                        //     application exits, fido is no longer holding onto
                        //     the peer socket, nor do we hold it in our
                        //     `SocketWorker` as it gets dropped after serving
                        //     the last request.
                        // In either case, we just discard the incoming bytes.
                        shut_rd = true;
                        return total;
                    }
                    Err(err) => panic!("failed to write into the zircon socket: {:?}", err),
                };
                assert_eq!(written, chunk.len());
                total += chunk.len();
            }
            total
        });
        // TODO(https://fxbug.dev/112391): Instead of inferring the state in
        // Bindings, we can reclaim the memory more promptly by teaching Core
        // about SHUT_RD.
        if shut_rd {
            let BufferLimits { len: _, capacity } = self.out_of_order.limits();
            if capacity != 0 {
                self.out_of_order = RingBuffer::new(0);
            }
            return;
        }
        assert_eq!(count, nread);
    }
}

impl Drop for ReceiveBufferWithZirconSocket {
    fn drop(&mut self) {
        // Make sure the FDIO is aware that we are not writing anymore so that
        // it can transition into the right state.
        if let Some(socket) = self.socket.as_ref() {
            socket
                .set_disposition(
                    /* disposition */ Some(zx::SocketWriteDisposition::Disabled),
                    /* peer_disposition */ None,
                )
                .expect("failed to set socket disposition");
        }
    }
}

#[derive(Debug)]
pub(crate) struct SendBufferWithZirconSocket {
    zx_socket_capacity: usize,
    socket: Arc<zx::Socket>,
    ready_to_send: RingBuffer,
    notifier: NeedsDataNotifier,
}

impl Buffer for SendBufferWithZirconSocket {
    fn limits(&self) -> BufferLimits {
        let Self { zx_socket_capacity, socket, ready_to_send, notifier } = self;
        let info = socket.info().expect("failed to get socket info");

        let BufferLimits { capacity: ready_to_send_capacity, len: ready_to_send_len } =
            ready_to_send.limits();
        let len = info.rx_buf_size + ready_to_send_len;
        let capacity = *zx_socket_capacity + ready_to_send_capacity;

        // Core checks for limits whenever `tcp::do_send` is hit. If it sees
        // that there's no data in the send buffer, it'll end its attempt to
        // send so we must make sure the watcher in the send task is hit when we
        // observe zero bytes on the zircon socket from here.
        if len == 0 {
            notifier.schedule();
        }
        BufferLimits { capacity, len }
    }

    fn target_capacity(&self) -> usize {
        let Self { zx_socket_capacity, socket: _, ready_to_send, notifier: _ } = self;
        *zx_socket_capacity + ready_to_send.target_capacity()
    }

    fn request_capacity(&mut self, size: usize) {
        let ring_buffer_size = usize::min(usize::max(size, Self::MIN_CAPACITY), Self::MAX_CAPACITY);

        let Self { zx_socket_capacity: _, notifier: _, ready_to_send, socket: _ } = self;

        ready_to_send.set_target_size(ring_buffer_size);

        // Eagerly pull more data out of the Zircon socket into the ring buffer.
        self.poll()
    }
}

impl Takeable for SendBufferWithZirconSocket {
    fn take(&mut self) -> Self {
        let Self { zx_socket_capacity, socket, ready_to_send: data, notifier } = self;
        Self {
            zx_socket_capacity: *zx_socket_capacity,
            socket: Arc::clone(socket),
            ready_to_send: std::mem::replace(data, RingBuffer::new(0)),
            notifier: notifier.clone(),
        }
    }
}

impl SendBufferWithZirconSocket {
    /// The minimum send buffer size, in bytes.
    ///
    /// Borrowed from Linux: https://man7.org/linux/man-pages/man7/socket.7.html
    const MIN_CAPACITY: usize = 2048;
    /// The maximum send buffer size in bytes.
    ///
    /// 4MiB was picked to match Linux's behavior.
    const MAX_CAPACITY: usize = 1 << 22;

    fn new(socket: Arc<zx::Socket>, notifier: NeedsDataNotifier, target_capacity: usize) -> Self {
        let ring_buffer_size =
            usize::min(usize::max(target_capacity, Self::MIN_CAPACITY), Self::MAX_CAPACITY);
        let ready_to_send = RingBuffer::new(ring_buffer_size);
        let info = socket.info().expect("failed to get socket info");
        let zx_socket_capacity = info.rx_buf_max;
        Self { zx_socket_capacity, socket, ready_to_send, notifier }
    }

    fn poll(&mut self) {
        let want_bytes = {
            let BufferLimits { len, capacity } = self.ready_to_send.limits();
            capacity - len
        };
        if want_bytes == 0 {
            return;
        }
        let write_result =
            self.ready_to_send.writable_regions().into_iter().try_fold(0, |acc, b| {
                trace_duration!("zx::Socket::read");
                match self.socket.read(b) {
                    Ok(n) => {
                        if n == b.len() {
                            ControlFlow::Continue(acc + n)
                        } else {
                            ControlFlow::Break(acc + n)
                        }
                    }
                    Err(
                        zx::Status::SHOULD_WAIT | zx::Status::PEER_CLOSED | zx::Status::BAD_STATE,
                    ) => ControlFlow::Break(acc),
                    Err(e) => panic!("failed to read from the zircon socket: {:?}", e),
                }
            });
        let (ControlFlow::Continue(bytes_written) | ControlFlow::Break(bytes_written)) =
            write_result;

        self.ready_to_send.make_readable(bytes_written);
        if bytes_written < want_bytes {
            debug_assert!(write_result.is_break());
            self.notifier.schedule();
        }
    }
}

impl SendBuffer for SendBufferWithZirconSocket {
    fn mark_read(&mut self, count: usize) {
        self.ready_to_send.mark_read(count);
        self.poll()
    }

    fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
    where
        F: FnOnce(SendPayload<'a>) -> R,
    {
        self.poll();
        let Self { ready_to_send, zx_socket_capacity: _, notifier: _, socket: _ } = self;
        // Since the reported readable bytes length includes the bytes in
        // `socket`, a reasonable caller could try to peek at those. Since only
        // the bytes in `ready_to_send` are peekable, don't pass through a
        // request that would result in an out-of-bounds peek.
        let BufferLimits { len, capacity: _ } = ready_to_send.limits();
        if offset >= len {
            f(SendPayload::Contiguous(&[]))
        } else {
            ready_to_send.peek_with(offset, f)
        }
    }
}

struct BindingData<I: IpExt> {
    id: SocketId<I>,
    peer: zx::Socket,
    local_socket_and_watcher: Option<(Arc<zx::Socket>, NeedsDataWatcher)>,
    send_task_abort: Option<futures::channel::oneshot::Sender<()>>,
}

impl<I: IpExt> BindingData<I> {
    fn new(
        sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        non_sync_ctx: &mut BindingsNonSyncCtxImpl,
        properties: SocketWorkerProperties,
    ) -> Self {
        let (local, peer) = zx::Socket::create_stream();
        let local = Arc::new(local);
        let SocketWorkerProperties {} = properties;
        let notifier = NeedsDataNotifier::default();
        let watcher = notifier.watcher();
        let id = create_socket::<I, _>(
            sync_ctx,
            non_sync_ctx,
            LocalZirconSocketAndNotifier(Arc::clone(&local), notifier),
        );
        Self { id, peer, local_socket_and_watcher: Some((local, watcher)), send_task_abort: None }
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

impl<I: IpExt + IpSockAddrExt> worker::SocketWorkerHandler for BindingData<I>
where
    DeviceId<BindingsNonSyncCtxImpl>:
        TryFromFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
    WeakDeviceId<BindingsNonSyncCtxImpl>:
        TryIntoFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
{
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
                let Self { id, peer: _, local_socket_and_watcher, send_task_abort } = self;
                let (socket, watcher) = local_socket_and_watcher
                    .take()
                    .expect("connected socket did not provide socket and watcher");
                let sender =
                    spawn_send_task(ctx.clone(), socket, watcher, *id, &spawners.socket_scope);
                assert_matches::assert_matches!(send_task_abort.replace(sender), None);
            }
        }
    }

    fn handle_request(
        &mut self,
        ctx: &mut Ctx,
        request: Self::Request,
        spawners: &worker::TaskSpawnerCollection<crate::bindings::util::TaskWaitGroupSpawner>,
    ) -> ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request, spawners)
    }

    fn close(
        self,
        sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    ) {
        let Self { mut id, peer, local_socket_and_watcher: _, send_task_abort } = self;
        match shutdown::<I, _>(
            &sync_ctx,
            non_sync_ctx,
            &mut id,
            Shutdown { send: true, receive: true },
        ) {
            Ok(true) => {
                peer.set_disposition(
                    Some(zx::SocketWriteDisposition::Disabled),
                    Some(zx::SocketWriteDisposition::Disabled),
                )
                .expect("failed to set socket disposition");
            }
            Ok(false) | Err(NoConnection) => {}
        }
        close::<I, _>(sync_ctx, non_sync_ctx, id);
        if let Some(send_task_abort) = send_task_abort {
            // Signal the task to stop but drop the canceled error. The data
            // notifier might have been closed in `close` or due to state
            // machine progression.
            send_task_abort.send(()).unwrap_or_else(|()| ());
        }
    }
}

pub(super) fn spawn_worker(
    domain: fposix_socket::Domain,
    proto: fposix_socket::StreamSocketProtocol,
    ctx: crate::bindings::Ctx,
    request_stream: fposix_socket::StreamSocketRequestStream,
    spawner: &worker::ProviderScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
) where
    DeviceId<BindingsNonSyncCtxImpl>: TryFromFidlWithContext<Never, Error = DeviceNotFoundError>
        + TryFromFidlWithContext<NonZeroU64, Error = DeviceNotFoundError>,
{
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
            ConnectionError::ConnectionReset => fposix::Errno::Econnrefused,
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

/// Spawns a task that sends more data from the `socket` each time we observe
/// a wakeup through the `watcher`.
fn spawn_send_task<I: IpExt>(
    mut ctx: crate::bindings::Ctx,
    socket: Arc<zx::Socket>,
    mut watcher: NeedsDataWatcher,
    id: SocketId<I>,
    spawner: &worker::SocketScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
) -> futures::channel::oneshot::Sender<()> {
    let (sender, abort) = futures::channel::oneshot::channel();
    let abort = abort.map(|r| r.expect("send task abort dropped without signaling"));
    let watch_fut = async move {
        let mut signals = futures::future::OptionFuture::default();
        loop {
            let mut watcher_next = watcher.next().fuse();

            // NB: Extract work out of the select macro because rustfmt
            // doesn't like it.
            enum Work {
                Watcher(Option<()>),
                Signals(zx::Signals),
            }
            let work = futures::select! {
                w = watcher_next => Work::Watcher(w),
                s = signals => {
                    Work::Signals(s.expect("OptionFuture is only selected when non-empty"))
                }
            };
            match work {
                Work::Watcher(Some(())) => {
                    // Only create a new signals wait if it's already
                    // terminated, otherwise it means this is a spurious wakeup.
                    if signals.is_terminated() {
                        signals = Some(
                            fasync::OnSignals::new(&*socket, zx::Signals::SOCKET_READABLE)
                                .map(|r| r.expect("failed to observe signals on zircon socket")),
                        )
                        .into()
                    }
                }
                Work::Watcher(None) => break,
                Work::Signals(observed) => {
                    assert!(observed.contains(zx::Signals::SOCKET_READABLE));
                    let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
                    netstack3_core::transport::tcp::socket::do_send::<I, _>(
                        sync_ctx,
                        non_sync_ctx,
                        id.into(),
                    );
                }
            }
        }
    };
    spawner.spawn(async move {
        futures::pin_mut!(watch_fut);
        futures::future::select(watch_fut, abort)
            .map(|_: futures::future::Either<((), _), ((), _)>| ())
            .await;
    });
    sender
}

struct RequestHandler<'a, I: IpExt> {
    data: &'a mut BindingData<I>,
    ctx: &'a mut Ctx,
}

impl<I: IpSockAddrExt + IpExt> RequestHandler<'_, I>
where
    DeviceId<BindingsNonSyncCtxImpl>:
        TryFromFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
    WeakDeviceId<BindingsNonSyncCtxImpl>:
        TryIntoFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
{
    fn bind(self, addr: fnet::SocketAddress) -> Result<(), fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let addr = I::SocketAddress::from_sock_addr(addr)?;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let (addr, port) =
            addr.try_into_core_with_ctx(non_sync_ctx).map_err(IntoErrno::into_errno)?;
        *id = bind::<I, _>(sync_ctx, non_sync_ctx, *id, addr, NonZeroU16::new(port))
            .map_err(IntoErrno::into_errno)?;
        Ok(())
    }

    fn connect(
        self,
        addr: fnet::SocketAddress,
        spawner: &worker::SocketScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
    ) -> Result<(), fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort },
            ctx,
        } = self;

        let addr = I::SocketAddress::from_sock_addr(addr)?;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let (ip, remote_port) =
            addr.try_into_core_with_ctx(&non_sync_ctx).map_err(IntoErrno::into_errno)?;
        let port = NonZeroU16::new(remote_port).ok_or(fposix::Errno::Einval)?;
        let ip = ip.unwrap_or(ZonedAddr::Unzoned(I::LOOPBACK_ADDRESS).into());
        let connection = connect::<I, _>(sync_ctx, non_sync_ctx, *id, SocketAddr { ip, port })
            .map_err(IntoErrno::into_errno)?;
        if let Some((local, watcher)) = self.data.local_socket_and_watcher.take() {
            *id = connection;
            let sender = spawn_send_task::<I>(ctx.clone(), local, watcher, connection, spawner);
            assert_matches::assert_matches!(send_task_abort.replace(sender), None);
            Err(fposix::Errno::Einprogress)
        } else {
            Ok(())
        }
    }

    fn listen(self, backlog: i16) -> Result<(), fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let sync_ctx = ctx.sync_ctx();
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
        const MINIMUM_BACKLOG_SIZE: NonZeroUsize = const_unwrap_option(NonZeroUsize::new(1));
        const MAXIMUM_BACKLOG_SIZE: NonZeroUsize = const_unwrap_option(NonZeroUsize::new(4096));

        let backlog = usize::try_from(backlog).unwrap_or(0);
        let backlog = NonZeroUsize::new(backlog).map_or(MINIMUM_BACKLOG_SIZE, |b| {
            NonZeroUsize::min(MAXIMUM_BACKLOG_SIZE, NonZeroUsize::max(b, MINIMUM_BACKLOG_SIZE))
        });

        *id = listen::<I, _>(sync_ctx, *id, backlog).map_err(IntoErrno::into_errno)?;
        Ok(())
    }

    fn get_sock_name(self) -> Result<fnet::SocketAddress, fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let fidl = match get_info::<I, _>(sync_ctx, *id) {
            SocketInfo::Unbound(_) => return Err(fposix::Errno::Einval),
            SocketInfo::Bound(BoundInfo { addr, port, device: _ }) => {
                (addr, port).try_into_fidl_with_ctx(non_sync_ctx)
            }
            SocketInfo::Connection(ConnectionInfo { local_addr, remote_addr: _, device: _ }) => {
                local_addr.try_into_fidl_with_ctx(non_sync_ctx)
            }
        }
        .map_err(IntoErrno::into_errno)?;
        Ok(fidl.into_sock_addr())
    }

    fn get_peer_name(self) -> Result<fnet::SocketAddress, fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        match get_info::<I, _>(sync_ctx, *id) {
            SocketInfo::Unbound(_) | SocketInfo::Bound(_) => Err(fposix::Errno::Enotconn),
            SocketInfo::Connection(info) => Ok({
                info.remote_addr
                    .try_into_fidl_with_ctx(non_sync_ctx)
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
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;

        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let (accepted, addr, peer) =
            accept::<I, _>(sync_ctx, non_sync_ctx, *id).map_err(IntoErrno::into_errno)?;
        let addr = addr
            .try_into_fidl_with_ctx(non_sync_ctx)
            .unwrap_or_else(|DeviceNotFoundError| panic!("unknown device"))
            .into_sock_addr();
        let PeerZirconSocketAndWatcher { peer, watcher, socket } = peer;
        let (client, request_stream) = crate::bindings::socket::create_request_stream();
        peer.signal_handle(zx::Signals::NONE, ZXSIO_SIGNAL_CONNECTED)
            .expect("failed to signal connection established");
        spawn_connected_socket_task(
            ctx.clone(),
            accepted,
            peer,
            request_stream,
            socket,
            watcher,
            spawner,
        );
        Ok((want_addr.then_some(addr), client))
    }

    fn get_error(self) -> Result<(), fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let sync_ctx = ctx.sync_ctx();
        match get_socket_error(sync_ctx, *id) {
            Some(err) => Err(err.into_errno()),
            None => Ok(()),
        }
    }

    fn shutdown(self, mode: fposix_socket::ShutdownMode) -> Result<(), fposix::Errno> {
        let Self {
            data: BindingData { id, peer, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let shutdown_recv = mode.contains(fposix_socket::ShutdownMode::READ);
        let shutdown_send = mode.contains(fposix_socket::ShutdownMode::WRITE);
        let is_conn = shutdown::<I, _>(
            &sync_ctx,
            non_sync_ctx,
            id,
            Shutdown { send: shutdown_send, receive: shutdown_recv },
        )
        .map_err(IntoErrno::into_errno)?;
        if is_conn {
            let peer_disposition = shutdown_send.then_some(zx::SocketWriteDisposition::Disabled);
            let my_disposition = shutdown_recv.then_some(zx::SocketWriteDisposition::Disabled);
            peer.set_disposition(peer_disposition, my_disposition)
                .expect("failed to set socket disposition");
        }
        Ok(())
    }

    fn set_bind_to_device(self, device: Option<&str>) -> Result<(), fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let device = device
            .map(|name| non_sync_ctx.devices.get_device_by_name(name).ok_or(fposix::Errno::Enodev))
            .transpose()?;

        set_device(sync_ctx, non_sync_ctx, *id, device).map_err(IntoErrno::into_errno)
    }

    fn set_send_buffer_size(self, new_size: u64) {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let new_size =
            usize::try_from(new_size).ok_checked::<TryFromIntError>().unwrap_or(usize::MAX);
        set_send_buffer_size(sync_ctx, non_sync_ctx, *id, new_size);
    }

    fn send_buffer_size(self) -> u64 {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        send_buffer_size(sync_ctx, non_sync_ctx, *id)
            // If the socket doesn't have a send buffer (e.g. because it was shut
            // down for writing and all the data was sent to the peer), return 0.
            .unwrap_or(0)
            .try_into()
            .ok_checked::<TryFromIntError>()
            .unwrap_or(u64::MAX)
    }

    fn set_receive_buffer_size(self, new_size: u64) {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let new_size =
            usize::try_from(new_size).ok_checked::<TryFromIntError>().unwrap_or(usize::MAX);
        set_receive_buffer_size(sync_ctx, non_sync_ctx, *id, new_size);
    }

    fn receive_buffer_size(self) -> u64 {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        receive_buffer_size(sync_ctx, non_sync_ctx, *id)
            // If the socket doesn't have a receive buffer (e.g. because the remote
            // end signalled FIN and all data was sent to the client), return 0.
            .unwrap_or(0)
            .try_into()
            .ok_checked::<TryFromIntError>()
            .unwrap_or(u64::MAX)
    }

    fn set_reuse_address(self, value: bool) -> Result<(), fposix::Errno> {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let sync_ctx = ctx.sync_ctx();
        set_reuseaddr(sync_ctx, *id, value).map_err(IntoErrno::into_errno)
    }

    fn reuse_address(self) -> bool {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let sync_ctx = ctx.sync_ctx();
        reuseaddr(sync_ctx, *id)
    }

    /// Returns a [`ControlFlow`] to indicate whether the parent stream should
    /// continue being polled or dropped.
    ///
    /// If `Some(stream)` is returned in the `Continue` case, `stream` is a new
    /// stream of events that should be polled concurrently with the parent
    /// stream.
    fn handle_request(
        self,
        request: fposix_socket::StreamSocketRequest,
        spawners: &worker::TaskSpawnerCollection<crate::bindings::util::TaskWaitGroupSpawner>,
    ) -> ControlFlow<
        fposix_socket::StreamSocketCloseResponder,
        Option<fposix_socket::StreamSocketRequestStream>,
    > {
        let Self {
            data: BindingData { id: _, peer, local_socket_and_watcher: _, send_task_abort: _ },
            ctx: _,
        } = self;
        match request {
            fposix_socket::StreamSocketRequest::Bind { addr, responder } => {
                responder
                    .send(self.bind(addr))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::Connect { addr, responder } => {
                // Connect always spawns on the socket scope.
                let response = self.connect(addr, &spawners.socket_scope);
                responder
                    .send(response)
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
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
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::Listen { backlog, responder } => {
                responder
                    .send(self.listen(backlog))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
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
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::Close { responder } => {
                // We don't just close the socket because this socket worker is
                // potentially shared by a bunch of sockets because the client
                // can call `dup` on this socket. We will do the cleanup at the
                // end of this task.
                return ControlFlow::Break(responder);
            }
            fposix_socket::StreamSocketRequest::Clone2 { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel())
                    .expect("failed to create async channel");
                let rs = fposix_socket::StreamSocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(rs));
            }
            fposix_socket::StreamSocketRequest::SetBindToDevice { value, responder } => {
                let identifier = (!value.is_empty()).then_some(value.as_str());
                responder
                    .send(self.set_bind_to_device(identifier))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::Query { responder } => {
                responder
                    .send(fposix_socket::STREAM_SOCKET_PROTOCOL_NAME.as_bytes())
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetReuseAddress { value, responder } => {
                responder
                    .send(self.set_reuse_address(value))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetReuseAddress { responder } => {
                responder
                    .send(Ok(self.reuse_address()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetError { responder } => {
                responder
                    .send(self.get_error())
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetBroadcast { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetBroadcast { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetSendBuffer { value_bytes, responder } => {
                self.set_send_buffer_size(value_bytes);
                responder
                    .send(Ok(()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetSendBuffer { responder } => {
                responder
                    .send(Ok(self.send_buffer_size()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetReceiveBuffer { value_bytes, responder } => {
                responder
                    .send(Ok(self.set_receive_buffer_size(value_bytes)))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetReceiveBuffer { responder } => {
                responder
                    .send(Ok(self.receive_buffer_size()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetKeepAlive { value: enabled, responder } => {
                self.with_socket_options_mut(|so| so.keep_alive.enabled = enabled);
                responder
                    .send(Ok(()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetKeepAlive { responder } => {
                let enabled = self.with_socket_options(|so| so.keep_alive.enabled);
                responder
                    .send(Ok(enabled))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetOutOfBandInline { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetOutOfBandInline { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetNoCheck { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetNoCheck { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetLinger {
                linger: _,
                length_secs: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetLinger { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetReusePort { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetReusePort { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetAcceptConn { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetBindToDevice { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTimestamp { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTimestamp { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetOriginalDestination { responder } => {
                // When we support NAT, we should return the original address.
                responder
                    .send(Err(fposix::Errno::Enoent))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::Disconnect { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetSockName { responder } => {
                responder
                    .send(self.get_sock_name().as_ref().map_err(|e| *e))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetPeerName { responder } => {
                responder
                    .send(self.get_peer_name().as_ref().map_err(|e| *e))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::Shutdown { mode, responder } => {
                responder
                    .send(self.shutdown(mode))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpTypeOfService { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpTypeOfService { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpTtl { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpTtl { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpPacketInfo { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpPacketInfo { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpReceiveTypeOfService {
                value: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpReceiveTypeOfService { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpReceiveTtl { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpReceiveTtl { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpMulticastInterface {
                iface: _,
                address: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpMulticastInterface { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpMulticastTtl { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpMulticastTtl { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpMulticastLoopback { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpMulticastLoopback { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::AddIpMembership { membership: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::DropIpMembership { membership: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpTransparent { value: _, responder } => {
                // In theory this can be used on stream sockets, but we don't need it right now.
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpTransparent { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpReceiveOriginalDestinationAddress {
                value: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpReceiveOriginalDestinationAddress {
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::AddIpv6Membership { membership: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::DropIpv6Membership { membership: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6MulticastInterface {
                value: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6MulticastInterface { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6UnicastHops { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6UnicastHops { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6ReceiveHopLimit { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6ReceiveHopLimit { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6MulticastHops { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6MulticastHops { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6MulticastLoopback {
                value: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6MulticastLoopback { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6Only { value, responder } => {
                // TODO(https://fxbug.dev/21198): support dual-stack sockets.
                responder
                    .send(
                        match I::VERSION {
                            IpVersion::V6 => value,
                            IpVersion::V4 => false,
                        }
                        .then_some(())
                        .ok_or(fposix::Errno::Eopnotsupp),
                    )
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6Only { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6ReceiveTrafficClass {
                value: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6ReceiveTrafficClass { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6TrafficClass { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6TrafficClass { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetIpv6ReceivePacketInfo {
                value: _,
                responder,
            } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetIpv6ReceivePacketInfo { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetInfo { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            // Note for the following two options:
            // Nagle enabled means TCP delays sending segment, thus meaning
            // TCP_NODELAY is turned off. They have opposite meanings.
            fposix_socket::StreamSocketRequest::SetTcpNoDelay { value, responder } => {
                self.with_socket_options_mut(|so| {
                    so.nagle_enabled = !value;
                });
                responder
                    .send(Ok(()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpNoDelay { responder } => {
                let nagle_enabled = self.with_socket_options(|so| so.nagle_enabled);
                responder
                    .send(Ok(!nagle_enabled))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpMaxSegment { value_bytes: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpMaxSegment { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpCork { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpCork { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpKeepAliveIdle { value_secs, responder } => {
                match NonZeroU64::new(value_secs.into())
                    .filter(|value_secs| value_secs.get() <= MAX_TCP_KEEPIDLE_SECS)
                {
                    Some(secs) => {
                        self.with_socket_options_mut(|so| {
                            so.keep_alive.idle = NonZeroDuration::from_nonzero_secs(secs)
                        });
                        responder
                            .send(Ok(()))
                            .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
                    }
                    None => {
                        responder
                            .send(Err(fposix::Errno::Einval))
                            .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
                    }
                }
            }
            fposix_socket::StreamSocketRequest::GetTcpKeepAliveIdle { responder } => {
                let secs =
                    self.with_socket_options(|so| Duration::from(so.keep_alive.idle).as_secs());
                responder
                    .send(Ok(u32::try_from(secs).unwrap()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
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
                        responder
                            .send(Ok(()))
                            .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
                    }
                    None => {
                        responder
                            .send(Err(fposix::Errno::Einval))
                            .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
                    }
                }
            }
            fposix_socket::StreamSocketRequest::GetTcpKeepAliveInterval { responder } => {
                let secs =
                    self.with_socket_options(|so| Duration::from(so.keep_alive.interval).as_secs());
                responder
                    .send(Ok(u32::try_from(secs).unwrap()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
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
                        responder
                            .send(Ok(()))
                            .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
                    }
                    None => {
                        responder
                            .send(Err(fposix::Errno::Einval))
                            .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
                    }
                };
            }
            fposix_socket::StreamSocketRequest::GetTcpKeepAliveCount { responder } => {
                let count = self.with_socket_options(|so| so.keep_alive.count);
                responder
                    .send(Ok(u32::from(u8::from(count))))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
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
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpSynCount { responder } => {
                let syn_cnt = self.with_socket_options(|so| u32::from(so.max_syn_retries.get()));
                responder
                    .send(Ok(syn_cnt))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
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
                responder
                    .send(Ok(()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpLinger { responder } => {
                let linger_secs =
                    self.with_socket_options(|so| so.fin_wait2_timeout.map(|d| d.as_secs()));
                let respond_value = linger_secs.map(|x| u32::try_from(x).unwrap()).into_fidl();
                responder
                    .send(Ok(&respond_value))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpDeferAccept { value_secs: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpDeferAccept { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpWindowClamp { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpWindowClamp { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpInfo { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpQuickAck { value, responder } => {
                self.with_socket_options_mut(|so| so.delayed_ack = !value);
                responder
                    .send(Ok(()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpQuickAck { responder } => {
                let quick_ack = self.with_socket_options(|so| !so.delayed_ack);
                responder
                    .send(Ok(quick_ack))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpCongestion { value: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpCongestion { responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::SetTcpUserTimeout { value_millis, responder } => {
                let user_timeout =
                    NonZeroU64::new(value_millis.into()).map(NonZeroDuration::from_nonzero_millis);
                self.with_socket_options_mut(|so| {
                    so.user_timeout = user_timeout;
                });
                responder
                    .send(Ok(()))
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
            fposix_socket::StreamSocketRequest::GetTcpUserTimeout { responder } => {
                let millis = self.with_socket_options(|so| {
                    so.user_timeout.map(|d| d.get().as_millis()).unwrap_or(0)
                });
                let result =
                    u32::try_from(millis).map_err(|_: TryFromIntError| fposix::Errno::Einval);
                responder
                    .send(result)
                    .unwrap_or_else(|e| tracing::error!("failed to respond: {e:?}"));
            }
        }
        ControlFlow::Continue(None)
    }

    fn with_socket_options_mut<R, F: FnOnce(&mut SocketOptions) -> R>(self, f: F) -> R {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        with_socket_options_mut(sync_ctx, non_sync_ctx, *id, f)
    }

    fn with_socket_options<R, F: FnOnce(&SocketOptions) -> R>(self, f: F) -> R {
        let Self {
            data: BindingData { id, peer: _, local_socket_and_watcher: _, send_task_abort: _ },
            ctx,
        } = self;
        with_socket_options(ctx.sync_ctx(), *id, f)
    }
}

fn spawn_connected_socket_task<I: IpExt + IpSockAddrExt>(
    ctx: Ctx,
    accepted: SocketId<I>,
    peer: zx::Socket,
    request_stream: fposix_socket::StreamSocketRequestStream,
    local_socket: Arc<zx::Socket>,
    watcher: NeedsDataWatcher,
    spawner: &worker::ProviderScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
) where
    DeviceId<BindingsNonSyncCtxImpl>:
        TryFromFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
    WeakDeviceId<BindingsNonSyncCtxImpl>:
        TryIntoFidlWithContext<<I::SocketAddress as SockAddr>::Zone, Error = DeviceNotFoundError>,
{
    spawner.spawn(SocketWorker::<BindingData<I>>::serve_stream_with(
        ctx,
        move |_: &SyncCtx<_>, _: &mut BindingsNonSyncCtxImpl, SocketWorkerProperties {}| {
            BindingData {
                id: accepted,
                peer,
                local_socket_and_watcher: Some((local_socket, watcher)),
                send_task_abort: None,
            }
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
    D: TryIntoFidlWithContext<
        <<A::Version as IpSockAddrExt>::SocketAddress as SockAddr>::Zone,
        Error = DeviceNotFoundError,
    >,
{
    type Error = DeviceNotFoundError;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<<A::Version as IpSockAddrExt>::SocketAddress, Self::Error> {
        let Self { ip, port } = self;
        Ok((ip, port).try_into_fidl_with_ctx(ctx)?)
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use test_case::test_case;

    use super::*;

    const TEST_BYTES: &'static [u8] = b"Hello";

    #[test]
    fn receive_buffer() {
        let (local, peer) = zx::Socket::create_stream();
        let mut rbuf = ReceiveBufferWithZirconSocket::new(Arc::new(local), u16::MAX as usize);
        assert_eq!(rbuf.write_at(0, &TEST_BYTES), TEST_BYTES.len());
        assert_eq!(rbuf.write_at(TEST_BYTES.len() * 2, &TEST_BYTES), TEST_BYTES.len());
        assert_eq!(rbuf.write_at(TEST_BYTES.len(), &TEST_BYTES), TEST_BYTES.len());
        rbuf.make_readable(TEST_BYTES.len() * 3);
        let mut buf = [0u8; TEST_BYTES.len() * 3];
        assert_eq!(rbuf.limits().len, TEST_BYTES.len() * 3);
        assert_eq!(peer.read(&mut buf), Ok(TEST_BYTES.len() * 3));
        assert_eq!(&buf, b"HelloHelloHello");
    }

    #[test]
    fn send_buffer() {
        let (local, peer) = zx::Socket::create_stream();
        let notifier = NeedsDataNotifier::default();
        let mut sbuf =
            SendBufferWithZirconSocket::new(Arc::new(local), notifier, u16::MAX as usize);
        assert_eq!(peer.write(TEST_BYTES), Ok(TEST_BYTES.len()));
        assert_eq!(sbuf.limits().len, TEST_BYTES.len());
        sbuf.peek_with(0, |avail| {
            assert_eq!(avail, SendPayload::Contiguous(TEST_BYTES));
        });
        assert_eq!(peer.write(TEST_BYTES), Ok(TEST_BYTES.len()));
        assert_eq!(sbuf.limits().len, TEST_BYTES.len() * 2);
        sbuf.mark_read(TEST_BYTES.len());
        assert_eq!(sbuf.limits().len, TEST_BYTES.len());
        sbuf.peek_with(0, |avail| {
            assert_eq!(avail, SendPayload::Contiguous(TEST_BYTES));
        });
    }

    #[test_case(0, SendBufferWithZirconSocket::MIN_CAPACITY; "below min")]
    #[test_case(1 << 16, 1 << 16; "in range")]
    #[test_case(1 << 32, SendBufferWithZirconSocket::MAX_CAPACITY; "above max")]
    fn send_buffer_limits(target: usize, expected: usize) {
        let (local, _peer) = zx::Socket::create_stream();
        let notifier = NeedsDataNotifier::default();
        let sbuf = SendBufferWithZirconSocket::new(Arc::new(local), notifier, target);
        let ring_buffer_capacity = sbuf.limits().capacity - sbuf.socket.info().unwrap().rx_buf_max;
        assert_eq!(ring_buffer_capacity, expected)
    }

    #[test]
    fn send_buffer_peek_past_ring_buffer() {
        let (local, peer) = zx::Socket::create_stream();
        let mut sbuf = SendBufferWithZirconSocket::new(
            Arc::new(local),
            NeedsDataNotifier::default(),
            SendBufferWithZirconSocket::MIN_CAPACITY,
        );

        // Fill the send buffer up completely.
        const BYTES: [u8; 1024] = [1; 1024];
        loop {
            match peer.write(&BYTES) {
                Ok(0) | Err(zx::Status::SHOULD_WAIT) => break,
                Ok(_) => sbuf.poll(),
                Err(e) => panic!("couldn't write: {:?}", e),
            }
        }

        assert!(
            sbuf.limits().len > SendBufferWithZirconSocket::MIN_CAPACITY,
            "len includes zx socket"
        );

        // Peeking past the end of the ring buffer should not cause a crash.
        sbuf.peek_with(SendBufferWithZirconSocket::MIN_CAPACITY, |payload| {
            assert_matches!(payload, SendPayload::Contiguous(&[]))
        })
    }

    const LARGE_PAYLOAD: [u8; 1 << 12] = [b'a'; 1 << 12];

    /// Fills up the ring buffer and zircon socket.
    fn fill(peer: &zx::Socket, sbuf: &mut SendBufferWithZirconSocket) {
        while peer.write(&LARGE_PAYLOAD[..]).map_or(false, |l| l == LARGE_PAYLOAD.len()) {
            sbuf.poll();
        }
    }

    #[test]
    fn send_buffer_resize_empties_zircon_socket() {
        // Regression test for https://fxbug.dev/119242.
        let (local, peer) = zx::Socket::create_stream();
        let notifier = NeedsDataNotifier::default();
        let mut sbuf = SendBufferWithZirconSocket::new(
            Arc::new(local),
            notifier,
            SendBufferWithZirconSocket::MIN_CAPACITY,
        );

        // Fill up the ring buffer and zircon socket.
        fill(&peer, &mut sbuf);

        sbuf.request_capacity(SendBufferWithZirconSocket::MIN_CAPACITY + TEST_BYTES.len());
        assert_eq!(peer.write(TEST_BYTES), Ok(TEST_BYTES.len()));
    }

    #[test]
    fn send_buffer_resize_down_capacity() {
        // Regression test for https://fxbug.dev/121449.
        let (local, peer) = zx::Socket::create_stream();
        let notifier = NeedsDataNotifier::default();
        let mut sbuf = SendBufferWithZirconSocket::new(
            Arc::new(local),
            notifier,
            SendBufferWithZirconSocket::MAX_CAPACITY,
        );

        // Fill up the ring buffer and zircon socket.
        fill(&peer, &mut sbuf);

        // Request a shrink of the send buffer.
        let capacity_before = sbuf.limits().capacity;
        sbuf.request_capacity(SendBufferWithZirconSocket::MIN_CAPACITY);

        // Empty out the ring buffer and zircon socket by reading from them.
        while {
            let len = sbuf.peek_with(0, |payload| payload.len());
            sbuf.mark_read(len);
            len != 0
        } {}

        let capacity = sbuf.limits().capacity;
        // The requested capacity isn't directly reflected in `cap` but we can
        // assert that its change is equal to the requested change.
        const EXPECTED_CAPACITY_DECREASE: usize =
            SendBufferWithZirconSocket::MAX_CAPACITY - SendBufferWithZirconSocket::MIN_CAPACITY;
        assert_eq!(
            capacity,
            capacity_before - EXPECTED_CAPACITY_DECREASE,
            "capacity_before: {}, expected decrease: {}",
            capacity_before,
            EXPECTED_CAPACITY_DECREASE
        );

        // The socket's capacity is a measure of how many readable bytes it can
        // hold. If the socket is implemented correctly, this loop will continue
        // until the send buffer's ring buffer is full and the socket buffer is
        // full, then exit.
        while sbuf.limits().len < capacity {
            let _: usize = peer.write(&LARGE_PAYLOAD[..]).expect("can write");
            sbuf.poll();
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn send_buffer_installs_notifier_on_empty_limits_check() {
        let (local, peer) = zx::Socket::create_stream();
        let notifier = NeedsDataNotifier::default();
        let mut watcher = notifier.watcher();
        let sbuf = SendBufferWithZirconSocket::new(
            Arc::new(local),
            notifier,
            SendBufferWithZirconSocket::MAX_CAPACITY,
        );
        // Watcher starts without pending data.
        assert_eq!(futures::poll!(watcher.next()), futures::task::Poll::Pending);

        // Check initial limits, there's no data and the watcher should be
        // asserted once.
        let BufferLimits { len, capacity: _ } = sbuf.limits();
        assert_eq!(len, 0);
        assert_eq!(futures::poll!(watcher.next()), futures::task::Poll::Ready(Some(())));
        assert_eq!(futures::poll!(watcher.next()), futures::task::Poll::Pending);

        // Send data from the peer. Limits returns the available data and
        // doesn't wake the watcher.
        let peer_data = [1, 2, 3, 4];
        assert_eq!(peer.write(&peer_data[..]).expect("write to peer"), peer_data.len());
        let BufferLimits { len, capacity: _ } = sbuf.limits();
        assert_eq!(len, peer_data.len());
        assert_eq!(futures::poll!(watcher.next()), futures::task::Poll::Pending);
    }
}
