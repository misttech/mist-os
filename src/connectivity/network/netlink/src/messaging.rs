// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing message passing between Netlink and its clients.

use futures::Stream;
use netlink_packet_utils::Emitable;

/// A type capable of sending messages, `M`, from Netlink to a client.
pub trait Sender<M>: Clone + Send + Sync + 'static {
    /// Sends the given message to the client.
    ///
    /// Implementors must ensure this call does not block.
    fn send(&mut self, message: M);
}

/// A type capable of receiving messages, `M`, from a client to Netlink.
///
/// [`Stream`] already provides a sufficient interface for this purpose.
pub trait Receiver<M>: Stream<Item = M> + Send + 'static {}

/// Blanket implementation allows any [`Stream`] to be used as a [`Receiver`].
impl<M: Send, S> Receiver<M> for S where S: Stream<Item = M> + Send + 'static {}

/// A type capable of providing a concrete type of [`Sender`] & [`Receiver`].
pub trait SenderReceiverProvider {
    /// The type of [`Sender`] provided.
    type Sender<M: Clone + Emitable + Send + Sync + 'static>: Sender<M>;
    /// The type of [`Receiver`] provided.
    type Receiver<M: Send + 'static>: Receiver<M>;
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use std::{
        marker::PhantomData,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Debug, Default)]
    pub(crate) struct FakeSender<M> {
        sent_messages: Arc<Mutex<Vec<M>>>,
    }

    impl<M: Clone + Send + Emitable + 'static> Sender<M> for FakeSender<M> {
        fn send(&mut self, message: M) {
            self.sent_messages.lock().unwrap().push(message)
        }
    }

    pub(crate) struct FakeSenderSink<M> {
        messages: Arc<Mutex<Vec<M>>>,
    }

    impl<M> FakeSenderSink<M> {
        pub(crate) fn take_messages(&mut self) -> Vec<M> {
            self.messages.lock().unwrap().drain(..).collect()
        }
    }

    pub(crate) fn fake_sender_with_sink<M>() -> (FakeSender<M>, FakeSenderSink<M>) {
        let shared_message_buffer = Arc::new(Mutex::new(Vec::default()));
        (
            FakeSender { sent_messages: shared_message_buffer.clone() },
            FakeSenderSink { messages: shared_message_buffer },
        )
    }

    #[derive(Debug)]
    pub(crate) struct FakeReceiver<M>(PhantomData<M>);

    impl<M> Default for FakeReceiver<M> {
        fn default() -> Self {
            FakeReceiver(PhantomData)
        }
    }

    impl<M> Stream for FakeReceiver<M> {
        type Item = M;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(None)
        }
    }

    pub(crate) struct FakeSenderReceiverProvider;

    impl SenderReceiverProvider for FakeSenderReceiverProvider {
        type Sender<M: Clone + Emitable + Send + Sync + 'static> = FakeSender<M>;
        type Receiver<M: Send + 'static> = FakeReceiver<M>;
    }
}
