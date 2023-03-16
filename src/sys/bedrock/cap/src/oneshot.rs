// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{AnyCapability, Capability, Remote},
    fuchsia_zircon as zx,
    futures::channel::oneshot,
    futures::future::BoxFuture,
};

pub type Sender = oneshot::Sender<AnyCapability>;

impl Capability for Sender {}

impl Remote for Sender {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!()
    }
}

pub type Receiver = oneshot::Receiver<AnyCapability>;

impl Capability for Receiver {}

impl Remote for Receiver {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        todo!()
    }
}

pub fn oneshot() -> (Sender, Receiver) {
    oneshot::channel::<AnyCapability>()
}
