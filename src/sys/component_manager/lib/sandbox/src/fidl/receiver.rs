// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{DirReceiver, Receiver};
use fidl::endpoints::Proxy;
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::future::{self, Either};
use std::pin::pin;

impl Receiver {
    pub(crate) async fn handle_receiver(self, receiver_proxy: fsandbox::ReceiverProxy) {
        let mut on_closed = receiver_proxy.on_closed();
        loop {
            match future::select(pin!(self.receive()), on_closed).await {
                Either::Left((msg, fut)) => {
                    on_closed = fut;
                    let Some(msg) = msg else {
                        return;
                    };
                    if let Err(_) = receiver_proxy.receive(msg.channel) {
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

impl DirReceiver {
    pub(crate) async fn handle_receiver(self, receiver_proxy: fsandbox::DirReceiverProxy) {
        let mut on_closed = receiver_proxy.on_closed();
        loop {
            match future::select(pin!(self.receive()), on_closed).await {
                Either::Left((ch, fut)) => {
                    on_closed = fut;
                    let Some(ch) = ch else {
                        return;
                    };
                    if let Err(_) = receiver_proxy.receive(ch) {
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
