// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::error::Error;
use core::future::Future;

use crate::{Decoder, Encoder};

// Design philosophy:
//
// - Fan-out is best handled by protocols (via executors)
// - Fan-in is best handled by transports (in whatever way is appropriate)
//
// Therefore:
//
// - A transport may only have one receiver at a time. To parallelize, spawn a future for each
//   message.
// - A transport may have many senders at a time. If a transport is serialized, then it must
//   determine the best way to enqueue operations.

/// A transport layer which can send and receive messages.
///
/// The futures provided by this trait should be cancel-safe, which constrains their behavior:
///
/// - Operations should not partially complete.
/// - Operations should only complete during polling.
///
/// `SendFuture` should return an `Poll::Ready` with an error when polled after the transport is
/// closed.
pub trait Transport: 'static {
    /// The error type for the transport.
    type Error: Error + Send + Sync + 'static;

    /// Splits the transport into a sender and receiver.
    fn split(self) -> (Self::Sender, Self::Receiver);

    /// The sender half of the transport. Dropping all of the senders for a transport should close
    /// the transport.
    type Sender: Send + Clone;
    /// The buffer type for senders.
    type SendBuffer: Send;
    /// The encoder for send buffers.
    type Encoder<'b>: Encoder + Send
    where
        Self: 'b;
    /// The future type for send operations.
    type SendFuture<'s>: Future<Output = Result<(), Self::Error>> + Send
    where
        Self: 's;

    /// Acquires an empty send buffer for the transport.
    fn acquire(sender: &Self::Sender) -> Self::SendBuffer;
    /// Gets the encoder for a buffer.
    fn encoder(buffer: &mut Self::SendBuffer) -> Self::Encoder<'_>;
    /// Sends an encoded message over the transport.
    fn send(sender: &Self::Sender, buffer: Self::SendBuffer) -> Self::SendFuture<'_>;
    /// Closes the transport.
    fn close(sender: &Self::Sender);

    /// The receiver half of the transport.
    type Receiver: Send;
    /// The future type for receive operations.
    type RecvFuture<'r>: Future<Output = Result<Option<Self::RecvBuffer>, Self::Error>> + Send
    where
        Self: 'r;
    /// The buffer type for receivers.
    type RecvBuffer: Send;
    /// The decoder for receive buffers.
    type Decoder<'b>: Decoder<'b> + Send
    where
        Self: 'b;

    /// Receives an encoded message over the transport.
    fn recv(receiver: &mut Self::Receiver) -> Self::RecvFuture<'_>;
    /// Gets the decoder for a buffer.
    fn decoder(buffer: &mut Self::RecvBuffer) -> Self::Decoder<'_>;
}
