// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{registry, CapabilityTrait, ConversionError, Open};
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_zircon::{self as zx};
use futures::channel::mpsc;
use std::{fmt::Debug, sync::Arc};
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
#[derive(Debug, Clone)]
pub struct Sender {
    inner: std::sync::Arc<dyn Sendable>,
}

impl Sender {
    pub fn new_sendable(sender: impl Sendable + 'static) -> Self {
        Self { inner: std::sync::Arc::new(sender) }
    }

    pub(crate) fn new(sender: mpsc::UnboundedSender<Message>) -> Self {
        Self { inner: std::sync::Arc::new(sender) }
    }

    pub(crate) fn send_channel(&self, channel: zx::Channel) -> Result<(), ()> {
        self.send(Message { channel })
    }

    pub fn send(&self, msg: Message) -> Result<(), ()> {
        self.inner.send(msg)
    }
}

impl From<Sender> for Open {
    fn from(sender: Sender) -> Self {
        Self::new(vfs::service::endpoint(move |_scope, server_end| {
            let _ = sender.send_channel(server_end.into_zx_channel().into());
        }))
    }
}

impl Sendable for Sender {
    fn send(&self, message: Message) -> Result<(), ()> {
        self.send(message)
    }
}

impl CapabilityTrait for Sender {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(vfs::service::endpoint(move |_scope, server_end| {
            let _ = self.send_channel(server_end.into_zx_channel().into());
        }))
    }
}

impl From<Sender> for fsandbox::SenderCapability {
    fn from(value: Sender) -> Self {
        fsandbox::SenderCapability { token: registry::insert_token(value.into()) }
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
    use fidl::endpoints::ClientEnd;
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;
    use vfs::execution_scope::ExecutionScope;
    use zx::HandleBased;

    // NOTE: sending-and-receiving tests are written in `receiver.rs`.

    /// Tests that a Sender can be cloned by cloning its FIDL token.
    /// and capabilities sent to the original and clone arrive at the same Receiver.
    #[fuchsia::test]
    async fn fidl_clone() {
        let (receiver, sender) = Receiver::new();

        // Send a channel through the Sender.
        let (ch1, _ch2) = zx::Channel::create();
        sender.send_channel(ch1).unwrap();

        // Convert the Sender to a FIDL token.
        let fidl_sender: fsandbox::SenderCapability = sender.into();

        // Clone the Sender by cloning the token.
        let token_clone = fsandbox::SenderCapability {
            token: fidl_sender.token.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        };
        let sender_clone =
            match crate::Capability::try_from(fsandbox::Capability::Sender(token_clone)).unwrap() {
                crate::Capability::Sender(sender) => sender,
                capability @ _ => panic!("wrong type {capability:?}"),
            };

        // Send a channel through the cloned Sender.
        let (ch1, _ch2) = zx::Channel::create();
        sender_clone.send_channel(ch1).unwrap();

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
