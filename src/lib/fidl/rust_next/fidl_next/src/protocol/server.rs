// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL protocol servers.

use crate::protocol::{encode_buffer, MessageBuffer, ProtocolError, Transport};
use crate::{Decoder, Encode, EncodeError, Encoder};

/// Makes a server endpoint from a transport endpoint.
pub fn make_server<T: Transport>(transport: T) -> (Server<T>, Requests<T>) {
    let (sender, receiver) = transport.split();
    (Server { sender }, Requests { receiver })
}

/// A sender for a server endpoint.
#[derive(Clone)]
pub struct Server<T: Transport> {
    sender: T::Sender,
}

impl<T: Transport> Server<T> {
    /// Send an event.
    pub fn send_event<'s, M>(
        &'s self,
        ordinal: u64,
        event: &mut M,
    ) -> Result<T::SendFuture<'s>, EncodeError>
    where
        for<'a> T::Encoder<'a>: Encoder,
        M: for<'a> Encode<T::Encoder<'a>>,
    {
        let mut buffer = T::acquire(&self.sender);
        encode_buffer(&mut buffer, 0, ordinal, event)?;
        Ok(T::send(&self.sender, buffer))
    }

    /// Send a response to a transactional request.
    pub fn send_response<'s, M>(
        &'s self,
        responder: Responder,
        ordinal: u64,
        response: &mut M,
    ) -> Result<T::SendFuture<'s>, EncodeError>
    where
        for<'a> T::Encoder<'a>: Encoder,
        M: for<'a> Encode<T::Encoder<'a>>,
    {
        let mut buffer = T::acquire(&self.sender);
        encode_buffer(&mut buffer, responder.txid, ordinal, response)?;
        Ok(T::send(&self.sender, buffer))
    }
}

/// A receiver for a server endpoint.
pub struct Requests<T: Transport> {
    receiver: T::Receiver,
}

impl<T: Transport> Requests<T> {
    /// Returns the next request from the client, if any.
    pub async fn next(&mut self) -> Result<Option<Request<T>>, ProtocolError<T::Error>>
    where
        for<'a> T::Decoder<'a>: Decoder<'a>,
    {
        let next = T::recv(&mut self.receiver).await.map_err(ProtocolError::TransportError)?;

        if let Some(buffer) = next {
            let (txid, buffer) = MessageBuffer::parse_header(buffer)?;
            if txid == 0 {
                Ok(Some(Request::OneWay { buffer }))
            } else {
                Ok(Some(Request::Transaction { responder: Responder { txid }, buffer }))
            }
        } else {
            Ok(None)
        }
    }
}

/// A request sent to the server.
pub enum Request<T: Transport> {
    /// A one-way request which does not expect a response.
    OneWay {
        /// The buffer containing the request.
        buffer: MessageBuffer<T>,
    },
    /// A transactional request which expects a response.
    Transaction {
        /// A responder which can be used to answer this request.
        responder: Responder,
        /// The buffer containing the request.
        buffer: MessageBuffer<T>,
    },
}

/// A responder for a transactional request.
pub struct Responder {
    txid: u32,
}
