// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FIDL protocol clients.

use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt as _;

use crate::protocol::lockers::Lockers;
use crate::protocol::{encode_buffer, MessageBuffer, ProtocolError, Transport};
use crate::{Decoder, Encode, EncodeError, Encoder};

/// Makes a client endpoint from a transport endpoint.
pub fn make_client<T: Transport>(transport: T) -> (Dispatcher<T>, Client<T>, Events<T>) {
    // TODO: this needs a reasonable bound
    let (events_sender, events_receiver) = unbounded();
    let (transport_sender, transport_receiver) = transport.split();
    let shared = Arc::new(Shared::new());
    (
        Dispatcher { shared: shared.clone(), receiver: transport_receiver, sender: events_sender },
        Client { shared, sender: transport_sender },
        Events { receiver: events_receiver },
    )
}

struct Shared<T: Transport> {
    transactions: Mutex<Lockers<MessageBuffer<T>>>,
    is_stopped: AtomicBool,
}

impl<T: Transport> Shared<T> {
    fn new() -> Self {
        Self { transactions: Mutex::new(Lockers::new()), is_stopped: AtomicBool::new(false) }
    }
}

/// A dispatcher for a client endpoint.
///
/// It must be actively polled to receive events and transaction responses.
pub struct Dispatcher<T: Transport> {
    shared: Arc<Shared<T>>,
    receiver: T::Receiver,
    sender: UnboundedSender<Result<MessageBuffer<T>, ProtocolError<T::Error>>>,
}

impl<T: Transport> Dispatcher<T> {
    /// Runs the dispatcher.
    ///
    /// If the dispatcher encounters an error, it will send the error to the events receiver before
    /// terminating.
    pub async fn run(&mut self)
    where
        for<'a> T::Decoder<'a>: Decoder<'a>,
    {
        if let Err(e) = self.try_run().await {
            // Ignore errors about the receiver being disconnected, since we still want to pump
            // the transport even if the user is ignoring events.
            let _ = self.sender.unbounded_send(Err(e));
        }

        self.shared.is_stopped.store(true, Ordering::Relaxed);
        self.shared.transactions.lock().unwrap().wake_all();
    }

    async fn try_run(&mut self) -> Result<(), ProtocolError<T::Error>>
    where
        for<'a> T::Decoder<'a>: Decoder<'a>,
    {
        while let Some(buffer) =
            T::recv(&mut self.receiver).await.map_err(ProtocolError::TransportError)?
        {
            let (txid, buffer) = MessageBuffer::parse_header(buffer)?;
            if txid == 0 {
                // This is an event, send to the receiver
                // Ignore errors about the receiver being disconnected, since we still want to pump
                // the transport even if the user is ignoring events.
                let _ = self.sender.unbounded_send(Ok(buffer));
            } else {
                let mut transactions = self.shared.transactions.lock().unwrap();
                let entry = transactions
                    .get(txid - 1)
                    .ok_or_else(|| ProtocolError::UnrequestedResponse(txid))?;

                if entry.write(buffer).map_err(|_| ProtocolError::UnrequestedResponse(txid))? {
                    // Reader canceled, we can drop the entry
                    transactions.free(txid - 1);
                }
            }
        }

        Ok(())
    }
}

/// A client endpoint.
#[derive(Clone)]
pub struct Client<T: Transport> {
    shared: Arc<Shared<T>>,
    sender: T::Sender,
}

impl<T: Transport> Client<T> {
    /// Send a request.
    pub fn send_request<'s, M>(
        &'s self,
        ordinal: u64,
        request: &mut M,
    ) -> Result<T::SendFuture<'s>, EncodeError>
    where
        for<'a> T::Encoder<'a>: Encoder,
        M: for<'a> Encode<T::Encoder<'a>>,
    {
        Self::send_message(&self.sender, 0, ordinal, request)
    }

    /// Send a request and await for a response.
    pub fn send_transaction<'s, M>(
        &'s self,
        ordinal: u64,
        transaction: &mut M,
    ) -> Result<TransactionFuture<'s, T>, EncodeError>
    where
        for<'a> T::Encoder<'a>: Encoder,
        M: for<'a> Encode<T::Encoder<'a>>,
    {
        let index = self.shared.transactions.lock().unwrap().alloc();

        // Send with txid = index + 1 because indices start at 0.
        match Self::send_message(&self.sender, index + 1, ordinal, transaction) {
            Ok(future) => Ok(TransactionFuture {
                shared: &self.shared,
                index,
                ordinal,
                state: TransactionFutureState::Sending(future),
            }),
            Err(e) => {
                self.shared.transactions.lock().unwrap().free(index);
                Err(e)
            }
        }
    }

    fn send_message<'s, M>(
        sender: &'s T::Sender,
        txid: u32,
        ordinal: u64,
        message: &mut M,
    ) -> Result<T::SendFuture<'s>, EncodeError>
    where
        for<'a> T::Encoder<'a>: Encoder,
        M: for<'a> Encode<T::Encoder<'a>>,
    {
        let mut buffer = T::acquire(sender);
        encode_buffer(&mut buffer, txid, ordinal, message)?;
        Ok(T::send(sender, buffer))
    }
}

enum TransactionFutureState<'a, T: 'a + Transport> {
    Sending(T::SendFuture<'a>),
    Receiving,
    // We store the completion state locally so that we can free the transaction slot during poll,
    // instead of waiting until the future is dropped.
    Completed,
}

/// A future for a request pending a response.
pub struct TransactionFuture<'a, T: Transport> {
    shared: &'a Shared<T>,
    index: u32,
    ordinal: u64,
    state: TransactionFutureState<'a, T>,
}

impl<T: Transport> Drop for TransactionFuture<'_, T> {
    fn drop(&mut self) {
        let mut transactions = self.shared.transactions.lock().unwrap();
        match self.state {
            // SAFETY: The future was canceled before it could be sent. The transaction ID was never
            // used, so it's safe to immediately reuse.
            TransactionFutureState::Sending(_) => transactions.free(self.index),
            TransactionFutureState::Receiving => {
                if transactions.get(self.index).unwrap().cancel() {
                    transactions.free(self.index);
                }
            }
            // We already freed the slot when we completed.
            TransactionFutureState::Completed => (),
        }
    }
}

impl<T: Transport> TransactionFuture<'_, T> {
    fn poll_receiving(&mut self, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let mut transactions = self.shared.transactions.lock().unwrap();
        if let Some(ready) = transactions.get(self.index).unwrap().read(cx.waker()) {
            transactions.free(self.index);
            self.state = TransactionFutureState::Completed;

            if ready.ordinal() != self.ordinal {
                return Poll::Ready(Err(ProtocolError::InvalidResponseOrdinal {
                    expected: self.ordinal,
                    actual: ready.ordinal(),
                }));
            }

            Poll::Ready(Ok(ready))
        } else {
            Poll::Pending
        }
    }
}

impl<T: Transport> Future for TransactionFuture<'_, T> {
    type Output = Result<MessageBuffer<T>, ProtocolError<T::Error>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We treat the state as pinned as long as it is sending.
        let this = unsafe { Pin::into_inner_unchecked(self) };

        match &mut this.state {
            TransactionFutureState::Sending(future) => {
                // SAFETY: Because the state is sending, we always treat its future as pinned.
                let pinned = unsafe { Pin::new_unchecked(future) };
                match pinned.poll(cx) {
                    // The send has not completed yet. Leave the state as sending.
                    Poll::Pending => {
                        // If we would pend but the dispatcher is stopped, return an error instead.
                        if this.shared.is_stopped.load(Ordering::Relaxed) {
                            return Poll::Ready(Err(ProtocolError::DispatcherStopped));
                        }

                        Poll::Pending
                    }
                    Poll::Ready(Ok(())) => {
                        // The send completed successfully. Change the state to receiving and poll
                        // for receiving.
                        this.state = TransactionFutureState::Receiving;
                        this.poll_receiving(cx)
                    }
                    Poll::Ready(Err(e)) => {
                        // The send completed unsuccessfully. We can safely free the cell and set
                        // our state to completed.

                        this.shared.transactions.lock().unwrap().free(this.index);
                        this.state = TransactionFutureState::Completed;
                        Poll::Ready(Err(ProtocolError::TransportError(e)))
                    }
                }
            }
            TransactionFutureState::Receiving => this.poll_receiving(cx),
            // We could reach here if this future is polled after completion, but that's not
            // supposed to happen.
            TransactionFutureState::Completed => unreachable!(),
        }
    }
}

/// The events for a client endpoint.
pub struct Events<T: Transport> {
    receiver: UnboundedReceiver<Result<MessageBuffer<T>, ProtocolError<T::Error>>>,
}

impl<T: Transport> Events<T> {
    /// Returns the next event received by the client, if any.
    pub async fn next(&mut self) -> Result<Option<MessageBuffer<T>>, ProtocolError<T::Error>> {
        self.receiver.next().await.transpose()
    }
}
