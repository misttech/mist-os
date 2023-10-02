// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCast, Capability, Remote, Sender},
    fidl::endpoints::{create_proxy, Proxy},
    fidl_fuchsia_component_sandbox as fsandbox,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{
        channel::mpsc,
        future::{self, BoxFuture, Either},
        lock::Mutex,
        stream::Peekable,
        FutureExt, StreamExt,
    },
    std::fmt::Debug,
    std::pin::pin,
    std::sync::Arc,
};

/// A capability that represents the receiving end of a channel that transfers Zircon handles.
#[derive(Capability, Debug, Clone)]
#[capability(try_clone = "clone", convert = "to_self_only")]
pub struct Receiver {
    inner: Arc<Mutex<Peekable<mpsc::UnboundedReceiver<zx::Handle>>>>,
    sender: mpsc::UnboundedSender<zx::Handle>,
}

impl Receiver {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self { inner: Arc::new(Mutex::new(receiver.peekable())), sender }
    }

    pub fn new_sender(&self) -> Sender {
        Sender { inner: self.sender.clone() }
    }

    pub async fn receive(&self) -> zx::Handle {
        // Panic here instead of blocking, if this happens then we have a bug
        let mut receiver_guard = self
            .inner
            .try_lock()
            .expect("multiple places wanted to read a receiver at the same time");
        // The following unwrap is infallible because we're also holding a sender
        receiver_guard.next().await.unwrap()
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
                Either::Left((handle, fut)) => {
                    on_closed = fut;
                    if let Err(_) = receiver_proxy.receive(handle) {
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
