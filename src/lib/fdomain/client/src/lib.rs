// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_message::TransactionHeader;
use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot::Sender as OneshotSender;
use futures::stream::Stream as StreamTrait;
use futures::FutureExt;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{ready, Context, Poll, Waker};
use {fidl_fuchsia_fdomain as proto, fuchsia_async as _};

mod channel;
mod event;
mod event_pair;
mod handle;
mod responder;
mod socket;

#[cfg(test)]
mod test;

pub mod fidl;

use responder::{Responder, ResponderStatus};

pub use channel::{
    AnyHandle, Channel, ChannelMessageStream, ChannelWriter, HandleInfo, MessageBuf,
};
pub use event::Event;
pub use event_pair::Eventpair as EventPair;
pub use handle::{AsHandleRef, Handle, HandleBased, HandleRef, OnFDomainSignals, Peered};
pub use proto::{Error as FDomainError, WriteChannelError, WriteSocketError};
pub use socket::{Socket, SocketDisposition, SocketReadStream, SocketWriter};

// Unsupported handle types.
#[rustfmt::skip]
pub use Handle as Fifo;
#[rustfmt::skip]
pub use Handle as Job;
#[rustfmt::skip]
pub use Handle as Process;
#[rustfmt::skip]
pub use Handle as Resource;
#[rustfmt::skip]
pub use Handle as Stream;
#[rustfmt::skip]
pub use Handle as Thread;
#[rustfmt::skip]
pub use Handle as Vmar;
#[rustfmt::skip]
pub use Handle as Vmo;

fdomain_macros::extract_ordinals_env!("FDOMAIN_FIDL_PATH");

fn write_fdomain_error(error: &FDomainError, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match error {
        FDomainError::TargetError(e) => write!(f, "Target-side error {e}"),
        FDomainError::BadHandleId(proto::BadHandleId { id }) => {
            write!(f, "Tried to use invalid handle id {id}")
        }
        FDomainError::WrongHandleType(proto::WrongHandleType { expected, got }) => write!(
            f,
            "Tried to use handle as {expected:?} but target reported handle was of type {got:?}"
        ),
        FDomainError::StreamingReadInProgress(proto::StreamingReadInProgress {}) => {
            write!(f, "Handle is occupied delivering streaming reads")
        }
        FDomainError::NoReadInProgress(proto::NoReadInProgress {}) => {
            write!(f, "No streaming read was in progress")
        }
        FDomainError::NoErrorPending(proto::NoErrorPending {}) => {
            write!(f, "Tried to dismiss write errors on handle where none had occurred")
        }
        FDomainError::NewHandleIdOutOfRange(proto::NewHandleIdOutOfRange { id }) => {
            write!(f, "Tried to create a handle with id {id}, which is outside the valid range for client handles")
        }
        FDomainError::NewHandleIdReused(proto::NewHandleIdReused { id, same_call }) => {
            if *same_call {
                write!(f, "Tried to create two or more new handles with the same id {id}")
            } else {
                write!(f, "Tried to create a new handle with id {id}, which is already the id of an existing handle")
            }
        }
        FDomainError::ErrorPending(proto::ErrorPending {}) => {
            write!(f, "Cannot write to handle again without dismissing previous write error")
        }
        FDomainError::WroteToSelf(proto::WroteToSelf {}) => {
            write!(f, "Tried to write a channel into itself")
        }
        FDomainError::ClosedDuringRead(proto::ClosedDuringRead {}) => {
            write!(f, "Handle closed while being read")
        }
        _ => todo!(),
    }
}

/// Result type alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error type emitted by FDomain operations.
#[derive(Clone)]
pub enum Error {
    SocketWrite(WriteSocketError),
    ChannelWrite(WriteChannelError),
    FDomain(FDomainError),
    Protocol(::fidl::Error),
    ProtocolObjectTypeIncompatible,
    ProtocolRightsIncompatible,
    ProtocolSignalsIncompatible,
    ProtocolStreamEventIncompatible,
    Transport(Arc<std::io::Error>),
    ConnectionMismatch,
    StreamingAborted,
    ClientLost,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SocketWrite(proto::WriteSocketError { error, wrote }) => {
                write!(f, "While writing socket (after {wrote} bytes written successfully): ")?;
                write_fdomain_error(error, f)
            }
            Self::ChannelWrite(proto::WriteChannelError::Error(error)) => {
                write!(f, "While writing channel: ")?;
                write_fdomain_error(error, f)
            }
            Self::ChannelWrite(proto::WriteChannelError::OpErrors(errors)) => {
                write!(f, "Couldn't write all handles into a channel:")?;
                for (pos, error) in
                    errors.iter().enumerate().filter_map(|(num, x)| x.as_ref().map(|y| (num, &**y)))
                {
                    write!(f, "\n  Handle in position {pos}: ")?;
                    write_fdomain_error(error, f)?;
                }
                Ok(())
            }
            Self::ProtocolObjectTypeIncompatible => {
                write!(f, "The FDomain protocol does not recognize an object type")
            }
            Self::ProtocolRightsIncompatible => {
                write!(f, "The FDomain protocol does not recognize some rights")
            }
            Self::ProtocolSignalsIncompatible => {
                write!(f, "The FDomain protocol does not recognize some signals")
            }
            Self::ProtocolStreamEventIncompatible => {
                write!(f, "The FDomain protocol does not recognize a received streaming IO event")
            }
            Self::FDomain(e) => write_fdomain_error(e, f),
            Self::Protocol(e) => write!(f, "Protocol error: {e}"),
            Self::Transport(e) => write!(f, "Transport error: {e:?}"),
            Self::ConnectionMismatch => {
                write!(f, "Tried to use an FDomain handle from a different connection")
            }
            Self::StreamingAborted => write!(f, "This channel is no longer streaming"),
            Self::ClientLost => write!(f, "The client associated with this handle was destroyed"),
        }
    }
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SocketWrite(e) => f.debug_tuple("SocketWrite").field(e).finish(),
            Self::ChannelWrite(e) => f.debug_tuple("ChannelWrite").field(e).finish(),
            Self::FDomain(e) => f.debug_tuple("FDomain").field(e).finish(),
            Self::Protocol(e) => f.debug_tuple("Protocol").field(e).finish(),
            Self::Transport(e) => f.debug_tuple("Transport").field(e).finish(),
            Self::ProtocolObjectTypeIncompatible => write!(f, "ProtocolObjectTypeIncompatible "),
            Self::ProtocolRightsIncompatible => write!(f, "ProtocolRightsIncompatible "),
            Self::ProtocolSignalsIncompatible => write!(f, "ProtocolSignalsIncompatible "),
            Self::ProtocolStreamEventIncompatible => write!(f, "ProtocolStreamEventIncompatible"),
            Self::ConnectionMismatch => write!(f, "ConnectionMismatch"),
            Self::StreamingAborted => write!(f, "StreamingAborted"),
            Self::ClientLost => write!(f, "ClientLost"),
        }
    }
}

impl std::error::Error for Error {}

impl From<FDomainError> for Error {
    fn from(other: FDomainError) -> Self {
        Self::FDomain(other)
    }
}

impl From<::fidl::Error> for Error {
    fn from(other: ::fidl::Error) -> Self {
        Self::Protocol(other)
    }
}

impl From<WriteSocketError> for Error {
    fn from(other: WriteSocketError) -> Self {
        Self::SocketWrite(other)
    }
}

impl From<WriteChannelError> for Error {
    fn from(other: WriteChannelError) -> Self {
        Self::ChannelWrite(other)
    }
}

/// An error emitted internally by the client. Similar to [`Error`] but does not
/// contain several variants which are irrelevant in the contexts where it is
/// used.
enum InnerError {
    Protocol(::fidl::Error),
    ProtocolStreamEventIncompatible,
    Transport(Arc<std::io::Error>),
}

impl Clone for InnerError {
    fn clone(&self) -> Self {
        match self {
            InnerError::Protocol(a) => InnerError::Protocol(a.clone()),
            InnerError::ProtocolStreamEventIncompatible => {
                InnerError::ProtocolStreamEventIncompatible
            }
            InnerError::Transport(a) => InnerError::Transport(Arc::clone(a)),
        }
    }
}

impl From<InnerError> for Error {
    fn from(other: InnerError) -> Self {
        match other {
            InnerError::Protocol(p) => Error::Protocol(p),
            InnerError::ProtocolStreamEventIncompatible => Error::ProtocolStreamEventIncompatible,
            InnerError::Transport(t) => Error::Transport(t),
        }
    }
}

impl From<::fidl::Error> for InnerError {
    fn from(other: ::fidl::Error) -> Self {
        InnerError::Protocol(other)
    }
}

// TODO(399717689) Figure out if we could just use AsyncRead/Write instead of a special trait.
/// Implemented by objects which provide a transport over which we can speak the
/// FDomain protocol.
///
/// The implementer must provide two things:
/// 1) An incoming stream of messages presented as `Vec<u8>`. This is provided
///    via the `Stream` trait, which this trait requires.
/// 2) A way to send messages. This is provided by implementing the
///    `poll_send_message` method.
pub trait FDomainTransport: StreamTrait<Item = Result<Box<[u8]>, std::io::Error>> + Send {
    /// Attempt to send a message asynchronously. Messages should be sent so
    /// that they arrive at the target in order.
    fn poll_send_message(
        self: Pin<&mut Self>,
        msg: &[u8],
        ctx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>>;
}

/// Wrapper for an `FDomainTransport` implementer that:
/// 1) Provides a queue for outgoing messages so we need not have an await point
///    when we submit a message.
/// 2) Drops the transport on error, then returns the last observed error for
///    all future operations.
enum Transport {
    Transport(Pin<Box<dyn FDomainTransport>>, VecDeque<Box<[u8]>>, Vec<Waker>),
    Error(InnerError),
}

impl Transport {
    /// Get the failure mode of the transport if it has failed.
    fn error(&self) -> Option<InnerError> {
        match self {
            Transport::Transport(_, _, _) => None,
            Transport::Error(inner_error) => Some(inner_error.clone()),
        }
    }

    /// Enqueue a message to be sent on this transport.
    fn push_msg(&mut self, msg: Box<[u8]>) {
        if let Transport::Transport(_, v, w) = self {
            v.push_back(msg);
            w.drain(..).for_each(Waker::wake);
        }
    }

    /// Push messages in the send queue out through the transport.
    fn poll_send_messages(&mut self, ctx: &mut Context<'_>) -> Poll<InnerError> {
        match self {
            Transport::Error(e) => Poll::Ready(e.clone()),
            Transport::Transport(t, v, w) => {
                while let Some(msg) = v.front() {
                    match t.as_mut().poll_send_message(msg, ctx) {
                        Poll::Ready(Ok(())) => {
                            v.pop_front();
                        }
                        Poll::Ready(Err(e)) => {
                            let e = Arc::new(e);
                            *self = Transport::Error(InnerError::Transport(Arc::clone(&e)));
                            return Poll::Ready(InnerError::Transport(e));
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }

                if v.is_empty() {
                    w.push(ctx.waker().clone());
                } else {
                    ctx.waker().wake_by_ref();
                }
                Poll::Pending
            }
        }
    }

    /// Get the next incoming message from the transport.
    fn poll_next(&mut self, ctx: &mut Context<'_>) -> Poll<Option<Result<Box<[u8]>, InnerError>>> {
        match self {
            Transport::Error(e) => Poll::Ready(Some(Err(e.clone()))),
            Transport::Transport(t, _, _) => match ready!(t.as_mut().poll_next(ctx)) {
                Some(Ok(x)) => Poll::Ready(Some(Ok(x))),
                Some(Err(e)) => {
                    let e = Arc::new(e);
                    *self = Transport::Error(InnerError::Transport(Arc::clone(&e)));
                    Poll::Ready(Some(Err(InnerError::Transport(e))))
                }
                Option::None => Poll::Ready(None),
            },
        }
    }
}

/// State of a channel that is or has been read from.
struct ChannelReadState {
    wakers: Vec<Waker>,
    queued: VecDeque<Result<proto::ChannelMessage, Error>>,
    read_request_pending: bool,
    is_streaming: bool,
}

impl ChannelReadState {
    /// Handle an incoming message, which is either a channel streaming event or
    /// response to a `ChannelRead` request.
    fn handle_incoming_message(&mut self, msg: Result<proto::ChannelMessage, Error>) {
        self.queued.push_back(msg);
        self.wakers.drain(..).for_each(Waker::wake);
    }
}

/// Lock-protected interior of `Client`
struct ClientInner {
    transport: Transport,
    transactions: HashMap<NonZeroU32, responder::Responder>,
    socket_read_subscriptions: HashMap<proto::HandleId, UnboundedSender<Result<Vec<u8>, Error>>>,
    channel_read_states: HashMap<proto::HandleId, ChannelReadState>,
    next_tx_id: u32,
    waiting_to_close: Vec<proto::HandleId>,
    waiting_to_close_waker: Waker,
}

impl ClientInner {
    /// Serialize and enqueue a new transaction, including header and transaction ID.
    fn request<S: fidl_message::Body>(&mut self, ordinal: u64, request: S, responder: Responder) {
        let tx_id = self.next_tx_id;

        let header = TransactionHeader::new(tx_id, ordinal, fidl_message::DynamicFlags::FLEXIBLE);
        let msg = fidl_message::encode_message(header, request).expect("Could not encode request!");
        self.next_tx_id += 1;
        assert!(
            self.transactions.insert(tx_id.try_into().unwrap(), responder).is_none(),
            "Allocated same tx id twice!"
        );
        self.transport.push_msg(msg.into());
    }

    /// Polls the underlying transport to ensure any incoming or outgoing
    /// messages are processed as far as possible. Errors if the transport has failed.
    fn try_poll_transport(
        &mut self,
        ctx: &mut Context<'_>,
    ) -> Poll<Result<Infallible, InnerError>> {
        if !self.waiting_to_close.is_empty() {
            let handles = std::mem::replace(&mut self.waiting_to_close, Vec::new());
            self.request(
                ordinals::CLOSE,
                proto::FDomainCloseRequest { handles },
                Responder::Ignore,
            );
        }

        self.waiting_to_close_waker = ctx.waker().clone();

        loop {
            if let Poll::Ready(e) = self.transport.poll_send_messages(ctx) {
                for sender in self.socket_read_subscriptions.values_mut() {
                    let _ = sender.unbounded_send(Err(e.clone().into()));
                }
                for (_, state) in self.channel_read_states.drain() {
                    state.wakers.into_iter().for_each(Waker::wake);
                }
                self.socket_read_subscriptions.clear();
                return Poll::Ready(Err(e));
            }
            let Poll::Ready(Some(result)) = self.transport.poll_next(ctx) else {
                return Poll::Pending;
            };
            let data = result?;
            let (header, data) = match fidl_message::decode_transaction_header(&data) {
                Ok(x) => x,
                Err(e) => {
                    self.transport = Transport::Error(InnerError::Protocol(e));
                    continue;
                }
            };

            let Some(tx_id) = NonZeroU32::new(header.tx_id) else {
                if let Err(e) = self.process_event(header, data) {
                    self.transport = Transport::Error(e);
                }
                continue;
            };

            let tx = self.transactions.remove(&tx_id).ok_or(::fidl::Error::InvalidResponseTxid)?;
            let responder_status = match tx.handle(self, Ok((header, data))) {
                Ok(x) => x,
                Err(e) => {
                    self.transport = Transport::Error(InnerError::Protocol(e));
                    continue;
                }
            };
            if let ResponderStatus::WriteErrorOccurred(handle) = responder_status {
                self.request(
                    ordinals::ACKNOWLEDGE_WRITE_ERROR,
                    proto::FDomainAcknowledgeWriteErrorRequest { handle },
                    Responder::Ignore,
                );
            }
        }
    }

    /// Process an incoming message that arose from an event rather than a transaction reply.
    fn process_event(&mut self, header: TransactionHeader, data: &[u8]) -> Result<(), InnerError> {
        match header.ordinal {
            ordinals::ON_SOCKET_STREAMING_DATA => {
                let msg = fidl_message::decode_message::<proto::SocketOnSocketStreamingDataRequest>(
                    header, data,
                )?;
                if let Entry::Occupied(mut o) = self.socket_read_subscriptions.entry(msg.handle) {
                    match msg.socket_message {
                        proto::SocketMessage::Data(data) => {
                            if o.get_mut().unbounded_send(Ok(data)).is_err() {
                                let _ = o.remove();
                                self.request(
                                    ordinals::READ_SOCKET_STREAMING_STOP,
                                    proto::SocketReadSocketStreamingStopRequest {
                                        handle: msg.handle,
                                    },
                                    Responder::Ignore,
                                );
                            }
                            Ok(())
                        }
                        proto::SocketMessage::Stopped(proto::AioStopped { error }) => {
                            let o = o.remove();
                            if let Some(error) = error {
                                let _ = o.unbounded_send(Err(Error::FDomain(*error)));
                            }
                            Ok(())
                        }
                        _ => Err(InnerError::ProtocolStreamEventIncompatible),
                    }
                } else {
                    Ok(())
                }
            }
            ordinals::ON_CHANNEL_STREAMING_DATA => {
                let msg = fidl_message::decode_message::<
                    proto::ChannelOnChannelStreamingDataRequest,
                >(header, data)?;
                let o = self.channel_read_states.entry(msg.handle).or_insert_with(|| {
                    ChannelReadState {
                        wakers: Vec::new(),
                        queued: VecDeque::new(),
                        is_streaming: false,
                        read_request_pending: false,
                    }
                });
                match msg.channel_sent {
                    proto::ChannelSent::Message(data) => {
                        o.handle_incoming_message(Ok(data));
                        Ok(())
                    }
                    proto::ChannelSent::Stopped(proto::AioStopped { error }) => {
                        if let Some(error) = error {
                            o.handle_incoming_message(Err(Error::FDomain(*error)));
                        }
                        o.is_streaming = false;
                        Ok(())
                    }
                    _ => Err(InnerError::ProtocolStreamEventIncompatible),
                }
            }
            _ => Err(::fidl::Error::UnknownOrdinal {
                ordinal: header.ordinal,
                protocol_name:
                    <proto::FDomainMarker as ::fidl::endpoints::ProtocolMarker>::DEBUG_NAME,
            }
            .into()),
        }
    }

    /// Polls the underlying transport to ensure any incoming or outgoing
    /// messages are processed as far as possible. If a failure occurs, puts the
    /// transport into an error state and fails all pending transactions.
    fn poll_transport(&mut self, ctx: &mut Context<'_>) {
        if let Poll::Ready(Err(e)) = self.try_poll_transport(ctx) {
            for (_, v) in std::mem::take(&mut self.transactions) {
                let _ = v.handle(self, Err(e.clone()));
            }
        }
    }

    /// Handles the response to a `ChannelRead` protocol message.
    pub(crate) fn handle_channel_read_response(
        &mut self,
        msg: Result<proto::ChannelMessage, Error>,
        id: proto::HandleId,
    ) {
        let state = self.channel_read_states.entry(id).or_insert_with(|| ChannelReadState {
            wakers: Vec::new(),
            queued: VecDeque::new(),
            is_streaming: false,
            read_request_pending: false,
        });
        state.handle_incoming_message(msg);
        state.read_request_pending = false;
    }
}

/// Represents a connection to an FDomain.
///
/// The client is constructed by passing it a transport object which represents
/// the raw connection to the remote FDomain. The `Client` wrapper then allows
/// us to construct and use handles which behave similarly to their counterparts
/// on a Fuchsia device.
pub struct Client(Mutex<ClientInner>);

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Client").field(&"...").finish()
    }
}

impl Client {
    /// Create a new FDomain client. The `transport` argument should contain the
    /// established connection to the target, ready to communicate the FDomain
    /// protocol.
    ///
    /// The second return item is a future that must be polled to keep
    /// transactions running.
    pub fn new(
        transport: impl FDomainTransport + 'static,
    ) -> (Arc<Self>, impl Future<Output = ()> + Send + 'static) {
        let ret = Arc::new(Client(Mutex::new(ClientInner {
            transport: Transport::Transport(Box::pin(transport), VecDeque::new(), Vec::new()),
            transactions: HashMap::new(),
            socket_read_subscriptions: HashMap::new(),
            channel_read_states: HashMap::new(),
            next_tx_id: 1,
            waiting_to_close: Vec::new(),
            waiting_to_close_waker: futures::task::noop_waker(),
        })));

        let client_weak = Arc::downgrade(&ret);
        let fut = futures::future::poll_fn(move |ctx| {
            let Some(client) = client_weak.upgrade() else {
                return Poll::Ready(());
            };

            client.0.lock().unwrap().poll_transport(ctx);
            Poll::Pending
        });

        (ret, fut)
    }

    /// Get the namespace for the connected FDomain. Calling this more than once is an error.
    pub async fn namespace(self: &Arc<Self>) -> Result<Channel, Error> {
        let new_handle = self.new_hid();
        self.transaction(
            ordinals::NAMESPACE,
            proto::FDomainNamespaceRequest { new_handle },
            Responder::Namespace,
        )
        .await?;
        Ok(Channel(Handle { id: new_handle.id, client: Arc::downgrade(self) }))
    }

    /// Create a new channel in the connected FDomain.
    pub fn create_channel(self: &Arc<Self>) -> (Channel, Channel) {
        let id_a = self.new_hid();
        let id_b = self.new_hid();
        let fut = self.transaction(
            ordinals::CREATE_CHANNEL,
            proto::ChannelCreateChannelRequest { handles: [id_a, id_b] },
            Responder::CreateChannel,
        );

        fuchsia_async::Task::spawn(async move {
            if let Err(e) = fut.await {
                log::debug!("FDomain channel creation failed: {e}");
            }
        })
        .detach();

        (
            Channel(Handle { id: id_a.id, client: Arc::downgrade(self) }),
            Channel(Handle { id: id_b.id, client: Arc::downgrade(self) }),
        )
    }

    /// Creates client and server endpoints connected to by a channel.
    pub fn create_endpoints<F: crate::fidl::ProtocolMarker>(
        self: &Arc<Self>,
    ) -> (crate::fidl::ClientEnd<F>, crate::fidl::ServerEnd<F>) {
        let (client, server) = self.create_channel();
        let client_end = crate::fidl::ClientEnd::<F>::new(client);
        let server_end = crate::fidl::ServerEnd::new(server);
        (client_end, server_end)
    }

    /// Creates a client proxy and a server endpoint connected by a channel.
    pub fn create_proxy<F: crate::fidl::ProtocolMarker>(
        self: &Arc<Self>,
    ) -> (F::Proxy, crate::fidl::ServerEnd<F>) {
        let (client_end, server_end) = self.create_endpoints::<F>();
        (client_end.into_proxy(), server_end)
    }

    /// Creates a client proxy and a server request stream connected by a channel.
    pub fn create_proxy_and_stream<F: crate::fidl::ProtocolMarker>(
        self: &Arc<Self>,
    ) -> (F::Proxy, F::RequestStream) {
        let (client_end, server_end) = self.create_endpoints::<F>();
        (client_end.into_proxy(), server_end.into_stream())
    }

    /// Create a new socket in the connected FDomain.
    fn create_socket(self: &Arc<Self>, options: proto::SocketType) -> (Socket, Socket) {
        let id_a = self.new_hid();
        let id_b = self.new_hid();
        let fut = self.transaction(
            ordinals::CREATE_SOCKET,
            proto::SocketCreateSocketRequest { handles: [id_a, id_b], options },
            Responder::CreateSocket,
        );

        fuchsia_async::Task::spawn(async move {
            if let Err(e) = fut.await {
                log::debug!("FDomain socket creation failed: {e}");
            }
        })
        .detach();

        (
            Socket(Handle { id: id_a.id, client: Arc::downgrade(self) }),
            Socket(Handle { id: id_b.id, client: Arc::downgrade(self) }),
        )
    }

    /// Create a new streaming socket in the connected FDomain.
    pub fn create_stream_socket(self: &Arc<Self>) -> (Socket, Socket) {
        self.create_socket(proto::SocketType::Stream)
    }

    /// Create a new datagram socket in the connected FDomain.
    pub fn create_datagram_socket(self: &Arc<Self>) -> (Socket, Socket) {
        self.create_socket(proto::SocketType::Datagram)
    }

    /// Create a new event pair in the connected FDomain.
    pub fn create_event_pair(self: &Arc<Self>) -> (EventPair, EventPair) {
        let id_a = self.new_hid();
        let id_b = self.new_hid();
        let fut = self.transaction(
            ordinals::CREATE_EVENT_PAIR,
            proto::EventPairCreateEventPairRequest { handles: [id_a, id_b] },
            Responder::CreateEventPair,
        );

        fuchsia_async::Task::spawn(async move {
            if let Err(e) = fut.await {
                log::debug!("FDomain event pair creation failed: {e}");
            }
        })
        .detach();

        (
            EventPair(Handle { id: id_a.id, client: Arc::downgrade(self) }),
            EventPair(Handle { id: id_b.id, client: Arc::downgrade(self) }),
        )
    }

    /// Create a new event handle in the connected FDomain.
    pub fn create_event(self: &Arc<Self>) -> Event {
        let id = self.new_hid();
        let fut = self.transaction(
            ordinals::CREATE_EVENT,
            proto::EventCreateEventRequest { handle: id },
            Responder::CreateEvent,
        );

        fuchsia_async::Task::spawn(async move {
            if let Err(e) = fut.await {
                log::debug!("FDomain event creation failed: {e}");
            }
        })
        .detach();

        Event(Handle { id: id.id, client: Arc::downgrade(self) })
    }

    /// Allocate a new HID, which should be suitable for use with the connected FDomain.
    pub(crate) fn new_hid(&self) -> proto::NewHandleId {
        // TODO: On the target side we have to keep a table of these which means
        // we can automatically detect collisions in the random value. On the
        // client side we'd have to add a whole data structure just for that
        // purpose. Should we?
        proto::NewHandleId { id: rand::random::<u32>() >> 1 }
    }

    /// Create a future which sends a FIDL message to the connected FDomain and
    /// waits for a response.
    ///
    /// Calling this method queues the transaction synchronously. Awaiting is
    /// only necessary to wait for the response.
    pub(crate) fn transaction<S: fidl_message::Body, R: 'static>(
        self: &Arc<Self>,
        ordinal: u64,
        request: S,
        f: impl Fn(OneshotSender<Result<R, Error>>) -> Responder,
    ) -> impl Future<Output = Result<R, Error>> + 'static {
        let mut inner = self.0.lock().unwrap();

        let (sender, receiver) = futures::channel::oneshot::channel();
        inner.request(ordinal, request, f(sender));
        receiver.map(|x| x.expect("Oneshot went away without reply!"))
    }

    /// Start getting streaming events for socket reads.
    pub(crate) fn start_socket_streaming(
        &self,
        id: proto::HandleId,
        output: UnboundedSender<Result<Vec<u8>, Error>>,
    ) -> Result<(), Error> {
        let mut inner = self.0.lock().unwrap();
        inner.socket_read_subscriptions.insert(id, output);
        inner.request(
            ordinals::READ_SOCKET_STREAMING_START,
            proto::SocketReadSocketStreamingStartRequest { handle: id },
            Responder::Ignore,
        );
        Ok(())
    }

    /// Stop getting streaming events for socket reads. Doesn't return errors
    /// because it's exclusively called in destructors where we have nothing to
    /// do with them.
    pub(crate) fn stop_socket_streaming(&self, id: proto::HandleId) {
        let mut inner = self.0.lock().unwrap();
        if inner.socket_read_subscriptions.remove(&id).is_some() {
            // TODO: Log?
            let _ = inner.request(
                ordinals::READ_SOCKET_STREAMING_STOP,
                proto::SocketReadSocketStreamingStopRequest { handle: id },
                Responder::Ignore,
            );
        }
    }

    /// Start getting streaming events for socket reads.
    pub(crate) fn start_channel_streaming(&self, id: proto::HandleId) -> Result<(), Error> {
        let mut inner = self.0.lock().unwrap();
        let state = inner.channel_read_states.entry(id).or_insert_with(|| ChannelReadState {
            wakers: Vec::new(),
            queued: VecDeque::new(),
            is_streaming: false,
            read_request_pending: false,
        });

        assert!(!state.is_streaming, "Initiated streaming twice!");
        state.is_streaming = true;

        inner.request(
            ordinals::READ_CHANNEL_STREAMING_START,
            proto::ChannelReadChannelStreamingStartRequest { handle: id },
            Responder::Ignore,
        );

        Ok(())
    }

    /// Stop getting streaming events for socket reads. Doesn't return errors
    /// because it's exclusively called in destructors where we have nothing to
    /// do with them.
    pub(crate) fn stop_channel_streaming(&self, id: proto::HandleId) {
        let mut inner = self.0.lock().unwrap();
        if let Some(state) = inner.channel_read_states.get_mut(&id) {
            if state.is_streaming {
                state.is_streaming = false;
                // TODO: Log?
                let _ = inner.request(
                    ordinals::READ_CHANNEL_STREAMING_STOP,
                    proto::ChannelReadChannelStreamingStopRequest { handle: id },
                    Responder::Ignore,
                );
            }
        }
    }

    /// Execute a read from a channel.
    pub(crate) fn poll_channel(
        &self,
        id: proto::HandleId,
        ctx: &mut Context<'_>,
        for_stream: bool,
    ) -> Poll<Option<Result<proto::ChannelMessage, Error>>> {
        let mut inner = self.0.lock().unwrap();
        if let Some(error) = inner.transport.error() {
            return Poll::Ready(Some(Err(error.into())));
        }

        let state = inner.channel_read_states.entry(id).or_insert_with(|| ChannelReadState {
            wakers: Vec::new(),
            queued: VecDeque::new(),
            is_streaming: false,
            read_request_pending: false,
        });

        if let Some(got) = state.queued.pop_front() {
            return Poll::Ready(Some(got));
        } else if for_stream && !state.is_streaming {
            return Poll::Ready(None);
        } else if !state.wakers.iter().any(|x| ctx.waker().will_wake(x)) {
            state.wakers.push(ctx.waker().clone());
        }

        if !state.read_request_pending && !state.is_streaming {
            inner.request(
                ordinals::READ_CHANNEL,
                proto::ChannelReadChannelRequest { handle: id },
                Responder::ReadChannel(id),
            );
        }

        Poll::Pending
    }

    /// Check whether this channel is streaming
    pub(crate) fn channel_is_streaming(&self, id: proto::HandleId) -> bool {
        let inner = self.0.lock().unwrap();
        let Some(state) = inner.channel_read_states.get(&id) else {
            return false;
        };
        state.is_streaming
    }

    /// Check that all the given handles are safe to transfer through a channel
    /// e.g. that there's no chance of in-flight reads getting dropped.
    pub(crate) fn clear_handles_for_transfer(&self, handles: &proto::Handles) {
        let inner = self.0.lock().unwrap();
        match handles {
            proto::Handles::Handles(handles) => {
                for handle in handles {
                    assert!(
                        !inner.channel_read_states.contains_key(handle),
                        "Tried to transfer handle after reading"
                    );
                }
            }
            proto::Handles::Dispositions(dispositions) => {
                for disposition in dispositions {
                    match &disposition.handle {
                        proto::HandleOp::Move_(handle) => assert!(
                            !inner.channel_read_states.contains_key(handle),
                            "Tried to transfer handle after reading"
                        ),
                        // Pretty sure this should be fine regardless of read state.
                        proto::HandleOp::Duplicate(_) => (),
                    }
                }
            }
        }
    }
}
