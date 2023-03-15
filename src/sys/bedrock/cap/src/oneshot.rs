// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{AnyCapability, Capability},
    futures::channel::oneshot,
};

pub type Sender = oneshot::Sender<AnyCapability>;

impl Capability for Sender {}

pub type Receiver = oneshot::Receiver<AnyCapability>;

impl Capability for Receiver {}

pub fn oneshot() -> (Sender, Receiver) {
    oneshot::channel::<AnyCapability>()
}
