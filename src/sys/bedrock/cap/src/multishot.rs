// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{Capability, Remote},
    anyhow::{Context, Error},
    fidl::endpoints::{create_request_stream, ControlHandle, Responder},
    fidl_fuchsia_component_bedrock as fbedrock, fuchsia_async as fasync, fuchsia_zircon as zx,
    fuchsia_zircon::HandleBased,
    futures::{future::BoxFuture, FutureExt, StreamExt, TryStreamExt},
};

#[derive(Debug)]
pub struct Sender<T: Capability>(pub async_channel::Sender<T>);

impl<T: Capability + TryFrom<zx::Handle>> Capability for Sender<T> {}

impl<T: Capability + TryFrom<zx::Handle>> Sender<T> {
    async fn serve_sender(self, mut stream: fbedrock::SenderRequestStream) -> Result<(), Error> {
        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fbedrock::SenderRequest::Send_ { capability: handle, responder, .. } => {
                    let capability = match T::try_from(handle) {
                        Ok(capability) => capability,
                        Err(_) => {
                            responder
                                .send(Err(fbedrock::SenderError::BadHandle))
                                .context("failed to send response")?;
                            continue;
                        }
                    };

                    if self.0.send(capability).await.is_err() {
                        responder.control_handle().shutdown_with_epitaph(zx::Status::BAD_STATE);
                        continue;
                    }

                    responder.send(Ok(())).context("failed to send response")?;
                }
            }
        }

        Ok(())
    }
}

impl<T: Capability + TryFrom<zx::Handle>> Remote for Sender<T> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (sender_client_end, sender_stream) =
            create_request_stream::<fbedrock::SenderMarker>().unwrap();

        let fut = async move {
            (*self).serve_sender(sender_stream).await.expect("failed to serve Sender");
        };

        (sender_client_end.into_handle(), Some(fut.boxed()))
    }
}

impl<T: Capability> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender(self.0.clone())
    }
}

#[derive(Debug)]
pub struct Receiver<T: Capability>(pub async_channel::Receiver<T>);

impl<T: Capability> Capability for Receiver<T> {}

impl<T: Capability> Receiver<T> {
    async fn serve_receiver(
        mut self,
        mut stream: fbedrock::ReceiverRequestStream,
    ) -> Result<(), Error> {
        // Tasks that serve the zx handles for values received from this Receiver.
        let mut value_tasks = vec![];

        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fbedrock::ReceiverRequest::Receive { responder, .. } => match self.0.next().await {
                    Some(value) => {
                        let (handle, fut) = Box::new(value).to_zx_handle();
                        if let Some(fut) = fut {
                            value_tasks.push(fasync::Task::spawn(fut));
                        }
                        responder.send(handle).context("failed to send response")?;
                    }
                    None => {
                        responder.control_handle().shutdown_with_epitaph(zx::Status::OK);
                    }
                },
            }
        }

        Ok(())
    }
}

impl<T: Capability> Remote for Receiver<T> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (receiver_client_end, receiver_stream) =
            create_request_stream::<fbedrock::ReceiverMarker>().unwrap();

        let fut = async move {
            (*self).serve_receiver(receiver_stream).await.expect("failed to serve Receiver");
        };

        (receiver_client_end.into_handle(), Some(fut.boxed()))
    }
}

impl<T: Capability> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Receiver(self.0.clone())
    }
}

/// A stream of capabilities from a sender to a receiver.
pub fn multishot<T: Capability>() -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = async_channel::unbounded::<T>();
    (Sender(sender), Receiver(receiver))
}

#[cfg(test)]
mod tests {
    use {
        crate::{handle::Handle, multishot::*},
        anyhow::{anyhow, Error},
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_component_bedrock as fbedrock,
        fuchsia_zircon::{self as zx, AsHandleRef},
        futures::{try_join, StreamExt},
    };

    /// Tests that a capability sent through the FIDL method `Sender.Send` can be received
    /// through the corresponding Receiver.
    #[fuchsia::test]
    async fn serve_sender_send() -> Result<(), Error> {
        let (sender, mut receiver) = multishot::<Handle>();

        let (sender_proxy, sender_stream) = create_proxy_and_stream::<fbedrock::SenderMarker>()?;
        let server = sender.serve_sender(sender_stream);

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let client = async move {
            sender_proxy
                .send_(event.into_handle())
                .await
                .context("failed to call Send")?
                .map_err(|err| anyhow!("failed to send: {:?}", err))?;

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        // The sent handle should be available on the receiving end.
        let handle = receiver.0.next().await.expect("receiver should have a handle");
        let got_koid = handle.get_koid().unwrap();
        assert_eq!(got_koid, expected_koid);

        Ok(())
    }

    /// Tests that a capability sent through a Sender can be received through the FIDL method
    /// `Receiver.Receive`.
    #[fuchsia::test]
    async fn serve_receiver_receive() -> Result<(), Error> {
        let (sender, receiver) = multishot::<Handle>();

        let (receiver_proxy, receiver_stream) =
            create_proxy_and_stream::<fbedrock::ReceiverMarker>()?;
        let server = receiver.serve_receiver(receiver_stream);

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        // Send the event through the Sender.
        let handle = Handle::from(event.into_handle());
        sender.0.send(handle).await.unwrap();

        let client = async move {
            let handle = receiver_proxy.receive().await.context("failed to call Receive")?;

            // The sent handle should be available on the receiving end.
            let got_koid = handle.get_koid().unwrap();
            assert_eq!(got_koid, expected_koid);

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        Ok(())
    }
}
