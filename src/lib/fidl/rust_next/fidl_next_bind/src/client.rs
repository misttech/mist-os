// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use fidl_next_codec::{Decode, DecoderExt as _};
use fidl_next_protocol::{self as protocol, IgnoreEvents, ProtocolError, Transport};

use crate::{ClientEnd, Error, Method, Response};

/// A strongly typed client sender.
#[repr(transparent)]
pub struct ClientSender<
    P,
    #[cfg(feature = "fuchsia")] T: Transport = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T: Transport,
> {
    sender: protocol::ClientSender<T>,
    _protocol: PhantomData<P>,
}

unsafe impl<P, T> Send for ClientSender<P, T>
where
    T: Transport,
    protocol::ClientSender<T>: Send,
{
}

impl<P, T: Transport> ClientSender<P, T> {
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

impl<P, T: Transport> Clone for ClientSender<P, T> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), _protocol: PhantomData }
    }
}

/// A protocol which supports clients.
pub trait ClientProtocol<
    H,
    #[cfg(feature = "fuchsia")] T: Transport = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T: Transport,
>: Sized + 'static
{
    /// Handles a received client event with the given handler.
    fn on_event(
        handler: &mut H,
        sender: &ClientSender<Self, T>,
        ordinal: u64,
        buffer: T::RecvBuffer,
    ) -> impl Future<Output = ()> + Send;
}

/// An adapter for a client protocol handler.
pub struct ClientAdapter<P, H> {
    handler: H,
    _protocol: PhantomData<P>,
}

unsafe impl<P, H> Send for ClientAdapter<P, H> where H: Send {}

impl<P, H> ClientAdapter<P, H> {
    /// Creates a new protocol client handler from a supported handler.
    pub fn from_untyped(handler: H) -> Self {
        Self { handler, _protocol: PhantomData }
    }
}

impl<P, H, T> protocol::ClientHandler<T> for ClientAdapter<P, H>
where
    P: ClientProtocol<H, T>,
    T: Transport,
{
    fn on_event(
        &mut self,
        sender: &protocol::ClientSender<T>,
        ordinal: u64,
        buffer: T::RecvBuffer,
    ) -> impl Future<Output = ()> + Send {
        P::on_event(&mut self.handler, ClientSender::wrap_untyped(sender), ordinal, buffer)
    }
}

/// A strongly typed client.
pub struct Client<
    P,
    #[cfg(feature = "fuchsia")] T: Transport = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T: Transport,
> {
    client: protocol::Client<T>,
    _protocol: PhantomData<P>,
}

unsafe impl<P, T> Send for Client<P, T>
where
    T: Transport,
    protocol::Client<T>: Send,
{
}

impl<P, T: Transport> Client<P, T> {
    /// Creates a new client from a client end.
    pub fn new(client_end: ClientEnd<P, T>) -> Self {
        Self { client: protocol::Client::new(client_end.into_untyped()), _protocol: PhantomData }
    }

    /// Returns the sender for the client.
    pub fn sender(&self) -> &ClientSender<P, T> {
        ClientSender::wrap_untyped(self.client.sender())
    }

    /// Creates a new client from an untyped client.
    pub fn from_untyped(client: protocol::Client<T>) -> Self {
        Self { client, _protocol: PhantomData }
    }

    /// Runs the client with the provided handler.
    pub async fn run<H>(&mut self, handler: H) -> Result<(), ProtocolError<T::Error>>
    where
        P: ClientProtocol<H, T>,
    {
        self.client.run(ClientAdapter { handler, _protocol: PhantomData::<P> }).await
    }

    /// Runs the client, ignoring any incoming events.
    pub async fn run_sender(&mut self) -> Result<(), ProtocolError<T::Error>> {
        self.client.run(IgnoreEvents).await
    }
}

/// A strongly typed response future.
pub struct ResponseFuture<
    'a,
    M,
    #[cfg(feature = "fuchsia")] T: Transport = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T: Transport,
> {
    future: protocol::ResponseFuture<'a, T>,
    _method: PhantomData<M>,
}

impl<'a, M, T: Transport> ResponseFuture<'a, M, T> {
    /// Creates a new response future from an untyped response future.
    pub fn from_untyped(future: protocol::ResponseFuture<'a, T>) -> Self {
        Self { future, _method: PhantomData }
    }
}

impl<M, T> Future for ResponseFuture<'_, M, T>
where
    M: Method,
    M::Response: Decode<T::RecvBuffer>,
    T: Transport,
{
    type Output = Result<Response<M, T>, Error<T::Error>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `self` is pinned, and `future` is a subfield of `self`, so `future` will not be
        // moved.
        let future = unsafe { self.map_unchecked_mut(|this| &mut this.future) };
        if let Poll::Ready(ready) = future.poll(cx)? {
            Poll::Ready(ready.decode().map_err(Error::Decode))
        } else {
            Poll::Pending
        }
    }
}
