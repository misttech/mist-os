// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{registry, CapabilityTrait, ConversionError, Open};
use fidl::endpoints::{create_request_stream, ClientEnd, ControlHandle, RequestStream, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{channel::mpsc, TryStreamExt};
use std::{fmt::Debug, sync::Arc};
use tracing::warn;
use vfs::directory::entry::DirectoryEntry;

#[derive(Debug)]
pub struct Message {
    pub channel: fidl::Channel,
}

impl From<fsandbox::ProtocolPayload> for Message {
    fn from(payload: fsandbox::ProtocolPayload) -> Self {
        Message { channel: payload.channel }
    }
}

/// Types that implement [`Sendable`] let the holder send channels
/// to them.
pub trait Sendable: Send + Sync + Debug {
    fn send(&self, message: Message) -> Result<(), ()>;
}

impl Sendable for mpsc::UnboundedSender<crate::Message> {
    fn send(&self, message: Message) -> Result<(), ()> {
        self.unbounded_send(message).map_err(|_| ())
    }
}

/// A capability that transfers another capability to a [Receiver].
#[derive(Debug)]
pub struct Sender {
    inner: std::sync::Arc<dyn Sendable>,

    /// The FIDL representation of this `Sender`.
    ///
    /// This will be `Some` if was previously converted into a `ClientEnd`, such as by calling
    /// [into_fidl], and the capability is not currently in the registry.
    client_end: Option<ClientEnd<fsandbox::SenderMarker>>,
}

impl Clone for Sender {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), client_end: None }
    }
}

impl Sender {
    pub fn new_sendable(sender: impl Sendable + 'static) -> Self {
        Self { inner: std::sync::Arc::new(sender), client_end: None }
    }

    pub(crate) fn new(sender: mpsc::UnboundedSender<Message>) -> Self {
        Self { inner: std::sync::Arc::new(sender), client_end: None }
    }

    pub(crate) fn send_channel(&self, channel: zx::Channel) -> Result<(), ()> {
        self.send(Message { channel })
    }

    pub fn send(&self, msg: Message) -> Result<(), ()> {
        self.inner.send(msg)
    }

    async fn serve_sender(self, mut stream: fsandbox::SenderRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::SenderRequest::Send_ { channel, control_handle: _ } => {
                    if let Err(_err) = self.send_channel(channel) {
                        stream.control_handle().shutdown_with_epitaph(zx::Status::PEER_CLOSED);
                        return;
                    }
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

        // Move this capability into the registry.
        registry::spawn_task(self.into(), koid, sender.serve_sender(stream));
    }

    /// Sets this Sender's client end to the provided one.
    ///
    /// This should only be used to put a remoted client end back into the Sender after it is
    /// removed from the registry.
    pub(crate) fn set_client_end(&mut self, client_end: ClientEnd<fsandbox::SenderMarker>) {
        self.client_end = Some(client_end)
    }
}

impl From<Sender> for Open {
    fn from(sender: Sender) -> Self {
        Self::new(vfs::service::endpoint(move |_scope, server_end| {
            let _ = sender.send_channel(server_end.into_zx_channel().into());
        }))
    }
}

impl CapabilityTrait for Sender {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(vfs::service::endpoint(move |_scope, server_end| {
            let _ = self.send_channel(server_end.into_zx_channel().into());
        }))
    }
}

impl From<Sender> for ClientEnd<fsandbox::SenderMarker> {
    /// Serves the `fuchsia.sandbox.Sender` protocol for this Sender and moves it into the registry.
    fn from(mut sender: Sender) -> ClientEnd<fsandbox::SenderMarker> {
        sender.client_end.take().unwrap_or_else(|| {
            let (client_end, sender_stream) =
                create_request_stream::<fsandbox::SenderMarker>().unwrap();
            sender.serve_and_register(sender_stream, client_end.get_koid().unwrap());
            client_end
        })
    }
}

impl From<Sender> for fsandbox::Capability {
    fn from(sender: Sender) -> Self {
        fsandbox::Capability::Sender(sender.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Receiver;
    use assert_matches::assert_matches;
    use fidl::endpoints::create_endpoints;
    use fidl_fuchsia_io as fio;
    use fidl_fuchsia_unknown as funknown;
    use futures::StreamExt;
    use vfs::execution_scope::ExecutionScope;

    // NOTE: sending-and-receiving tests are written in `receiver.rs`.

    /// Tests that a Sender can be cloned via `fuchsia.unknown/Cloneable.Clone2`
    /// and capabilities sent to the original and clone arrive at the same Receiver.
    #[fuchsia::test]
    async fn fidl_clone() {
        let (receiver, sender) = Receiver::new();

        // Send a channel through the Sender.
        let (ch1, _ch2) = zx::Channel::create();
        sender.send_channel(ch1).unwrap();

        // Convert the Sender to a FIDL proxy.
        let client_end: ClientEnd<fsandbox::SenderMarker> = sender.into();
        let sender_proxy = client_end.into_proxy().unwrap();

        // Clone the Sender with `Clone2`.
        let (clone_client_end, clone_server_end) = create_endpoints::<funknown::CloneableMarker>();
        let _ = sender_proxy.clone2(clone_server_end);
        let clone_client_end: ClientEnd<fsandbox::SenderMarker> =
            clone_client_end.into_channel().into();
        let clone_proxy = clone_client_end.into_proxy().unwrap();

        // Send a channel through the cloned Sender.
        let (ch1, _ch2) = zx::Channel::create();
        clone_proxy.send_(ch1).unwrap();

        // The Receiver should receive two channels, one from each sender.
        for _ in 0..2 {
            let _ch = receiver.receive().await.unwrap();
        }
    }

    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_node_reference_and_describe() {
        let receiver = {
            let (receiver, sender) = Receiver::new();
            let open: Open = sender.into();
            let (client_end, server_end) = zx::Channel::create();
            let scope = ExecutionScope::new();
            open.open(
                scope,
                fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE,
                ".",
                server_end,
            );

            // The NODE_REFERENCE connection should be terminated on the sender side.
            let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
            let node: fio::NodeProxy = client_end.into_proxy().unwrap();
            let result = node.take_event_stream().next().await.unwrap();
            assert_matches!(
                result,
                Ok(fio::NodeEvent::OnOpen_ { s, info })
                    if s == zx::Status::OK.into_raw()
                    && *info.as_ref().unwrap().as_ref() == fio::NodeInfoDeprecated::Service(fio::Service {})
            );

            receiver
        };

        // After closing the sender, the receiver should be done.
        assert_matches!(receiver.receive().await, None);
    }

    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_describe() {
        let (receiver, sender) = Receiver::new();
        let open: Open = sender.into();

        let (client_end, server_end) = zx::Channel::create();
        // The VFS should send the DESCRIBE event, then hand us the channel.
        open.open(ExecutionScope::new(), fio::OpenFlags::DESCRIBE, ".", server_end);

        // Check we got the channel.
        assert_matches!(receiver.receive().await, Some(_));

        // Check the client got describe.
        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = node.take_event_stream().next().await.unwrap();
        assert_matches!(
            result,
            Ok(fio::NodeEvent::OnOpen_ { s, info })
            if s == zx::Status::OK.into_raw()
            && *info.as_ref().unwrap().as_ref() == fio::NodeInfoDeprecated::Service(fio::Service {})
        );
    }

    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_empty() {
        let (receiver, sender) = Receiver::new();
        let open: Open = sender.into();

        let (client_end, server_end) = zx::Channel::create();
        // The VFS should not send any event, but directly hand us the channel.
        open.open(ExecutionScope::new(), fio::OpenFlags::empty(), ".", server_end);

        // Check that we got the channel.
        assert_matches!(receiver.receive().await, Some(_));

        // Check that there's no event.
        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        assert_matches!(node.take_event_stream().next().await, None);
    }
}
