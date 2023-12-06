// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl::endpoints::{create_request_stream, ClientEnd, ControlHandle, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{channel::mpsc, TryStreamExt};
use std::any;
use std::fmt::Debug;
use tracing::warn;

use crate::{registry, AnyCapability, AnyCast, Capability, ConversionError, Open};

/// A capability that transfers another capability to a [Receiver].
#[derive(Capability, Debug)]
pub struct Sender<T: Capability + From<zx::Handle>> {
    inner: mpsc::UnboundedSender<T>,

    /// The FIDL representation of this `Sender`.
    ///
    /// This will be `Some` if was previously converted into a `ClientEnd`, such as by calling
    /// [into_fidl], and the capability is not currently in the registry.
    client_end: Option<ClientEnd<fsandbox::SenderMarker>>,
}

impl<T: Capability + From<zx::Handle>> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), client_end: None }
    }
}

impl<T: Capability + From<zx::Handle>> Sender<T> {
    pub(crate) fn new(sender: mpsc::UnboundedSender<T>) -> Self {
        Self { inner: sender, client_end: None }
    }

    pub fn send(&self, capability: T) {
        self.send_internal(capability)
    }

    pub fn send_handle(&self, handle: zx::Handle) {
        self.send_internal(T::from(handle))
    }

    fn send_internal(&self, capability: T) {
        // TODO: what lifecycle transitions would cause a receiver to be destroyed and leave a sender?
        self.inner.unbounded_send(capability).expect("Sender has no corresponding Receiver")
    }

    async fn serve_sender(self, mut stream: fsandbox::SenderRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::SenderRequest::Send_ { capability: fidl_capability, control_handle } => {
                    let Ok(any) = AnyCapability::try_from(fidl_capability) else {
                        control_handle.shutdown();
                        return;
                    };
                    let Ok(capability) = T::try_from(any) else {
                        control_handle.shutdown();
                        return;
                    };
                    self.send(capability);
                }
                fsandbox::SenderRequest::Open {
                    flags: _,
                    mode: _,
                    path: _,
                    object,
                    control_handle: _,
                } => {
                    self.send_handle(object.into());
                }
                fsandbox::SenderRequest::Clone2 { request, control_handle: _ } => {
                    // The clone is registered under the koid of the client end.
                    let koid = request.basic_info().unwrap().related_koid;
                    let server_end: ServerEnd<fsandbox::SenderMarker> =
                        request.into_channel().into();
                    let stream = server_end.into_stream().unwrap();
                    self.clone().serve_and_register(stream, koid);
                }
                fsandbox::SenderRequest::_UnknownMethod { ordinal, .. } => {
                    warn!("Received unknown Sender request with ordinal {ordinal}");
                }
            }
        }
    }

    /// Serves the `fuchsia.sandbox.Sender` protocol for this Sender and moves it into the registry.
    pub fn serve_and_register(self, stream: fsandbox::SenderRequestStream, koid: zx::Koid) {
        let sender = self.clone();
        let fut = sender.serve_sender(stream);

        // Move this capability into the registry.
        let task = fasync::Task::spawn(fut);
        registry::insert_with_task(Box::new(self), koid, task);
    }

    /// Sets this Sender's client end to the provided one.
    ///
    /// This should only be used to put a remoted client end back into the Sender after it is
    /// removed from the registry.
    pub(crate) fn set_client_end(&mut self, client_end: ClientEnd<fsandbox::SenderMarker>) {
        self.client_end = Some(client_end)
    }
}

impl<T: Capability + From<zx::Handle>> Capability for Sender<T> {
    fn try_into_capability(
        self,
        type_id: any::TypeId,
    ) -> Result<Box<dyn any::Any>, ConversionError> {
        if type_id == any::TypeId::of::<Self>() {
            return Ok(Box::new(self) as Box<dyn any::Any>);
        }
        if type_id == any::TypeId::of::<Open>() {
            return Ok(Box::new(Open::from(self)) as Box<dyn any::Any>);
        }
        Err(ConversionError::NotSupported)
    }
}

impl<T: Capability + From<zx::Handle>> From<Sender<T>> for ClientEnd<fsandbox::SenderMarker> {
    /// Serves the `fuchsia.sandbox.Sender` protocol for this Sender and moves it into the registry.
    fn from(mut sender: Sender<T>) -> ClientEnd<fsandbox::SenderMarker> {
        sender.client_end.take().unwrap_or_else(|| {
            let (client_end, sender_stream) =
                create_request_stream::<fsandbox::SenderMarker>().unwrap();
            sender.serve_and_register(sender_stream, client_end.get_koid().unwrap());
            client_end
        })
    }
}

impl<T: Capability + From<zx::Handle>> From<Sender<T>> for fsandbox::Capability {
    fn from(sender: Sender<T>) -> Self {
        fsandbox::Capability::Sender(sender.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OneShotHandle, Receiver};
    use anyhow::{Context, Result};
    use fidl::endpoints::create_endpoints;
    use fidl_fuchsia_unknown as funknown;
    use fuchsia_zircon::HandleBased;

    /// Tests that a Sender can be cloned via `fuchsia.unknown/Cloneable.Clone2`
    /// and capabilities sent to the original and clone arrive at the same Receiver.
    #[fuchsia::test]
    async fn fidl_clone() -> Result<()> {
        let receiver = Receiver::<OneShotHandle>::new();
        let sender = receiver.new_sender();

        // Send a OneShotHandle through the Sender.
        let event = zx::Event::create();
        sender.send(OneShotHandle::from(event.into_handle()));

        // Convert the Sender to a FIDL proxy.
        let client_end: ClientEnd<fsandbox::SenderMarker> = sender.into();
        let sender_proxy = client_end.into_proxy().unwrap();

        // Clone the Sender with `Clone2`.
        let (clone_client_end, clone_server_end) = create_endpoints::<funknown::CloneableMarker>();
        let _ = sender_proxy.clone2(clone_server_end);
        let clone_client_end: ClientEnd<fsandbox::SenderMarker> =
            clone_client_end.into_channel().into();
        let clone_proxy = clone_client_end.into_proxy().unwrap();

        // Send a OneShotHandle through the clone.
        let event = zx::Event::create();
        clone_proxy
            .send_(OneShotHandle::from(event.into_handle()).into_fidl())
            .context("failed to call Send")?;

        // The Receiver should receive two Unit capabilities, one from each sender.
        for _ in 0..2 {
            let one_shot = receiver.receive().await;
            one_shot.get_handle().unwrap();
        }

        Ok(())
    }
}
