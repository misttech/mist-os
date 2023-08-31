// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{receiver::Message, AnyCast, Capability, Remote},
    anyhow::{Context, Error},
    fidl::endpoints::create_request_stream,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{channel::mpsc, future::BoxFuture, FutureExt, TryStreamExt},
    moniker::{Moniker, MonikerBase},
    std::fmt::Debug,
};

/// A capability that represents a Zircon handle.
#[derive(Capability, Debug, Clone)]
#[capability(try_clone = "clone", convert = "to_self_only")]
pub struct Sender {
    pub(crate) inner: mpsc::UnboundedSender<Message>,
}

impl Sender {
    pub fn send(&mut self, message: Message) {
        self.inner.unbounded_send(message).expect("TODO: what lifecycle transitions would cause a receiver to be destroyed and leave a sender?");
    }
}

impl Remote for Sender {
    fn to_zx_handle(mut self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (sender_client_end, sender_stream) =
            create_request_stream::<fsandbox::SenderMarker>().unwrap();
        let fut = async move {
            self.serve_sender(sender_stream).await.expect("failed to serve Sender");
        };
        (sender_client_end.into_handle(), Some(fut.boxed()))
    }
}

impl Sender {
    pub async fn serve_sender(
        &mut self,
        mut stream: fsandbox::SenderRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fsandbox::SenderRequest::Send_ { capability, responder } => {
                    if capability.is_invalid() {
                        let _ = responder.send(Err(fsandbox::SenderError::BadHandle));
                        continue;
                    }
                    self.send(Message {
                        handle: capability,
                        flags: fio::OpenFlags::empty(),  // TODO
                        target_moniker: Moniker::root(), // TODO
                    });
                    let _ = responder.send(Ok(()));
                }
            }
        }
        Ok(())
    }
}
