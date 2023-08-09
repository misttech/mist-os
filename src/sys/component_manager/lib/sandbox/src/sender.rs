// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        capability::{Capability, Remote},
        receiver::Message,
        AnyCapability, AnyCloneCapability, TryIntoOpen,
    },
    anyhow::{Context, Error},
    fidl::endpoints::create_request_stream,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{channel::mpsc, future::BoxFuture, FutureExt, TryStreamExt},
    moniker::Moniker,
    std::fmt::Debug,
};

/// A capability that represents a Zircon handle.
#[derive(Debug, Clone)]
pub struct Sender {
    pub(crate) inner: mpsc::UnboundedSender<Message>,
    /// The moniker of the component this sender was given to
    pub(crate) moniker: Moniker,
}

impl Sender {
    pub fn clone_with_new_moniker(&self, moniker: Moniker) -> Self {
        Self { inner: self.inner.clone(), moniker }
    }

    pub fn send(&mut self, message: Message) {
        self.inner.unbounded_send(message).expect("TODO: what lifecycle transitions would cause a receiver to be destroyed and leave a sender?");
    }
}

impl Capability for Sender {}

impl Remote for Sender {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (sender_client_end, sender_stream) =
            create_request_stream::<fsandbox::SenderMarker>().unwrap();
        let fut = async move {
            let mut sender = *self;
            sender.serve_sender(sender_stream).await.expect("failed to serve Sender");
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
                        flags: fio::OpenFlags::empty(), // TODO
                        target_moniker: self.moniker.clone(),
                    });
                    let _ = responder.send(Ok(()));
                }
            }
        }
        Ok(())
    }
}

impl TryIntoOpen for Sender {}

impl<'a> TryFrom<&'a AnyCapability> for &'a Sender {
    type Error = ();

    fn try_from(value: &AnyCapability) -> Result<&Sender, ()> {
        value.as_any().downcast_ref::<Sender>().ok_or(())
    }
}

impl<'a> TryFrom<&'a mut AnyCapability> for &'a mut Sender {
    type Error = ();

    fn try_from(value: &mut AnyCapability) -> Result<&mut Sender, ()> {
        value.as_any_mut().downcast_mut::<Sender>().ok_or(())
    }
}

impl<'a> TryFrom<&'a AnyCloneCapability> for &'a Sender {
    type Error = ();

    fn try_from(value: &AnyCloneCapability) -> Result<&Sender, ()> {
        value.as_any().downcast_ref::<Sender>().ok_or(())
    }
}

impl<'a> TryFrom<&'a mut AnyCloneCapability> for &'a mut Sender {
    type Error = ();

    fn try_from(value: &mut AnyCloneCapability) -> Result<&mut Sender, ()> {
        value.as_any_mut().downcast_mut::<Sender>().ok_or(())
    }
}
