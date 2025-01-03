// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::protocol::{self, DispatcherError, Transport};

use super::{ClientEnd, Method, ResponseBuffer};

/// A strongly typed client.
pub struct Client<T: Transport, P> {
    client: protocol::Client<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> Client<T, P> {
    /// Creates a new client and dispatcher from a client end.
    pub fn new(client_end: ClientEnd<T, P>) -> (Self, ClientDispatcher<T, P>) {
        let (client, dispatcher) = protocol::Client::new(client_end.into_untyped());
        (Self::from_untyped(client), ClientDispatcher::from_untyped(dispatcher))
    }

    /// Creates a new strongly typed client from an untyped client.
    pub fn from_untyped(client: protocol::Client<T>) -> Self {
        Self { client, _protocol: PhantomData }
    }

    /// Returns the underlying untyped client.
    pub fn untyped(&self) -> &protocol::Client<T> {
        &self.client
    }
}

impl<T: Transport, P> Clone for Client<T, P> {
    fn clone(&self) -> Self {
        Self { client: self.client.clone(), _protocol: PhantomData }
    }
}

/// A protocol which supports clients.
pub trait ClientProtocol<T: Transport, H> {
    /// Handles a received client event with the given handler.
    fn on_event(handler: &mut H, ordinal: u64, buffer: T::RecvBuffer);
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
    fn on_event(&mut self, ordinal: u64, buffer: T::RecvBuffer) {
        P::on_event(&mut self.handler, ordinal, buffer)
    }
}

/// A strongly typed client dispatcher.
pub struct ClientDispatcher<T: Transport, P> {
    dispatcher: protocol::ClientDispatcher<T>,
    _protocol: PhantomData<P>,
}

impl<T: Transport, P> ClientDispatcher<T, P> {
    /// Creates a new client dispathcer from an untyped client dispatcher.
    pub fn from_untyped(dispatcher: protocol::ClientDispatcher<T>) -> Self {
        Self { dispatcher, _protocol: PhantomData }
    }

    /// Runs the dispatcher with the provided handler.
    pub async fn run<H>(&mut self, handler: H) -> Result<(), DispatcherError<T::Error>>
    where
        P: ClientProtocol<T, H>,
    {
        self.dispatcher.run(ClientAdapter { handler, _protocol: PhantomData::<P> }).await
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
