// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{AnyCast, Capability, CloneError, Sender},
    derivative::Derivative,
    fidl::endpoints::{create_proxy, Proxy},
    fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{
        channel::mpsc,
        future::{self, BoxFuture, Either},
        lock::{MappedMutexGuard, Mutex, MutexGuard},
        FutureExt, StreamExt,
    },
    std::fmt::Debug,
    std::pin::pin,
    std::sync::{self, Arc},
};

#[derive(Debug)]
pub enum MessageOrTask<M: Capability + From<zx::Handle>> {
    Message(M),
    Task(fasync::Task<()>),
}

/// A capability that represents a Zircon handle.
// TODO(fxbug.dev/298112397): Does Receiver need to implement Clone? If not, we could remove the Arc around the Mutex
#[derive(Capability, Derivative)]
#[derivative(Debug)]
pub struct Receiver<M: Capability + From<zx::Handle>> {
    inner: Arc<Mutex<PeekableReceiver<M>>>,
    sender: mpsc::UnboundedSender<MessageOrTask<M>>,
    #[derivative(Debug = "ignore")]
    sender_tasks: Arc<sync::Mutex<fasync::TaskGroup>>,
}

impl<M: Capability + From<zx::Handle>> Clone for Receiver<M> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            sender: self.sender.clone(),
            sender_tasks: self.sender_tasks.clone(),
        }
    }
}

impl<M: Capability + From<zx::Handle>> Receiver<M> {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            inner: Arc::new(Mutex::new(PeekableReceiver::new(receiver))),
            sender,
            sender_tasks: Arc::new(sync::Mutex::new(fasync::TaskGroup::new())),
        }
    }

    pub fn new_sender(&self) -> Sender<M> {
        Sender::new(self.sender.clone())
    }

    pub async fn receive(&self) -> M {
        loop {
            let mut receiver_guard = self.inner.lock().await;
            // The following unwrap is infallible because we're also holding a sender
            match receiver_guard.read().await {
                MessageOrTask::Message(msg) => return msg,
                MessageOrTask::Task(task) => {
                    let mut sender_tasks = self.sender_tasks.lock().unwrap();
                    sender_tasks.add(task);
                }
            }
        }
    }

    pub async fn peek<'a>(&'a self) -> MappedMutexGuard<'a, PeekableReceiver<M>, M> {
        let mut receiver_guard = self.inner.lock().await;
        let tasks = receiver_guard.load_peek_value().await;
        for task in tasks {
            let mut sender_tasks = self.sender_tasks.lock().unwrap();
            sender_tasks.add(task);
        }
        MutexGuard::map(receiver_guard, |receiver: &mut PeekableReceiver<M>| {
            receiver.peek_value_mut().unwrap()
        })
    }

    pub async fn handle_receiver(&self, receiver_proxy: fsandbox::ReceiverProxy) {
        let mut on_closed = receiver_proxy.on_closed();
        loop {
            match future::select(pin!(self.receive()), on_closed).await {
                Either::Left((msg, fut)) => {
                    on_closed = fut;
                    let (handle, fut) = msg.to_zx_handle();
                    if let Some(fut) = fut {
                        let mut sender_tasks = self.sender_tasks.lock().unwrap();
                        sender_tasks.spawn(fut);
                    }
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

impl<M: Capability + From<zx::Handle>> Capability for Receiver<M> {
    fn try_clone(&self) -> Result<Self, CloneError> {
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

/// This provides very similar functionality to `futures::stream::Peekable`, but allows a reference
/// to a peeked message to be acquired outside of an async block, which is necessary to provide a
/// `MappedMutexGuard` of the peeked value.
pub struct PeekableReceiver<M: Capability + From<zx::Handle>> {
    receiver: mpsc::UnboundedReceiver<MessageOrTask<M>>,
    peeked_message: Option<M>,
}

impl<M: Capability + From<zx::Handle>> PeekableReceiver<M> {
    fn new(receiver: mpsc::UnboundedReceiver<MessageOrTask<M>>) -> Self {
        Self { receiver, peeked_message: None }
    }

    /// Returns the peeked value, if any.
    fn peek_value_mut(&mut self) -> Option<&mut M> {
        self.peeked_message.as_mut()
    }

    /// Blocks until a message has been loaded. Returns any sender tasks we received in the process
    /// of waiting for the next message.
    async fn load_peek_value(&mut self) -> Vec<fasync::Task<()>> {
        let mut tasks = Vec::new();
        if self.peeked_message.is_some() {
            return tasks;
        }
        loop {
            match self
                .receiver
                .next()
                .await
                .expect("this is infallible, we're also holding a sender")
            {
                MessageOrTask::Message(m) => {
                    self.peeked_message = Some(m);
                    return tasks;
                }
                MessageOrTask::Task(t) => tasks.push(t),
            }
        }
    }

    /// Reads a new MessageOrTask from the receiver, and returns the value.
    async fn read(&mut self) -> MessageOrTask<M> {
        if let Some(m) = self.peeked_message.take() {
            MessageOrTask::Message(m)
        } else {
            self.receiver.next().await.expect("this is infallible, we're also holding a sender")
        }
    }
}
