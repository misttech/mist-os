// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cm_types::Name,
    cm_util::WeakTaskGroup,
    futures::future::BoxFuture,
    lazy_static::lazy_static,
    sandbox::{Dict, Message, Receiver, Sender, TryClone},
    tracing::warn,
};

lazy_static! {
    static ref SENDER: Name = "sender".parse().unwrap();
    static ref RECEIVER: Name = "receiver".parse().unwrap();
}

// TODO: use the `Name` type in `Dict`, so that sandboxes aren't holding duplicate strings.

#[derive(Debug, Default)]
pub struct Sandbox {
    inner: Dict,
}

impl Clone for Sandbox {
    fn clone(&self) -> Self {
        Self { inner: self.inner.try_clone().expect("non-cloneable capability in a sandbox") }
    }
}

impl Sandbox {
    pub fn new() -> Self {
        Self { inner: Dict::new() }
    }

    pub fn get_protocol<'a>(&'a self, name: &Name) -> Option<CapabilityDict<'a>> {
        self.inner
            .entries
            .get(&name.as_str().to_string())
            .and_then(|dict| dict.try_into().ok())
            .map(|inner| CapabilityDict { inner })
    }

    pub fn get_protocol_mut<'a>(&'a mut self, name: &Name) -> Option<CapabilityDictMut<'a>> {
        self.inner
            .entries
            .get_mut(&name.as_str().to_string())
            .and_then(|dict| dict.try_into().ok())
            .map(|inner| CapabilityDictMut { inner })
    }

    pub fn get_or_insert_protocol<'a>(&'a mut self, name: Name) -> CapabilityDictMut<'a> {
        CapabilityDictMut {
            inner: self
                .inner
                .entries
                .entry(name.as_str().to_string())
                .or_insert(Box::new(Dict::new()))
                .try_into()
                .unwrap(),
        }
    }
}

impl From<Dict> for Sandbox {
    fn from(inner: Dict) -> Self {
        Self { inner }
    }
}

/// A mutable dict for a single capability.
pub struct CapabilityDict<'a> {
    inner: &'a Dict,
}

impl<'a> CapabilityDict<'a> {
    pub fn get_sender(&self) -> Option<&Sender> {
        self.inner.entries.get(&SENDER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }

    #[allow(unused)]
    pub fn get_receiver(&self) -> Option<&Receiver> {
        self.inner.entries.get(&RECEIVER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }
}

/// A mutable dict for a single capability.
pub struct CapabilityDictMut<'a> {
    inner: &'a mut Dict,
}

impl<'a> CapabilityDictMut<'a> {
    pub fn get_sender(&mut self) -> Option<&mut Sender> {
        self.inner.entries.get_mut(&SENDER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }

    pub fn insert_sender(&mut self, sender: Sender) {
        self.inner.entries.insert(SENDER.as_str().to_string(), Box::new(sender));
    }

    #[allow(unused)]
    pub fn get_receiver(&mut self) -> Option<&mut Receiver> {
        self.inner.entries.get_mut(&RECEIVER.as_str().to_string()).and_then(|v| v.try_into().ok())
    }

    #[allow(unused)]
    pub fn insert_receiver(&mut self, receiver: Receiver) {
        self.inner.entries.insert(RECEIVER.as_str().to_string(), Box::new(receiver));
    }

    /// Sends the message to the sender in this capability dict. If that fails, returns the
    /// message.
    pub fn send(&mut self, message: Message) -> Result<(), Message> {
        if let Some(sender) = self.get_sender() {
            sender.send(message);
            Ok(())
        } else {
            Err(message)
        }
    }
}

/// Waits for a new message on a receiver, and launches a new async task on a `WeakTaskGroup` to
/// handle each new message from the receiver.
pub struct LaunchTaskOnReceive {
    receiver: Receiver,
    task_to_launch: Box<
        dyn Fn(Message) -> BoxFuture<'static, Result<(), anyhow::Error>> + Sync + Send + 'static,
    >,
    // Note that we explicitly need a `WeakTaskGroup` because if our `run` call is scheduled on the
    // same task group as we'll be launching tasks on then if we held a strong reference we would
    // inadvertently give the task group a strong reference to itself and make it un-droppable.
    task_group: WeakTaskGroup,
    task_name: String,
}

impl LaunchTaskOnReceive {
    pub fn new(
        task_group: WeakTaskGroup,
        task_name: impl Into<String>,
        receiver: Receiver,
        task_to_launch: Box<
            dyn Fn(Message) -> BoxFuture<'static, Result<(), anyhow::Error>>
                + Sync
                + Send
                + 'static,
        >,
    ) -> Self {
        Self { receiver, task_to_launch, task_group, task_name: task_name.into() }
    }

    pub async fn run(self) {
        loop {
            let message = self.receiver.receive().await;
            let task_name = self.task_name.clone();
            let fut = (self.task_to_launch)(message);
            self.task_group
                .spawn(async move {
                    if let Err(error) = fut.await {
                        warn!(%error, "{} failed", task_name);
                    }
                })
                .await;
        }
    }
}
