// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Channel, Handle};
use futures::{Stream, TryStream};
use std::marker::PhantomData;

///
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

    /// Returns true if the proxy has received the `PEER_CLOSED` signal.
    fn is_closed(&self) -> bool;

    /// Returns a future that completes when the proxy receives the
    /// `PEER_CLOSED` signal.
    fn on_closed(&self) -> crate::OnFDomainSignals;
}

/// A stream of requests coming into a FIDL server over a channel.
pub trait RequestStream: Sized + Send + Stream + TryStream<Error = crate::Error> + Unpin {
    /// The protocol which this `RequestStream` serves.
    type Protocol: ProtocolMarker<RequestStream = Self>;

    /// The control handle for this `RequestStream`.
    type ControlHandle: ControlHandle;

    /// Returns a copy of the `ControlHandle` for the given stream.
    /// This handle can be used to send events or shut down the request stream.
    fn control_handle(&self) -> Self::ControlHandle;

    /// Create a request stream from the given channel.
    fn from_channel(inner: Channel) -> Self;

    /// Convert this FIDL request stream into a request stream of another FIDL protocol.
    fn cast_stream<F: RequestStream>(self) -> F;
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

/// The Request type associated with a Marker.
pub type Request<Marker> = <<Marker as ProtocolMarker>::RequestStream as futures::TryStream>::Ok;

/// The `Client` end of a FIDL connection.
#[derive(Eq, PartialEq, Ord, PartialOrd, Hash)]
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
#[derive(Eq, PartialEq, Ord, PartialOrd, Hash)]
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
