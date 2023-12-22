// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{registry, AnyCast, Capability, Message, Sender};
use derivative::Derivative;
use fidl::endpoints::{create_proxy, Proxy, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{
    channel::mpsc::{self, UnboundedReceiver},
    future::{self, Either},
    lock::Mutex,
    StreamExt,
};
use std::fmt::Debug;
use std::pin::pin;
use std::sync::Arc;

/// A capability that transfers another capability to a [Sender].
#[derive(Capability, Derivative)]
#[derivative(Debug)]
pub struct Receiver<T: Default + Debug + Send + Sync + 'static> {
    /// `inner` uses an async mutex because it will be locked across an await point
    /// when asynchronously waiting for the next message.
    inner: Arc<Mutex<UnboundedReceiver<Message<T>>>>,

    /// The FIDL representation of this `Receiver`.
    ///
    /// This will be `Some` if was previously converted into a `ServerEnd`, such as by calling
    /// [into_fidl], and the capability is not currently in the registry.
    server_end: Option<ServerEnd<fsandbox::ReceiverMarker>>,
}

impl<T: Default + Debug + Send + Sync + 'static> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), server_end: None }
    }
}

impl<T: Default + Debug + Send + Sync + 'static> Receiver<T> {
    pub fn new() -> (Self, Sender<T>) {
        let (sender, receiver) = mpsc::unbounded();
        let receiver = Self { inner: Arc::new(Mutex::new(receiver)), server_end: None };
        (receiver, Sender::new(sender))
    }

    /// Waits to receive a message, or return `None` if there are no more messages and all
    /// senders are dropped.
    pub async fn receive(&self) -> Option<Message<T>> {
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
                    let p = msg.payload;
                    if let Err(_) = receiver_proxy.receive(p.channel, p.flags) {
                        return;
                    }
                }
                Either::Right((_, _)) => {
                    return;
                }
            }
        }
    }

    /// Handles the `fuchsia.sandbox.Receiver` protocol for this Receiver
    /// and moves it into the registry.
    fn handle_and_register(self, proxy: fsandbox::ReceiverProxy, koid: zx::Koid) {
        let receiver = self.clone();
        let fut = async move {
            receiver.handle_receiver(proxy).await;
        };

        // Move this capability into the registry.
        let task = fasync::Task::spawn(fut);
        registry::insert_with_task(Box::new(self), koid, task);
    }

    /// Sets this Receiver's server end to the provided one.
    ///
    /// This should only be used to put a remoted server end back into the Receiver after it is
    /// removed from the registry.
    pub(crate) fn set_server_end(&mut self, server_end: ServerEnd<fsandbox::ReceiverMarker>) {
        self.server_end = Some(server_end)
    }
}

impl<T: Default + Debug + Send + Sync + 'static> Capability for Receiver<T> {}

impl<T: Default + Debug + Send + Sync + 'static> From<Receiver<T>>
    for ServerEnd<fsandbox::ReceiverMarker>
{
    fn from(mut receiver: Receiver<T>) -> Self {
        receiver.server_end.take().unwrap_or_else(|| {
            let (receiver_proxy, server_end) = create_proxy::<fsandbox::ReceiverMarker>().unwrap();
            receiver.handle_and_register(receiver_proxy, server_end.get_koid().unwrap());
            server_end
        })
    }
}

impl<T: Default + Debug + Send + Sync + 'static> From<Receiver<T>> for fsandbox::Capability {
    fn from(receiver: Receiver<T>) -> Self {
        fsandbox::Capability::Receiver(receiver.into())
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use fidl_fuchsia_io as fio;
    use zx::Peered;

    use super::*;

    #[fuchsia::test]
    async fn send_and_receive() {
        let (receiver, sender) = Receiver::<()>::new();

        let (ch1, ch2) = zx::Channel::create();
        sender.send_channel(ch1, fio::OpenFlags::empty()).unwrap();

        let message = receiver.receive().await.unwrap();

        // Check connectivity.
        message.payload.channel.signal_peer(zx::Signals::empty(), zx::Signals::USER_1).unwrap();
        ch2.wait_handle(zx::Signals::USER_1, zx::Time::INFINITE).unwrap();
    }

    #[fuchsia::test]
    async fn send_fail_when_receiver_dropped() {
        let (receiver, sender) = Receiver::<()>::new();

        drop(receiver);

        let (ch1, _ch2) = zx::Channel::create();
        sender.send_channel(ch1, fio::OpenFlags::empty()).unwrap_err();
    }

    #[test]
    fn receive_blocks_while_sender_alive() {
        let mut ex = fasync::TestExecutor::new();
        let (receiver, sender) = Receiver::<()>::new();

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
        let (receiver, sender) = Receiver::<()>::new();

        let (ch1, _ch2) = zx::Channel::create();
        sender.send_channel(ch1, fio::OpenFlags::empty()).unwrap();

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
        let (receiver, sender) = Receiver::<()>::new();

        let (ch1, ch2) = zx::Channel::create();
        sender.send_channel(ch1, fio::OpenFlags::empty()).unwrap();

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
