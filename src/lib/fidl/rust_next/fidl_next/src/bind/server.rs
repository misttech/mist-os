// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

use crate::protocol::{self, DispatcherError, Transport};
use crate::{Encode, EncodeError};

use super::{Method, ServerEnd};

/// A storngly typed protocol server.
pub struct Server<T: Transport, P> {
    server: protocol::Server<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> Server<T, P> {
    /// Wraps an untyped server reference, returning a typed server reference.
    pub fn wrap_untyped(client: &protocol::Server<T>) -> &Self {
        unsafe { &*(client as *const protocol::Server<T>).cast() }
    }

    /// Returns the underlying untyped server.
    pub fn as_untyped(&self) -> &protocol::Server<T> {
        &self.server
    }

    /// Closes the channel from the server end.
    pub fn close(&self) {
        self.as_untyped().close();
    }
}

impl<T: Transport, P> Clone for Server<T, P> {
    fn clone(&self) -> Self {
        Self { server: self.server.clone(), _protocol: PhantomData }
    }
}

/// A protocol which supports servers.
pub trait ServerProtocol<T: Transport, H>: Sized {
    /// Handles a received server one-way message with the given handler.
    fn on_one_way(handler: &mut H, server: &Server<T, Self>, ordinal: u64, buffer: T::RecvBuffer);

    /// Handles a received server two-way message with the given handler.
    fn on_two_way(
        handler: &mut H,
        server: &Server<T, Self>,
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
    fn on_one_way(&mut self, server: &protocol::Server<T>, ordinal: u64, buffer: T::RecvBuffer) {
        P::on_one_way(&mut self.handler, Server::wrap_untyped(server), ordinal, buffer)
    }

    fn on_two_way(
        &mut self,
        server: &protocol::Server<T>,
        ordinal: u64,
        buffer: <T as Transport>::RecvBuffer,
        responder: protocol::Responder,
    ) {
        P::on_two_way(&mut self.handler, Server::wrap_untyped(server), ordinal, buffer, responder)
    }
}

/// A strongly typed server dispatcher.
pub struct ServerDispatcher<T: Transport, P> {
    dispatcher: protocol::ServerDispatcher<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> ServerDispatcher<T, P> {
    /// Creates a new server dispatcher from a server end.
    pub fn new(server_end: ServerEnd<T, P>) -> ServerDispatcher<T, P> {
        let dispatcher = protocol::ServerDispatcher::new(server_end.into_untyped());
        Self { dispatcher, _protocol: PhantomData }
    }

    /// Returns the server for the dispatcher.
    pub fn server(&self) -> &Server<T, P> {
        Server::wrap_untyped(self.dispatcher.server())
    }

    /// Creates a new server dispathcer from an untyped server dispatcher.
    pub fn from_untyped(dispatcher: protocol::ServerDispatcher<T>) -> Self {
        Self { dispatcher, _protocol: PhantomData }
    }

    /// Runs the dispatcher with the provided handler.
    pub async fn run<H>(&mut self, handler: H) -> Result<(), DispatcherError<T::Error>>
    where
        P: ServerProtocol<T, H>,
    {
        self.dispatcher.run(ServerAdapter { handler, _protocol: PhantomData::<P> }).await
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
        server: &'s Server<T, P>,
        response: &mut R,
    ) -> Result<T::SendFuture<'s>, EncodeError>
    where
        T: Transport,
        M: Method<Protocol = P>,
        for<'buf> R: Encode<T::Encoder<'buf>, Encoded<'buf> = M::Response<'buf>>,
    {
        server.as_untyped().send_response(self.responder, M::ORDINAL, response)
    }
}
