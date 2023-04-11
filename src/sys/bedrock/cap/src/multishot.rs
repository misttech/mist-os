// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{Capability, Remote},
    fuchsia_zircon as zx,
    futures::channel::mpsc,
    futures::future::BoxFuture,
};

#[derive(Debug, Clone)]
pub struct Sender<T: Capability>(pub mpsc::UnboundedSender<T>);

impl<T: Capability> Capability for Sender<T> {}

impl<T: Capability> Remote for Sender<T> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!()
    }
}

#[derive(Debug)]
pub struct Receiver<T: Capability>(pub mpsc::UnboundedReceiver<T>);

impl<T: Capability> Capability for Receiver<T> {}

impl<T: Capability> Remote for Receiver<T> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!()
    }
}

/// A stream of capabilities from a sender to a receiver.
pub fn multishot<T: Capability>() -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = mpsc::unbounded::<T>();
    (Sender(sender), Receiver(receiver))
}
