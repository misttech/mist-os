// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::registry;
use crate::{Connector, ConversionError, Message, Receiver};
use fidl::endpoints::ClientEnd;
use fidl::handle::Channel;
use futures::channel::mpsc;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use vfs::execution_scope::ExecutionScope;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

impl Connector {
    pub(crate) fn send_channel(&self, channel: Channel) -> Result<(), ()> {
        self.send(Message { channel })
    }

    pub(crate) fn new_with_fidl_receiver(
        receiver_client: ClientEnd<fsandbox::ReceiverMarker>,
        scope: &fasync::Scope,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        let receiver = Receiver::new(receiver);
        // Exits when ServerEnd<Receiver> is closed
        scope.spawn(receiver.handle_receiver(receiver_client.into_proxy()));
        Self::new_sendable(sender)
    }
}

impl crate::RemotableCapability for Connector {
    fn try_into_directory_entry(
        self,
        _scope: ExecutionScope,
    ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(vfs::service::endpoint(move |_scope, server_end| {
            let _ = self.send_channel(server_end.into_zx_channel().into());
        }))
    }
}

impl From<Connector> for fsandbox::Connector {
    fn from(value: Connector) -> Self {
        fsandbox::Connector { token: registry::insert_token(value.into()) }
    }
}

impl From<Connector> for fsandbox::Capability {
    fn from(connector: Connector) -> Self {
        fsandbox::Capability::Connector(connector.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::ClientEnd;
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;
    use vfs::directory::entry::OpenRequest;
    use vfs::execution_scope::ExecutionScope;
    use vfs::ToObjectRequest;

    // TODO(340891837): This test only runs on host because of the reliance on Open
    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_node_reference_and_describe() {
        let receiver = {
            let (receiver, sender) = Connector::new();
            let open: crate::DirEntry = sender.into();
            let (client, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>();
            const FLAGS: fio::Flags =
                fio::Flags::PROTOCOL_NODE.union(fio::Flags::FLAG_SEND_REPRESENTATION);
            FLAGS.to_object_request(server.into_channel()).handle(|request| {
                open.open_entry(OpenRequest::new(
                    ExecutionScope::new(),
                    FLAGS,
                    vfs::Path::dot(),
                    request,
                ))
            });

            // The NODE_REFERENCE connection should be terminated on the sender side.
            let result = client.take_event_stream().next().await.unwrap();
            assert_matches!(
                result,
                Ok(fio::NodeEvent::OnRepresentation { payload: fio::Representation::Node(_) })
            );

            receiver
        };

        // After closing the sender, the receiver should be done.
        assert_matches!(receiver.receive().await, None);
    }

    // TODO(340891837): This test only runs on host because of the reliance on Open
    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_empty() {
        let (receiver, sender) = Connector::new();
        let open: crate::DirEntry = sender.into();

        let (client_end, server_end) = Channel::create();
        // The VFS should not send any event, but directly hand us the channel.
        const FLAGS: fio::Flags = fio::Flags::PROTOCOL_SERVICE;
        FLAGS.to_object_request(server_end).handle(|request| {
            open.open_entry(OpenRequest::new(
                ExecutionScope::new(),
                FLAGS,
                vfs::Path::dot(),
                request,
            ))
        });
        // Check that we got the channel.
        assert_matches!(receiver.receive().await, Some(_));

        // Check that there's no event, as we should be connected to the other end of the capability
        // directly (i.e. this shouldn't actually be a fuchsia.io/Node connection).
        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy();
        assert_matches!(node.take_event_stream().next().await, None);
    }
}
