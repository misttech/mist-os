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
    channel::mpsc,
    future::{self, Either},
    lock::{MappedMutexGuard, Mutex, MutexGuard},
    StreamExt,
};
use std::fmt::Debug;
use std::pin::pin;
use std::sync::Arc;

/// A capability that transfers another capability to a [Sender].
#[derive(Capability, Derivative)]
#[derivative(Debug)]
pub struct Receiver<T: Default + Debug + Send + Sync + 'static> {
    inner: Arc<Mutex<PeekableReceiver<T>>>,
    sender: mpsc::UnboundedSender<Message<T>>,

    /// The FIDL representation of this `Receiver`.
    ///
    /// This will be `Some` if was previously converted into a `ServerEnd`, such as by calling
    /// [into_fidl], and the capability is not currently in the registry.
    server_end: Option<ServerEnd<fsandbox::ReceiverMarker>>,
}

impl<T: Default + Debug + Send + Sync + 'static> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), sender: self.sender.clone(), server_end: None }
    }
}

impl<T: Default + Debug + Send + Sync + 'static> Receiver<T> {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            inner: Arc::new(Mutex::new(PeekableReceiver::<T>::new(receiver))),
            sender,
            server_end: None,
        }
    }

    pub fn new_sender(&self) -> Sender<T> {
        Sender::new(self.sender.clone())
    }

    pub async fn receive(&self) -> Message<T> {
        let mut receiver_guard = self.inner.lock().await;
        receiver_guard.read().await
    }

    pub async fn peek<'a>(&'a self) -> MappedMutexGuard<'a, PeekableReceiver<T>, Message<T>> {
        let mut receiver_guard = self.inner.lock().await;
        receiver_guard.load_peek_value().await;
        MutexGuard::map(receiver_guard, |receiver: &mut PeekableReceiver<T>| {
            receiver.peek_value_mut().unwrap()
        })
    }

    pub async fn handle_receiver(&self, receiver_proxy: fsandbox::ReceiverProxy) {
        let mut on_closed = receiver_proxy.on_closed();
        loop {
            match future::select(pin!(self.receive()), on_closed).await {
                Either::Left((msg, fut)) => {
                    on_closed = fut;
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

/// This provides very similar functionality to `futures::stream::Peekable`, but allows a reference
/// to a peeked value to be acquired outside of an async block, which is necessary to provide a
/// `MappedMutexGuard` of the peeked value.
pub struct PeekableReceiver<T: Default + Debug + Send + Sync + 'static> {
    receiver: mpsc::UnboundedReceiver<Message<T>>,
    peeked_value: Option<Message<T>>,
}

impl<T: Default + Debug + Send + Sync + 'static> PeekableReceiver<T> {
    fn new(receiver: mpsc::UnboundedReceiver<Message<T>>) -> Self {
        Self { receiver, peeked_value: None }
    }

    /// Returns the peeked value, if any.
    fn peek_value_mut(&mut self) -> Option<&mut Message<T>> {
        self.peeked_value.as_mut()
    }

    /// Blocks until a value has been loaded.
    async fn load_peek_value(&mut self) {
        if self.peeked_value.is_some() {
            return;
        }
        self.peeked_value = Some(
            self.receiver.next().await.expect("this is infallible, we're also holding a sender"),
        );
    }

    /// Reads a new value from the receiver, and returns the value.
    async fn read(&mut self) -> Message<T> {
        if let Some(value) = self.peeked_value.take() {
            value
        } else {
            self.receiver.next().await.expect("this is infallible, we're also holding a sender")
        }
    }
}
