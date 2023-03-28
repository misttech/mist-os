// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{AnyCapability, Capability, Remote},
    fuchsia_zircon as zx,
    futures::channel::mpsc,
    futures::future::BoxFuture,
};

pub type Sender = mpsc::UnboundedSender<AnyCapability>;

impl Capability for Sender {}

impl Remote for Sender {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!()
    }
}

pub type Receiver = mpsc::UnboundedReceiver<AnyCapability>;

impl Capability for Receiver {}

impl Remote for Receiver {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!()
    }
}

/// A stream of capabilities from a sender to a receiver.
pub fn multishot() -> (Sender, Receiver) {
    mpsc::unbounded::<AnyCapability>()
}
