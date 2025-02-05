// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL protocol servers.

use core::num::NonZeroU32;

use fidl_next_codec::{Encode, EncodeError, EncoderExt as _};

use crate::{decode_header, encode_header, ProtocolError, Transport};

/// A responder for a two-way message.
#[must_use]
pub struct Responder {
    txid: NonZeroU32,
}

/// A sender for a server endpoint.
pub struct ServerSender<T: Transport> {
    sender: T::Sender,
}

impl<T: Transport> ServerSender<T> {
    /// Closes the channel from the server end.
    pub fn close(&self) {
        T::close(&self.sender);
    }

    /// Send an event.
    pub fn send_event<M>(
        &self,
        ordinal: u64,
        event: &mut M,
    ) -> Result<T::SendFuture<'_>, EncodeError>
    where
        M: for<'a> Encode<T::Encoder<'a>>,
    {
        let mut buffer = T::acquire(&self.sender);
        encode_header::<T>(&mut buffer, 0, ordinal)?;
        T::encoder(&mut buffer).encode_next(event)?;
        Ok(T::send(&self.sender, buffer))
    }

    /// Send a response to a two-way message.
    pub fn send_response<M>(
        &self,
        responder: Responder,
        ordinal: u64,
        response: &mut M,
    ) -> Result<T::SendFuture<'_>, EncodeError>
    where
        M: for<'a> Encode<T::Encoder<'a>>,
    {
        let mut buffer = T::acquire(&self.sender);
        encode_header::<T>(&mut buffer, responder.txid.get(), ordinal)?;
        T::encoder(&mut buffer).encode_next(response)?;
        Ok(T::send(&self.sender, buffer))
    }
}

impl<T: Transport> Clone for ServerSender<T> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone() }
    }
}

/// A type which handles incoming events for a server.
pub trait ServerHandler<T: Transport> {
    /// Handles a received one-way server message.
    ///
    /// The server cannot handle more messages until `on_one_way` completes. If `on_one_way` may
    /// block, perform asynchronous work, or take a long time to process a message, it should
    /// offload work to an async task.
    fn on_one_way(&mut self, sender: &ServerSender<T>, ordinal: u64, buffer: T::RecvBuffer);

    /// Handles a received two-way server message.
    ///
    /// The server cannot handle more messages until `on_two_way` completes. If `on_two_way` may
    /// block, perform asynchronous work, or take a long time to process a message, it should
    /// offload work to an async task.
    fn on_two_way(
        &mut self,
        sender: &ServerSender<T>,
        ordinal: u64,
        buffer: T::RecvBuffer,
        responder: Responder,
    );
}

/// A server for an endpoint.
pub struct Server<T: Transport> {
    sender: ServerSender<T>,
    receiver: T::Receiver,
}

impl<T: Transport> Server<T> {
    /// Creates a new server from a transport.
    pub fn new(transport: T) -> Self {
        let (sender, receiver) = transport.split();
        Self { sender: ServerSender { sender }, receiver }
    }

    /// Returns the sender for the server.
    pub fn sender(&self) -> &ServerSender<T> {
        &self.sender
    }

    /// Runs the server with the provided handler.
    pub async fn run<H>(&mut self, mut handler: H) -> Result<(), ProtocolError<T::Error>>
    where
        H: ServerHandler<T>,
    {
        while let Some(mut buffer) =
            T::recv(&mut self.receiver).await.map_err(ProtocolError::TransportError)?
        {
            let (txid, ordinal) =
                decode_header::<T>(&mut buffer).map_err(ProtocolError::InvalidMessageHeader)?;
            if let Some(txid) = NonZeroU32::new(txid) {
                handler.on_two_way(&self.sender, ordinal, buffer, Responder { txid });
            } else {
                handler.on_one_way(&self.sender, ordinal, buffer);
            }
        }

        Ok(())
    }
}
