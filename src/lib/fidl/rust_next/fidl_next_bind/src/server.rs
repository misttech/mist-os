// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

use fidl_next_codec::{Encode, EncodeError};
use fidl_next_protocol::{self as protocol, ProtocolError, SendFuture, Transport};

use super::{Method, ServerEnd};

/// A storngly typed server sender.
pub struct ServerSender<
    P,
    #[cfg(feature = "fuchsia")] T: Transport = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T: Transport,
> {
    sender: protocol::ServerSender<T>,
    _protocol: PhantomData<P>,
}

unsafe impl<P, T> Send for ServerSender<P, T>
where
    protocol::ServerSender<T>: Send,
    T: Transport,
{
}

impl<P, T: Transport> ServerSender<P, T> {
    /// Wraps an untyped sender reference, returning a typed sender reference.
    pub fn wrap_untyped(client: &protocol::ServerSender<T>) -> &Self {
        unsafe { &*(client as *const protocol::ServerSender<T>).cast() }
    }

    /// Returns the underlying untyped sender.
    pub fn as_untyped(&self) -> &protocol::ServerSender<T> {
        &self.sender
    }

    /// Closes the channel from the server end.
    pub fn close(&self) {
        self.as_untyped().close();
    }
}

impl<P, T: Transport> Clone for ServerSender<P, T> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), _protocol: PhantomData }
    }
}

/// A protocol which supports servers.
pub trait ServerProtocol<
    H,
    #[cfg(feature = "fuchsia")] T: Transport = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T: Transport,
>: Sized
{
    /// Handles a received server one-way message with the given handler.
    fn on_one_way(
        handler: &mut H,
        server: &ServerSender<Self, T>,
        ordinal: u64,
        buffer: T::RecvBuffer,
    );

    /// Handles a received server two-way message with the given handler.
    fn on_two_way(
        handler: &mut H,
        server: &ServerSender<Self, T>,
        ordinal: u64,
        buffer: T::RecvBuffer,
        responder: protocol::Responder,
    );
}

/// An adapter for a server protocol handler.
pub struct ServerAdapter<P, H> {
    handler: H,
    _protocol: PhantomData<P>,
}

unsafe impl<P, H> Send for ServerAdapter<P, H> where H: Send {}

impl<P, H> ServerAdapter<P, H> {
    /// Creates a new protocol server handler from a supported handler.
    pub fn from_untyped(handler: H) -> Self {
        Self { handler, _protocol: PhantomData }
    }
}

impl<P, H, T> protocol::ServerHandler<T> for ServerAdapter<P, H>
where
    P: ServerProtocol<H, T>,
    T: Transport,
{
    fn on_one_way(
        &mut self,
        server: &protocol::ServerSender<T>,
        ordinal: u64,
        buffer: T::RecvBuffer,
    ) {
        P::on_one_way(&mut self.handler, ServerSender::wrap_untyped(server), ordinal, buffer)
    }

    fn on_two_way(
        &mut self,
        server: &protocol::ServerSender<T>,
        ordinal: u64,
        buffer: <T as Transport>::RecvBuffer,
        responder: protocol::Responder,
    ) {
        P::on_two_way(
            &mut self.handler,
            ServerSender::wrap_untyped(server),
            ordinal,
            buffer,
            responder,
        )
    }
}

/// A strongly typed server.
pub struct Server<
    P,
    #[cfg(feature = "fuchsia")] T: Transport = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T: Transport,
> {
    server: protocol::Server<T>,
    _protocol: PhantomData<P>,
}

unsafe impl<P, T> Send for Server<P, T>
where
    protocol::Server<T>: Send,
    T: Transport,
{
}

impl<P, T: Transport> Server<P, T> {
    /// Creates a new server from a server end.
    pub fn new(server_end: ServerEnd<P, T>) -> Self {
        Self { server: protocol::Server::new(server_end.into_untyped()), _protocol: PhantomData }
    }

    /// Returns the sender for the server.
    pub fn sender(&self) -> &ServerSender<P, T> {
        ServerSender::wrap_untyped(self.server.sender())
    }

    /// Creates a new server from an untyped server.
    pub fn from_untyped(server: protocol::Server<T>) -> Self {
        Self { server, _protocol: PhantomData }
    }

    /// Runs the server with the provided handler.
    pub async fn run<H>(&mut self, handler: H) -> Result<(), ProtocolError<T::Error>>
    where
        P: ServerProtocol<H, T>,
    {
        self.server.run(ServerAdapter { handler, _protocol: PhantomData::<P> }).await
    }
}

/// A strongly typed `Responder`.
#[must_use]
pub struct Responder<M> {
    responder: protocol::Responder,
    _method: PhantomData<M>,
}

impl<M> Responder<M> {
    /// Creates a new responder.
    pub fn from_untyped(responder: protocol::Responder) -> Self {
        Self { responder, _method: PhantomData }
    }

    /// Responds to the client.
    pub fn respond<'s, P, T, R>(
        self,
        server: &'s ServerSender<P, T>,
        response: R,
    ) -> Result<SendFuture<'s, T>, EncodeError>
    where
        T: Transport,
        M: Method<Protocol = P>,
        R: Encode<T::SendBuffer, Encoded = M::Response>,
    {
        server.as_untyped().send_response(self.responder, M::ORDINAL, response)
    }
}
