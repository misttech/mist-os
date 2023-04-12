// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{Capability, Remote},
    anyhow::{Context, Error},
    fidl::endpoints::{create_request_stream, ControlHandle, Responder},
    fidl_fuchsia_component_bedrock as fbedrock, fuchsia_zircon as zx,
    fuchsia_zircon::HandleBased,
    futures::channel::mpsc,
    futures::{future::BoxFuture, FutureExt, TryStreamExt},
};

#[derive(Debug)]
pub struct Sender<T: Capability>(pub mpsc::UnboundedSender<T>);

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
                                .send(&mut Err(fbedrock::SenderError::BadHandle))
                                .context("failed to send response")?;
                            continue;
                        }
                    };

                    if self.0.unbounded_send(capability).is_err() {
                        responder.control_handle().shutdown_with_epitaph(zx::Status::BAD_STATE);
                        continue;
                    }

                    responder.send(&mut Ok(())).context("failed to send response")?;
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
}
