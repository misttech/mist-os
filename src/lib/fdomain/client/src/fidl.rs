// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    AnyHandle, AsHandleRef, Channel, ChannelMessage, ChannelMessageStream, ChannelWriter, Error,
    Handle, HandleInfo,
};
use fidl_fuchsia_fdomain as proto;
use futures::{Stream, StreamExt, TryStream};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::task::Poll;

pub trait FDomainFlexibleIntoResult<T> {
    fn into_result_fdomain<P: ProtocolMarker>(
        self,
        method_name: &'static str,
    ) -> Result<T, fidl::Error>;
}

impl<T> FDomainFlexibleIntoResult<T> for fidl::encoding::Flexible<T> {
    fn into_result_fdomain<P: ProtocolMarker>(
        self,
        method_name: &'static str,
    ) -> Result<T, fidl::Error> {
        match self {
            fidl::encoding::Flexible::Ok(ok) => Ok(ok),
            fidl::encoding::Flexible::FrameworkErr(fidl::encoding::FrameworkErr::UnknownMethod) => {
                Err(fidl::Error::UnsupportedMethod { method_name, protocol_name: P::DEBUG_NAME })
            }
        }
    }
}

impl<T, E> FDomainFlexibleIntoResult<Result<T, E>> for fidl::encoding::FlexibleResult<T, E> {
    fn into_result_fdomain<P: ProtocolMarker>(
        self,
        method_name: &'static str,
    ) -> Result<Result<T, E>, fidl::Error> {
        match self {
            fidl::encoding::FlexibleResult::Ok(ok) => Ok(Ok(ok)),
            fidl::encoding::FlexibleResult::DomainErr(err) => Ok(Err(err)),
            fidl::encoding::FlexibleResult::FrameworkErr(
                fidl::encoding::FrameworkErr::UnknownMethod,
            ) => Err(fidl::Error::UnsupportedMethod { method_name, protocol_name: P::DEBUG_NAME }),
        }
    }
}

#[derive(Debug)]
pub struct FDomainProxyChannel(Mutex<ChannelMessageStream>, ChannelWriter);

impl FDomainProxyChannel {
    pub fn on_closed(&self) -> crate::OnFDomainSignals {
        self.1.as_channel().on_closed()
    }

    pub fn read_etc(
        &self,
        ctx: &mut std::task::Context<'_>,
        bytes: &mut Vec<u8>,
        handles: &mut Vec<HandleInfo>,
    ) -> Poll<Result<(), Option<crate::Error>>> {
        let Some(got) = std::task::ready!(self.0.lock().unwrap().poll_next_unpin(ctx)) else {
            return Poll::Ready(Err(Some(Error::ClientLost)));
        };

        match got {
            Ok(got) => {
                *bytes = got.bytes;
                *handles = got.handles;
                Poll::Ready(Ok(()))
            }
            Err(Error::FDomain(proto::Error::TargetError(i)))
                if i == fidl::Status::PEER_CLOSED.into_raw() =>
            {
                Poll::Ready(Err(None))
            }
            Err(e) => Poll::Ready(Err(Some(e))),
        }
    }
}

impl ::fidl::encoding::ProxyChannelBox<FDomainResourceDialect> for FDomainProxyChannel {
    fn recv_etc_from(
        &self,
        ctx: &mut std::task::Context<'_>,
        buf: &mut ChannelMessage,
    ) -> Poll<Result<(), Option<Error>>> {
        let Some(got) = std::task::ready!(self.0.lock().unwrap().poll_next_unpin(ctx)) else {
            return Poll::Ready(Err(Some(Error::ClientLost)));
        };

        match got {
            Ok(got) => {
                *buf = got;
                Poll::Ready(Ok(()))
            }
            Err(Error::FDomain(proto::Error::TargetError(i)))
                if i == fidl::Status::PEER_CLOSED.into_raw() =>
            {
                Poll::Ready(Err(None))
            }
            Err(e) => Poll::Ready(Err(Some(e))),
        }
    }

    fn write_etc(&self, bytes: &[u8], handles: &mut [HandleInfo]) -> Result<(), Option<Error>> {
        let mut handle_ops = Vec::new();
        for handle in handles {
            handle_ops.push(crate::channel::HandleOp::Move(
                std::mem::replace(&mut handle.handle, AnyHandle::invalid()).into(),
                handle.rights,
            ));
        }
        let _ = self.1.write_etc(bytes, handle_ops);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.0.lock().unwrap().is_closed()
    }

    fn unbox(self) -> Channel {
        // We drop the queue of pending data here. The FIDL client has some
        // invariants it maintains that should make it very unlikely that
        // there's anything to read from it.
        let (channel, _) = self.0.into_inner().unwrap().rejoin(self.1);
        channel
    }

    fn as_channel(&self) -> &Channel {
        self.1.as_channel()
    }
}

#[derive(Debug, Copy, Clone, Default)]
pub struct FDomainResourceDialect;
impl ::fidl::encoding::ResourceDialect for FDomainResourceDialect {
    type Handle = Handle;
    type MessageBufEtc = ChannelMessage;
    type ProxyChannel = Channel;

    #[inline]
    fn with_tls_buf<R>(f: impl FnOnce(&mut ::fidl::encoding::TlsBuf<Self>) -> R) -> R {
        thread_local!(static TLS_BUF: RefCell<::fidl::encoding::TlsBuf<FDomainResourceDialect>> =
            RefCell::new(::fidl::encoding::TlsBuf::default()));
        TLS_BUF.with(|buf| f(&mut buf.borrow_mut()))
    }
}

impl ::fidl::encoding::MessageBufFor<FDomainResourceDialect> for ChannelMessage {
    fn new() -> ChannelMessage {
        ChannelMessage { bytes: Vec::new(), handles: Vec::new() }
    }

    fn split_mut(&mut self) -> (&mut Vec<u8>, &mut Vec<HandleInfo>) {
        (&mut self.bytes, &mut self.handles)
    }
}

impl Into<::fidl::TransportError> for Error {
    fn into(self) -> ::fidl::TransportError {
        match self {
            Error::FDomain(proto::Error::TargetError(i)) => {
                ::fidl::TransportError::Status(fidl::Status::from_raw(i))
            }
            Error::SocketWrite(proto::WriteSocketError {
                error: proto::Error::TargetError(i),
                ..
            }) => ::fidl::TransportError::Status(fidl::Status::from_raw(i)),
            Error::ChannelWrite(proto::WriteChannelError::Error(proto::Error::TargetError(i))) => {
                ::fidl::TransportError::Status(fidl::Status::from_raw(i))
            }
            Error::ChannelWrite(proto::WriteChannelError::OpErrors(ops)) => {
                let Some(op) = ops.into_iter().find_map(|x| x) else {
                    let err = Box::<dyn std::error::Error + Send + Sync>::from(
                        "Channel write handle operation reported failure with no status!"
                            .to_owned(),
                    );
                    return ::fidl::TransportError::Other(err.into());
                };
                let op = *op;
                Error::FDomain(op).into()
            }
            other => ::fidl::TransportError::Other(std::sync::Arc::new(other)),
        }
    }
}

impl ::fidl::encoding::ProxyChannelFor<FDomainResourceDialect> for Channel {
    type Boxed = FDomainProxyChannel;
    type Error = Error;
    type HandleDisposition = HandleInfo;

    fn boxed(self) -> Self::Boxed {
        let (a, b) = self.stream().unwrap();
        FDomainProxyChannel(Mutex::new(a), b)
    }

    fn write_etc(&self, bytes: &[u8], handles: &mut [HandleInfo]) -> Result<(), Option<Error>> {
        let mut handle_ops = Vec::new();
        for handle in handles {
            handle_ops.push(crate::channel::HandleOp::Move(
                std::mem::replace(&mut handle.handle, AnyHandle::invalid()).into(),
                handle.rights,
            ));
        }
        let _ = self.write_etc(bytes, handle_ops);
        Ok(())
    }
}

impl ::fidl::epitaph::ChannelLike for Channel {
    fn write_epitaph(&self, bytes: &[u8]) -> Result<(), ::fidl::TransportError> {
        let _ = self.write_etc(bytes, vec![]);
        Ok(())
    }
}

impl ::fidl::encoding::HandleFor<FDomainResourceDialect> for Handle {
    // This has to be static, so we can't encode a duplicate operation here
    // anyway. So use HandleInfo.
    type HandleInfo = HandleInfo;

    fn invalid() -> Self {
        Handle::invalid()
    }

    fn is_invalid(&self) -> bool {
        self.client.upgrade().is_none()
    }
}

impl ::fidl::encoding::HandleDispositionFor<FDomainResourceDialect> for HandleInfo {
    fn from_handle(handle: Handle, object_type: fidl::ObjectType, rights: fidl::Rights) -> Self {
        HandleInfo { handle: AnyHandle::from_handle(handle, object_type), rights }
    }
}

impl ::fidl::encoding::HandleInfoFor<FDomainResourceDialect> for HandleInfo {
    fn consume(
        &mut self,
        expected_object_type: fidl::ObjectType,
        expected_rights: fidl::Rights,
    ) -> Result<Handle, ::fidl::Error> {
        let handle_info = std::mem::replace(
            self,
            HandleInfo {
                handle: crate::AnyHandle::Unknown(Handle::invalid(), fidl::ObjectType::NONE),
                rights: fidl::Rights::empty(),
            },
        );
        let received_object_type = handle_info.handle.object_type();
        if expected_object_type != fidl::ObjectType::NONE
            && received_object_type != fidl::ObjectType::NONE
            && expected_object_type != received_object_type
        {
            return Err(fidl::Error::IncorrectHandleSubtype {
                // TODO: Find a way to put something better in here, either by
                // expanding what FIDL can return or casting the protocol values
                // to something FIDL can read.
                expected: fidl::ObjectType::NONE,
                received: fidl::ObjectType::NONE,
            });
        }

        let received_rights = handle_info.rights;
        if expected_rights != fidl::Rights::SAME_RIGHTS
            && received_rights != fidl::Rights::SAME_RIGHTS
            && expected_rights != received_rights
        {
            if !received_rights.contains(expected_rights) {
                return Err(fidl::Error::MissingExpectedHandleRights {
                    // TODO: As above, report something better here.
                    missing_rights: fidl::Rights::empty(),
                });
            }

            // TODO: The normal FIDL bindings call zx_handle_replace here to
            // forcibly downgrade the handle rights. That's a whole IO operation
            // for us so we won't bother, but maybe we should do something else?
        }
        Ok(handle_info.handle.into())
    }

    fn drop_in_place(&mut self) {
        *self = HandleInfo {
            handle: crate::AnyHandle::Unknown(Handle::invalid(), fidl::ObjectType::NONE),
            rights: fidl::Rights::empty(),
        };
    }
}

impl ::fidl::encoding::EncodableAsHandle for crate::Event {
    type Dialect = FDomainResourceDialect;
}

impl ::fidl::encoding::EncodableAsHandle for crate::EventPair {
    type Dialect = FDomainResourceDialect;
}

impl ::fidl::encoding::EncodableAsHandle for crate::Socket {
    type Dialect = FDomainResourceDialect;
}

impl ::fidl::encoding::EncodableAsHandle for crate::Channel {
    type Dialect = FDomainResourceDialect;
}

impl ::fidl::encoding::EncodableAsHandle for crate::Handle {
    type Dialect = FDomainResourceDialect;
}

impl<T: ProtocolMarker> ::fidl::encoding::EncodableAsHandle for ClientEnd<T> {
    type Dialect = FDomainResourceDialect;
}

impl<T: ProtocolMarker> ::fidl::encoding::EncodableAsHandle for ServerEnd<T> {
    type Dialect = FDomainResourceDialect;
}

/// Implementations of this trait can be used to manufacture instances of a FIDL
/// protocol and get metadata about a particular protocol.
pub trait ProtocolMarker: Sized + Send + Sync + 'static {
    /// The type of the structure against which FIDL requests are made.
    /// Queries made against the proxy are sent to the paired `ServerEnd`.
    type Proxy: Proxy<Protocol = Self>;

    /// The type of the stream of requests coming into a server.
    type RequestStream: RequestStream<Protocol = Self>;

    /// The name of the protocol suitable for debug purposes.
    ///
    /// For discoverable protocols, this should be identical to
    /// `<Self as DiscoverableProtocolMarker>::PROTOCOL_NAME`.
    const DEBUG_NAME: &'static str;
}

/// A marker for a particular FIDL protocol that is also discoverable.
///
/// Discoverable protocols may be referred to by a string name, and can be
/// conveniently exported in a service directory via an entry of that name.
///
/// If you get an error about this trait not being implemented, you probably
/// need to add the `@discoverable` attribute to the FIDL protocol, like this:
///
/// ```fidl
/// @discoverable
/// protocol MyProtocol { ... };
/// ```
pub trait DiscoverableProtocolMarker: ProtocolMarker {
    /// The name of the protocol (to be used for service lookup and discovery).
    const PROTOCOL_NAME: &'static str = <Self as ProtocolMarker>::DEBUG_NAME;
}

/// A type which allows querying a remote FIDL server over a channel.
pub trait Proxy: Sized + Send + Sync {
    /// The protocol which this `Proxy` controls.
    type Protocol: ProtocolMarker<Proxy = Self>;

    /// Create a proxy over the given channel.
    fn from_channel(inner: Channel) -> Self;

    /// Attempt to convert the proxy back into a channel.
    ///
    /// This will only succeed if there are no active clones of this proxy
    /// and no currently-alive `EventStream` or response futures that came from
    /// this proxy.
    fn into_channel(self) -> Result<Channel, Self>;

    /// Get a reference to the proxy's underlying channel.
    ///
    /// This should only be used for non-effectful operations. Reading or
    /// writing to the channel is unsafe because the proxy assumes it has
    /// exclusive control over these operations.
    fn as_channel(&self) -> &Channel;

    /// Get the client supporting this proxy.
    fn client(&self) -> Result<Arc<crate::Client>, Error> {
        self.as_channel().client()
    }
}

/// A stream of requests coming into a FIDL server over a channel.
pub trait RequestStream: Sized + Send + Stream + TryStream<Error = fidl::Error> + Unpin {
    /// The protocol which this `RequestStream` serves.
    type Protocol: ProtocolMarker<RequestStream = Self>;

    /// The control handle for this `RequestStream`.
    type ControlHandle: ControlHandle;

    /// Returns a copy of the `ControlHandle` for the given stream.
    /// This handle can be used to send events or shut down the request stream.
    fn control_handle(&self) -> Self::ControlHandle;

    /// Create a request stream from the given channel.
    fn from_channel(inner: Channel) -> Self;

    /// Convert to a `ServeInner`
    fn into_inner(self) -> (std::sync::Arc<fidl::ServeInner<FDomainResourceDialect>>, bool);

    /// Convert from a `ServeInner`
    fn from_inner(
        inner: std::sync::Arc<fidl::ServeInner<FDomainResourceDialect>>,
        is_terminated: bool,
    ) -> Self;
}

/// A type associated with a `RequestStream` that can be used to send FIDL
/// events or to shut down the request stream.
pub trait ControlHandle {
    /// Set the server to shutdown. The underlying channel is only closed the
    /// next time the stream is polled.
    fn shutdown(&self);

    /// Returns true if the server has received the `PEER_CLOSED` signal.
    fn is_closed(&self) -> bool;

    /// Returns a future that completes when the server receives the
    /// `PEER_CLOSED` signal.
    fn on_closed(&self) -> crate::OnFDomainSignals;
}

/// A type associated with a particular two-way FIDL method, used by servers to
/// send a response to the client.
pub trait Responder {
    /// The control handle for this protocol.
    type ControlHandle: ControlHandle;

    /// Returns the `ControlHandle` for this protocol.
    fn control_handle(&self) -> &Self::ControlHandle;

    /// Drops the responder without setting the channel to shutdown.
    ///
    /// This method shouldn't normally be used. Instead, send a response to
    /// prevent the channel from shutting down.
    fn drop_without_shutdown(self);
}

/// The Request type associated with a Marker.
pub type Request<Marker> = <<Marker as ProtocolMarker>::RequestStream as futures::TryStream>::Ok;

/// The `Client` end of a FIDL connection.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ClientEnd<T: ProtocolMarker> {
    inner: Channel,
    phantom: PhantomData<T>,
}

impl<T: ProtocolMarker> ClientEnd<T> {
    /// Create a new client from the provided channel.
    pub fn new(inner: Channel) -> Self {
        ClientEnd { inner, phantom: PhantomData }
    }

    /// Get a reference to the underlying channel
    pub fn channel(&self) -> &Channel {
        &self.inner
    }

    /// Extract the underlying channel.
    pub fn into_channel(self) -> Channel {
        self.inner
    }
}

impl<'c, T: ProtocolMarker> ClientEnd<T> {
    /// Convert the `ClientEnd` into a `Proxy` through which FIDL calls may be made.
    pub fn into_proxy(self) -> Result<T::Proxy, crate::Error> {
        Ok(T::Proxy::from_channel(self.inner))
    }
}

impl<T: ProtocolMarker> From<ClientEnd<T>> for Handle {
    fn from(client: ClientEnd<T>) -> Handle {
        client.into_channel().into()
    }
}

impl<T: ProtocolMarker> From<Handle> for ClientEnd<T> {
    fn from(handle: Handle) -> Self {
        ClientEnd { inner: handle.into(), phantom: PhantomData }
    }
}

impl<T: ProtocolMarker> From<Channel> for ClientEnd<T> {
    fn from(chan: Channel) -> Self {
        ClientEnd { inner: chan, phantom: PhantomData }
    }
}

/// The `Server` end of a FIDL connection.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ServerEnd<T: ProtocolMarker> {
    inner: Channel,
    phantom: PhantomData<T>,
}

impl<T: ProtocolMarker> ServerEnd<T> {
    /// Create a new `ServerEnd` from the provided channel.
    pub fn new(inner: Channel) -> ServerEnd<T> {
        ServerEnd { inner, phantom: PhantomData }
    }

    /// Get a reference to the underlying channel
    pub fn channel(&self) -> &Channel {
        &self.inner
    }

    /// Extract the inner channel.
    pub fn into_channel(self) -> Channel {
        self.inner
    }

    /// Create a stream of requests off of the channel.
    pub fn into_stream(self) -> Result<T::RequestStream, crate::Error>
    where
        T: ProtocolMarker,
    {
        Ok(T::RequestStream::from_channel(self.inner))
    }

    /// Create a stream of requests and an event-sending handle
    /// from the channel.
    pub fn into_stream_and_control_handle(
        self,
    ) -> Result<(T::RequestStream, <T::RequestStream as RequestStream>::ControlHandle), crate::Error>
    where
        T: ProtocolMarker,
    {
        let stream = self.into_stream()?;
        let control_handle = stream.control_handle();
        Ok((stream, control_handle))
    }
}

impl<T: ProtocolMarker> From<ServerEnd<T>> for Handle {
    fn from(server: ServerEnd<T>) -> Handle {
        server.into_channel().into()
    }
}

impl<T: ProtocolMarker> From<Handle> for ServerEnd<T> {
    fn from(handle: Handle) -> Self {
        ServerEnd { inner: handle.into(), phantom: PhantomData }
    }
}

impl<T: ProtocolMarker> From<Channel> for ServerEnd<T> {
    fn from(chan: Channel) -> Self {
        ServerEnd { inner: chan, phantom: PhantomData }
    }
}
