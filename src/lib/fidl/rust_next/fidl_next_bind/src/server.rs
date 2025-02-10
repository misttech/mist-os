// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

use fidl_next_codec::{Encode, EncodeError};
use fidl_next_protocol::{self as protocol, ProtocolError, Transport};

use super::{Method, ServerEnd};

/// A storngly typed server sender.
pub struct ServerSender<T: Transport, P> {
    sender: protocol::ServerSender<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> ServerSender<T, P> {
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

impl<T: Transport, P> Clone for ServerSender<T, P> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), _protocol: PhantomData }
    }
}

/// A protocol which supports servers.
pub trait ServerProtocol<T: Transport, H>: Sized {
    /// Handles a received server one-way message with the given handler.
    fn on_one_way(
        handler: &mut H,
        server: &ServerSender<T, Self>,
        ordinal: u64,
        buffer: T::RecvBuffer,
    );

    /// Handles a received server two-way message with the given handler.
    fn on_two_way(
        handler: &mut H,
        server: &ServerSender<T, Self>,
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

impl<P, H> ServerAdapter<P, H> {
    /// Creates a new protocol server handler from a supported handler.
    pub fn from_untyped(handler: H) -> Self {
        Self { handler, _protocol: PhantomData }
    }
}

impl<T, P, H> protocol::ServerHandler<T> for ServerAdapter<P, H>
where
    T: Transport,
    P: ServerProtocol<T, H>,
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
pub struct Server<T: Transport, P> {
    server: protocol::Server<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> Server<T, P> {
    /// Creates a new server from a server end.
    pub fn new(server_end: ServerEnd<T, P>) -> Self {
        Self { server: protocol::Server::new(server_end.into_untyped()), _protocol: PhantomData }
    }

    /// Returns the sender for the server.
    pub fn sender(&self) -> &ServerSender<T, P> {
        ServerSender::wrap_untyped(self.server.sender())
    }

    /// Creates a new server from an untyped server.
    pub fn from_untyped(server: protocol::Server<T>) -> Self {
        Self { server, _protocol: PhantomData }
    }

    /// Runs the server with the provided handler.
    pub async fn run<H>(&mut self, handler: H) -> Result<(), ProtocolError<T::Error>>
    where
        P: ServerProtocol<T, H>,
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
    pub fn respond<'s, T, P, R>(
        self,
        server: &'s ServerSender<T, P>,
        response: &mut R,
    ) -> Result<T::SendFuture<'s>, EncodeError>
    where
        T: Transport,
        M: Method<Protocol = P>,
        for<'buf> R: Encode<T::Encoder<'buf>, Encoded = M::Response>,
    {
        server.as_untyped().send_response(self.responder, M::ORDINAL, response)
    }
}
