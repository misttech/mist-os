// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use fidl_next_protocol::{self as protocol, ProtocolError, Transport};

use super::{ClientEnd, Method, ResponseBuffer};

/// A strongly typed client sender.
#[repr(transparent)]
pub struct ClientSender<T: Transport, P> {
    sender: protocol::ClientSender<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> ClientSender<T, P> {
    /// Wraps an untyped sender reference, returning a typed sender reference.
    pub fn wrap_untyped(client: &protocol::ClientSender<T>) -> &Self {
        unsafe { &*(client as *const protocol::ClientSender<T>).cast() }
    }

    /// Returns the underlying untyped sender.
    pub fn as_untyped(&self) -> &protocol::ClientSender<T> {
        &self.sender
    }

    /// Closes the channel from the client end.
    pub fn close(&self) {
        self.as_untyped().close();
    }
}

impl<T: Transport, P> Clone for ClientSender<T, P> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), _protocol: PhantomData }
    }
}

/// A protocol which supports clients.
pub trait ClientProtocol<T: Transport, H>: Sized {
    /// Handles a received client event with the given handler.
    fn on_event(
        handler: &mut H,
        sender: &ClientSender<T, Self>,
        ordinal: u64,
        buffer: T::RecvBuffer,
    );
}

/// An adapter for a client protocol handler.
pub struct ClientAdapter<P, H> {
    handler: H,
    _protocol: PhantomData<P>,
}

impl<P, H> ClientAdapter<P, H> {
    /// Creates a new protocol client handler from a supported handler.
    pub fn from_untyped(handler: H) -> Self {
        Self { handler, _protocol: PhantomData }
    }
}

impl<T, P, H> protocol::ClientHandler<T> for ClientAdapter<P, H>
where
    T: Transport,
    P: ClientProtocol<T, H>,
{
    fn on_event(
        &mut self,
        sender: &protocol::ClientSender<T>,
        ordinal: u64,
        buffer: T::RecvBuffer,
    ) {
        P::on_event(&mut self.handler, ClientSender::wrap_untyped(sender), ordinal, buffer)
    }
}

/// A strongly typed client.
pub struct Client<T: Transport, P> {
    client: protocol::Client<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> Client<T, P> {
    /// Creates a new client from a client end.
    pub fn new(client_end: ClientEnd<T, P>) -> Self {
        Self { client: protocol::Client::new(client_end.into_untyped()), _protocol: PhantomData }
    }

    /// Returns the sender for the client.
    pub fn sender(&self) -> &ClientSender<T, P> {
        ClientSender::wrap_untyped(self.client.sender())
    }

    /// Creates a new client from an untyped client.
    pub fn from_untyped(client: protocol::Client<T>) -> Self {
        Self { client, _protocol: PhantomData }
    }

    /// Runs the client with the provided handler.
    pub async fn run<H>(&mut self, handler: H) -> Result<(), ProtocolError<T::Error>>
    where
        P: ClientProtocol<T, H>,
    {
        self.client.run(ClientAdapter { handler, _protocol: PhantomData::<P> }).await
    }
}

/// A strongly typed response future.
pub struct ResponseFuture<'a, T: Transport, M> {
    future: protocol::ResponseFuture<'a, T>,
    _method: PhantomData<M>,
}

impl<'a, T: Transport, M> ResponseFuture<'a, T, M> {
    /// Creates a new response future from an untyped response future.
    pub fn from_untyped(future: protocol::ResponseFuture<'a, T>) -> Self {
        Self { future, _method: PhantomData }
    }
}

impl<T, M> Future for ResponseFuture<'_, T, M>
where
    T: Transport,
    M: Method,
{
    type Output = Result<ResponseBuffer<T, M>, T::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `self` is pinned, and `future` is a subfield of `self`, so `future` will not be
        // moved.
        let future = unsafe { self.map_unchecked_mut(|this| &mut this.future) };
        future.poll(cx).map_ok(ResponseBuffer::from_untyped)
    }
}
