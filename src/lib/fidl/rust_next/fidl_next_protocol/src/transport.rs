// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::error::Error;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use fidl_next_codec::{Decoder, Encoder};

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
pub trait Transport {
    /// The error type for the transport.
    type Error: Error + Send + Sync + 'static;

    /// Splits the transport into a sender and receiver.
    fn split(self) -> (Self::Sender, Self::Receiver);

    /// The sender half of the transport. Dropping all of the senders for a transport should close
    /// the transport.
    type Sender: Send + Sync + Clone;
    /// The buffer type for senders.
    type SendBuffer: Encoder + Send;
    /// The future state for send operations.
    type SendFutureState: Send;

    /// Acquires an empty send buffer for the transport.
    fn acquire(sender: &Self::Sender) -> Self::SendBuffer;
    /// Begins sending a `SendBuffer` over this transport.
    ///
    /// Returns the state for a future which can be polled with `poll_send`.
    fn begin_send(sender: &Self::Sender, buffer: Self::SendBuffer) -> Self::SendFutureState;
    /// Polls a `SendFutureState` for completion with a sender.
    fn poll_send(
        future: Pin<&mut Self::SendFutureState>,
        cx: &mut Context<'_>,
        sender: &Self::Sender,
    ) -> Poll<Result<(), Self::Error>>;
    /// Closes the transport.
    fn close(sender: &Self::Sender);

    /// The receiver half of the transport.
    type Receiver: Send;
    /// The future state for receive operations.
    type RecvFutureState: Send;
    /// The buffer type for receivers.
    type RecvBuffer: Decoder + Send;

    /// Begins receiving a `RecvBuffer` over this transport.
    ///
    /// Returns the state for a future which can be polled with `poll_recv`.
    fn begin_recv(receiver: &mut Self::Receiver) -> Self::RecvFutureState;
    /// Polls a `RecvFutureState` for completion with a receiver.
    fn poll_recv(
        future: Pin<&mut Self::RecvFutureState>,
        cx: &mut Context<'_>,
        receiver: &mut Self::Receiver,
    ) -> Poll<Result<Option<Self::RecvBuffer>, Self::Error>>;
}

/// Helper methods for `Transport`.
pub trait TransportExt: Transport {
    /// Sends an encoded message over the transport.
    fn send(sender: &Self::Sender, buffer: Self::SendBuffer) -> SendFuture<'_, Self> {
        let future_state = Self::begin_send(sender, buffer);
        SendFuture { sender, future_state }
    }

    /// Receives an encoded message over the transport.
    fn recv(receiver: &mut Self::Receiver) -> RecvFuture<'_, Self> {
        let future_state = Self::begin_recv(receiver);
        RecvFuture { receiver, future_state }
    }
}

impl<T: Transport + ?Sized> TransportExt for T {}

/// A future which sends an encoded message over the transport.
#[must_use = "futures do nothing unless polled"]
pub struct SendFuture<'s, T: Transport + ?Sized> {
    sender: &'s T::Sender,
    future_state: T::SendFutureState,
}

impl<T: Transport> Future for SendFuture<'_, T> {
    type Output = Result<(), T::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { Pin::into_inner_unchecked(self) };
        let future_state = unsafe { Pin::new_unchecked(&mut this.future_state) };
        T::poll_send(future_state, cx, this.sender)
    }
}

/// A future which receives an encoded message over the transport.
#[must_use = "futures do nothing unless polled"]
pub struct RecvFuture<'r, T: Transport + ?Sized> {
    receiver: &'r mut T::Receiver,
    future_state: T::RecvFutureState,
}

impl<T: Transport> Future for RecvFuture<'_, T> {
    type Output = Result<Option<T::RecvBuffer>, T::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { Pin::into_inner_unchecked(self) };
        let future_state = unsafe { Pin::new_unchecked(&mut this.future_state) };
        T::poll_recv(future_state, cx, this.receiver)
    }
}
