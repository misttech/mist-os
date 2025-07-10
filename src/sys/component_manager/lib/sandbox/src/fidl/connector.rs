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

    pub(crate) fn new_with_owned_receiver(
        receiver_client: ClientEnd<fsandbox::ReceiverMarker>,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        let receiver = Receiver::new(receiver);
        let receiver_task =
            fasync::Task::spawn(receiver.handle_receiver(receiver_client.into_proxy()));
        Self::new_internal(sender, Some(Arc::new(receiver_task)))
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
    use fidl::handle::Status;
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;
    use vfs::execution_scope::ExecutionScope;

    // TODO(340891837): This test only runs on host because of the reliance on Open
    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_node_reference_and_describe() {
        let receiver = {
            let (receiver, sender) = Connector::new();
            let open: crate::DirEntry = sender.into();
            let (client_end, server_end) = Channel::create();
            let scope = ExecutionScope::new();
            open.open(
                scope,
                fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE,
                ".",
                server_end,
            );

            // The NODE_REFERENCE connection should be terminated on the sender side.
            let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
            let node: fio::NodeProxy = client_end.into_proxy();
            let result = node.take_event_stream().next().await.unwrap();
            assert_matches!(
                result,
                Ok(fio::NodeEvent::OnOpen_ { s, info })
                    if s == Status::OK.into_raw()
                    && *info.as_ref().unwrap().as_ref() == fio::NodeInfoDeprecated::Service(fio::Service {})
            );

            receiver
        };

        // After closing the sender, the receiver should be done.
        assert_matches!(receiver.receive().await, None);
    }

    // TODO(340891837): This test only runs on host because of the reliance on Open
    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_describe() {
        let (receiver, sender) = Connector::new();
        let open: crate::DirEntry = sender.into();

        let (client_end, server_end) = Channel::create();
        // The VFS should send the DESCRIBE event, then hand us the channel.
        open.open(ExecutionScope::new(), fio::OpenFlags::DESCRIBE, ".", server_end);

        // Check we got the channel.
        assert_matches!(receiver.receive().await, Some(_));

        // Check the client got describe.
        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy();
        let result = node.take_event_stream().next().await.unwrap();
        assert_matches!(
            result,
            Ok(fio::NodeEvent::OnOpen_ { s, info })
            if s == Status::OK.into_raw()
            && *info.as_ref().unwrap().as_ref() == fio::NodeInfoDeprecated::Service(fio::Service {})
        );
    }

    // TODO(340891837): This test only runs on host because of the reliance on Open
    #[fuchsia::test]
    async fn unwrap_server_end_or_serve_node_empty() {
        let (receiver, sender) = Connector::new();
        let open: crate::DirEntry = sender.into();

        let (client_end, server_end) = Channel::create();
        // The VFS should not send any event, but directly hand us the channel.
        open.open(ExecutionScope::new(), fio::OpenFlags::empty(), ".", server_end);

        // Check that we got the channel.
        assert_matches!(receiver.receive().await, Some(_));

        // Check that there's no event.
        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy();
        assert_matches!(node.take_event_stream().next().await, None);
    }
}
