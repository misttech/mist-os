// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCast, Capability, Sender},
    derivative::Derivative,
    fidl::endpoints::{create_proxy, Proxy},
    fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync,
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
    std::sync::{self, Arc},
};

#[derive(Debug)]
pub enum Message {
    Handle(zx::Handle),
    Task(fasync::Task<()>),
}

/// A capability that represents a Zircon handle.
// TODO(fxbug.dev/298112397): Does Receiver need to implement Clone? If not, we could remove the Arc around the Mutex
#[derive(Capability, Clone, Derivative)]
#[derivative(Debug)]
pub struct Receiver {
    inner: Arc<Mutex<Peekable<mpsc::UnboundedReceiver<Message>>>>,
    sender: mpsc::UnboundedSender<Message>,
    #[derivative(Debug = "ignore")]
    sender_tasks: Arc<sync::Mutex<fasync::TaskGroup>>,
}

impl Receiver {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            inner: Arc::new(Mutex::new(receiver.peekable())),
            sender,
            sender_tasks: Arc::new(sync::Mutex::new(fasync::TaskGroup::new())),
        }
    }

    pub fn new_sender(&self) -> Sender {
        Sender::new(self.sender.clone())
    }

    pub async fn receive(&self) -> Message {
        // Panic here instead of blocking, if this happens then we have a bug
        let mut receiver_guard = self
            .inner
            .try_lock()
            .expect("multiple places wanted to read a receiver at the same time");
        // The following unwrap is infallible because we're also holding a sender
        receiver_guard.next().await.unwrap()
    }
}

impl Capability for Receiver {
    fn try_clone(&self) -> Result<Self, ()> {
        Ok(self.clone())
    }

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
                Either::Left((msg, fut)) => {
                    on_closed = fut;
                    match msg {
                        Message::Handle(handle) => {
                            if let Err(_) = receiver_proxy.receive(handle) {
                                return;
                            }
                        }
                        Message::Task(task) => {
                            let mut sender_tasks = self.sender_tasks.lock().unwrap();
                            sender_tasks.add(task);
                        }
                    }
                }
                Either::Right((_, _)) => {
                    return;
                }
            }
        }
    }
}
