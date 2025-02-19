// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing message passing between Netlink and its clients.

use futures::Stream;
use netlink_packet_core::{NetlinkMessage, NetlinkSerializable};

use crate::multicast_groups::ModernGroup;

/// A type capable of sending messages, `M`, from Netlink to a client.
pub trait Sender<M>: Clone + Send + Sync + 'static {
    /// Sends the given message to the client.
    ///
    /// If the message is a multicast, `group` will hold a `Some`; `None` for
    /// unicast messages.
    ///
    /// Implementors must ensure this call does not block.
    fn send(&mut self, message: NetlinkMessage<M>, group: Option<ModernGroup>);
}

/// A type capable of receiving messages, `M`, from a client to Netlink.
///
/// [`Stream`] already provides a sufficient interface for this purpose.
pub trait Receiver<M>: Stream<Item = NetlinkMessage<M>> + Send + 'static {}

/// Blanket implementation allows any [`Stream`] to be used as a [`Receiver`].
impl<M: Send, S> Receiver<M> for S where S: Stream<Item = NetlinkMessage<M>> + Send + 'static {}

/// A type capable of providing a concrete type of [`Sender`] & [`Receiver`].
pub trait SenderReceiverProvider {
    /// The type of [`Sender`] provided.
    type Sender<M: Clone + NetlinkSerializable + Send + Sync + 'static>: Sender<M>;
    /// The type of [`Receiver`] provided.
    type Receiver<M: Send + 'static>: Receiver<M>;
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use futures::{FutureExt as _, StreamExt as _};

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct SentMessage<M> {
        pub message: NetlinkMessage<M>,
        pub group: Option<ModernGroup>,
    }

    impl<M> SentMessage<M> {
        pub(crate) fn unicast(message: NetlinkMessage<M>) -> Self {
            Self { message, group: None }
        }

        pub(crate) fn multicast(message: NetlinkMessage<M>, group: ModernGroup) -> Self {
            Self { message, group: Some(group) }
        }
    }

    #[derive(Clone, Debug)]
    pub(crate) struct FakeSender<M> {
        sender: futures::channel::mpsc::UnboundedSender<SentMessage<M>>,
    }

    impl<M: Clone + Send + NetlinkSerializable + 'static> Sender<M> for FakeSender<M> {
        fn send(&mut self, message: NetlinkMessage<M>, group: Option<ModernGroup>) {
            self.sender
                .unbounded_send(SentMessage { message, group })
                .expect("unable to send message");
        }
    }

    pub(crate) struct FakeSenderSink<M> {
        receiver: futures::channel::mpsc::UnboundedReceiver<SentMessage<M>>,
    }

    impl<M> FakeSenderSink<M> {
        pub(crate) fn take_messages(&mut self) -> Vec<SentMessage<M>> {
            let mut messages = Vec::new();
            while let Some(msg_opt) = self.receiver.next().now_or_never() {
                match msg_opt {
                    Some(msg) => messages.push(msg),
                    None => return messages, // Stream closed.
                };
            }
            // All receiver messages that were ready were added.
            messages
        }

        pub(crate) async fn next_message(&mut self) -> SentMessage<M> {
            self.receiver.next().await.expect("receiver unexpectedly closed")
        }
    }

    pub(crate) fn fake_sender_with_sink<M>() -> (FakeSender<M>, FakeSenderSink<M>) {
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        (FakeSender { sender }, FakeSenderSink { receiver })
    }
}
