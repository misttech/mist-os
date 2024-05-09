// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{Connector, Message};
use derivative::Derivative;
use fidl::endpoints::Proxy;
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::{
    channel::mpsc::{self, UnboundedReceiver},
    future::{self, Either},
    lock::Mutex,
    StreamExt,
};
use std::pin::pin;
use std::sync::Arc;

/// A capability that transfers another capability to a [Sender].
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Receiver {
    /// `inner` uses an async mutex because it will be locked across an await point
    /// when asynchronously waiting for the next message.
    inner: Arc<Mutex<UnboundedReceiver<Message>>>,
}

impl Clone for Receiver {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl Receiver {
    pub fn new() -> (Self, Connector) {
        let (sender, receiver) = mpsc::unbounded();
        let receiver = Self { inner: Arc::new(Mutex::new(receiver)) };
        (receiver, Connector::new(sender))
    }

    /// Waits to receive a message, or return `None` if there are no more messages and all
    /// senders are dropped.
    pub async fn receive(&self) -> Option<Message> {
        let mut receiver_guard = self.inner.lock().await;
        receiver_guard.next().await
    }

    pub async fn handle_receiver(&self, receiver_proxy: fsandbox::ReceiverProxy) {
        let mut on_closed = receiver_proxy.on_closed();
        loop {
            match future::select(pin!(self.receive()), on_closed).await {
                Either::Left((msg, fut)) => {
                    on_closed = fut;
                    let Some(msg) = msg else {
                        return;
                    };
                    if let Err(_) = receiver_proxy.receive(msg.channel) {
                        return;
                    }
                }
                Either::Right((_, _)) => {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;
    use fuchsia_zircon::{self as zx, AsHandleRef};
    use zx::Peered;

    use super::*;

    #[fuchsia::test]
    async fn send_and_receive() {
        let (receiver, sender) = Receiver::new();

        let (ch1, ch2) = zx::Channel::create();
        sender.send_channel(ch1).unwrap();

        let message = receiver.receive().await.unwrap();

        // Check connectivity.
        message.channel.signal_peer(zx::Signals::empty(), zx::Signals::USER_1).unwrap();
        ch2.wait_handle(zx::Signals::USER_1, zx::Time::INFINITE).unwrap();
    }

    #[fuchsia::test]
    async fn send_fail_when_receiver_dropped() {
        let (receiver, sender) = Receiver::new();

        drop(receiver);

        let (ch1, _ch2) = zx::Channel::create();
        sender.send_channel(ch1).unwrap_err();
    }

    #[test]
    fn receive_blocks_while_connector_alive() {
        let mut ex = fasync::TestExecutor::new();
        let (receiver, sender) = Receiver::new();

        {
            let mut fut = std::pin::pin!(receiver.receive());
            assert!(ex.run_until_stalled(&mut fut).is_pending());
        }

        drop(sender);

        let mut fut = std::pin::pin!(receiver.receive());
        let output = ex.run_until_stalled(&mut fut);
        assert_matches!(output, std::task::Poll::Ready(None));
    }

    /// It should be possible to conclusively ensure that no more messages will arrive.
    #[fuchsia::test]
    async fn drain_receiver() {
        let (receiver, sender) = Receiver::new();

        let (ch1, _ch2) = zx::Channel::create();
        sender.send_channel(ch1).unwrap();

        // Even if all the senders are closed after sending a message, it should still be
        // possible to receive that message.
        drop(sender);

        // Receive the message.
        assert!(receiver.receive().await.is_some());

        // Receiving again will fail.
        assert!(receiver.receive().await.is_none());
    }

    #[fuchsia::test]
    async fn receiver_fidl() {
        let (receiver, sender) = Receiver::new();

        let (ch1, ch2) = zx::Channel::create();
        sender.send_channel(ch1).unwrap();

        let (receiver_proxy, mut receiver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fsandbox::ReceiverMarker>().unwrap();

        let handler_fut = receiver.handle_receiver(receiver_proxy);
        let receive_fut = receiver_stream.next();
        let Either::Right((message, _)) =
            future::select(pin!(handler_fut), pin!(receive_fut)).await
        else {
            panic!("Handler should not finish");
        };
        let message = message.unwrap().unwrap();
        match message {
            fsandbox::ReceiverRequest::Receive { channel, .. } => {
                // Check connectivity.
                channel.signal_peer(zx::Signals::empty(), zx::Signals::USER_1).unwrap();
                ch2.wait_handle(zx::Signals::USER_1, zx::Time::INFINITE).unwrap();
            }
            _ => panic!("Unexpected message"),
        }
    }
}
