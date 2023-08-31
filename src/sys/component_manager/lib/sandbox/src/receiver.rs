// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCast, Capability, Remote, Sender},
    fidl::{
        endpoints::{create_proxy, ProtocolMarker, Proxy, RequestStream},
        AsyncChannel,
    },
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{
        channel::mpsc,
        future::{self, BoxFuture, Either},
        lock::Mutex,
        stream::Peekable,
        FutureExt, StreamExt,
    },
    moniker::Moniker,
    std::fmt::Debug,
    std::pin::pin,
    std::sync::Arc,
};

#[derive(Debug)]
pub struct Message {
    pub handle: zx::Handle,
    pub flags: fio::OpenFlags,
    pub target_moniker: Moniker,
}

impl Message {
    pub fn new(handle: zx::Handle, flags: fio::OpenFlags, target_moniker: Moniker) -> Self {
        Self { handle, flags, target_moniker }
    }

    pub fn take_handle_as_stream<P: ProtocolMarker>(self) -> P::RequestStream {
        let channel = AsyncChannel::from_channel(zx::Channel::from(self.handle))
            .expect("failed to convert handle into async channel");
        P::RequestStream::from_channel(channel)
    }
}

/// A capability that represents a Zircon handle.
#[derive(Capability, Debug, Clone)]
#[capability(try_clone = "clone", convert = "to_self_only")]
pub struct Receiver {
    inner: Arc<Mutex<Peekable<mpsc::UnboundedReceiver<Message>>>>,
    sender: mpsc::UnboundedSender<Message>,
}

impl Receiver {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self { inner: Arc::new(Mutex::new(receiver.peekable())), sender }
    }

    pub fn new_sender(&self) -> Sender {
        Sender { inner: self.sender.clone() }
    }

    pub async fn receive(&self) -> Message {
        // Panic here instead of blocking, if this happens then we have a bug
        let mut receiver_guard = self
            .inner
            .try_lock()
            .expect("multiple places wanted to read a receiver at the same time");
        receiver_guard.next().await.expect("this is infallible, we're also holding a sender")
    }
}

impl Remote for Receiver {
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (receiver_proxy, receiver_server) = create_proxy::<fsandbox::ReceiverMarker>().unwrap();
        let fut = async move {
            self.handle_receiver(receiver_proxy).await;
        };
        (receiver_server.into_handle(), Some(fut.boxed()))
    }
}

impl Receiver {
    pub async fn handle_receiver(&self, receiver_proxy: fsandbox::ReceiverProxy) {
        let mut on_closed = receiver_proxy.on_closed();
        loop {
            match future::select(pin!(self.receive()), on_closed).await {
                Either::Left((message, fut)) => {
                    on_closed = fut;
                    if let Err(_) = receiver_proxy.receive(message.handle) {
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
