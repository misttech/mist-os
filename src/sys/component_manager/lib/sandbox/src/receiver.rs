// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        capability::{Capability, Remote},
        sender::Sender,
        AnyCapability, AnyCloneCapability, TryIntoOpen,
    },
    anyhow::{Context, Error},
    fidl::{
        endpoints::{create_request_stream, ProtocolMarker, RequestStream},
        AsyncChannel,
    },
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{
        channel::mpsc, future::BoxFuture, lock::Mutex, stream::Peekable, FutureExt, StreamExt,
        TryStreamExt,
    },
    moniker::Moniker,
    std::fmt::Debug,
    std::sync::Arc,
};

#[derive(Debug)]
pub struct Message {
    pub handle: zx::Handle,
    pub flags: fio::OpenFlags,
    pub target_moniker: Moniker,
}

impl Message {
    pub fn new(handle: zx::Handle, flags: fio::OpenFlags, target_moniker: Moniker) -> Self {
        Self { handle, flags, target_moniker }
    }

    pub fn take_handle_as_stream<P: ProtocolMarker>(self) -> P::RequestStream {
        let channel = AsyncChannel::from_channel(zx::Channel::from(self.handle))
            .expect("failed to convert handle into async channel");
        P::RequestStream::from_channel(channel)
    }
}

/// A capability that represents a Zircon handle.
#[derive(Debug, Clone)]
pub struct Receiver {
    inner: Arc<Mutex<Peekable<mpsc::UnboundedReceiver<Message>>>>,
    sender: mpsc::UnboundedSender<Message>,
}

impl Receiver {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self { inner: Arc::new(Mutex::new(receiver.peekable())), sender }
    }

    pub fn new_sender(&self, moniker: Moniker) -> Sender {
        Sender { inner: self.sender.clone(), moniker: moniker }
    }

    pub async fn receive(&self) -> Message {
        // Panic here instead of blocking, if this happens then we have a bug
        let mut receiver_guard = self
            .inner
            .try_lock()
            .expect("multiple places wanted to read a receiver at the same time");
        receiver_guard.next().await.expect("this is infallible, we're also holding a sender")
    }
}

impl Capability for Receiver {}

impl Remote for Receiver {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (receiver_client_end, receiver_stream) =
            create_request_stream::<fsandbox::ReceiverMarker>().unwrap();
        let fut = async move {
            let receiver = *self;
            receiver.serve_receiver(receiver_stream).await.expect("failed to serve Receiver");
        };
        (receiver_client_end.into_handle(), Some(fut.boxed()))
    }
}

impl Receiver {
    pub async fn serve_receiver(
        &self,
        mut stream: fsandbox::ReceiverRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fsandbox::ReceiverRequest::Receive { responder } => {
                    let message = self.receive().await;
                    let _ = responder.send(message.handle);
                }
            }
        }
        Ok(())
    }
}

impl TryIntoOpen for Receiver {}

impl<'a> TryFrom<&'a AnyCapability> for &'a Receiver {
    type Error = ();

    fn try_from(value: &AnyCapability) -> Result<&Receiver, ()> {
        value.as_any().downcast_ref::<Receiver>().ok_or(())
    }
}

impl<'a> TryFrom<&'a mut AnyCapability> for &'a mut Receiver {
    type Error = ();

    fn try_from(value: &mut AnyCapability) -> Result<&mut Receiver, ()> {
        value.as_any_mut().downcast_mut::<Receiver>().ok_or(())
    }
}

impl<'a> TryFrom<&'a AnyCloneCapability> for &'a Receiver {
    type Error = ();

    fn try_from(value: &AnyCloneCapability) -> Result<&Receiver, ()> {
        value.as_any().downcast_ref::<Receiver>().ok_or(())
    }
}

impl<'a> TryFrom<&'a mut AnyCloneCapability> for &'a mut Receiver {
    type Error = ();

    fn try_from(value: &mut AnyCloneCapability) -> Result<&mut Receiver, ()> {
        value.as_any_mut().downcast_mut::<Receiver>().ok_or(())
    }
}
