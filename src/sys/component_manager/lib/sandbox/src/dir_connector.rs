// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{CapabilityBound, DirReceiver};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use futures::channel::mpsc;
use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

/// Types that implement [`DirConnectable`] let the holder send directory channels
/// to them.
pub trait DirConnectable: Send + Sync + Debug {
    fn send(&self, dir: ServerEnd<fio::DirectoryMarker>) -> Result<(), ()>;
}

impl DirConnectable for mpsc::UnboundedSender<ServerEnd<fio::DirectoryMarker>> {
    fn send(&self, dir: ServerEnd<fio::DirectoryMarker>) -> Result<(), ()> {
        self.unbounded_send(dir).map_err(|_| ())
    }
}

/// A capability to obtain a channel to a [fuchsia.io/Directory]. As the name suggests, this is
/// similar to [Connector], except the channel type is always [fuchsia.io/Directory], and vfs
/// nodes that wrap this capability should have the `DIRECTORY` entry_type.
#[derive(Debug, Clone)]
pub struct DirConnector {
    inner: Arc<dyn DirConnectable>,

    // This exists to keep the receiver server task alive as long as any clone of this Connector is
    // alive. This is set when creating a Connector through CapabilityStore. The inner type is
    // `fuchsia_async::Task` but libsandbox can't depend on fuchsia-async so we type-erase it to
    // Any.
    _receiver_task: Option<Arc<dyn Any + Send + Sync>>,
}

impl CapabilityBound for DirConnector {
    fn debug_typename() -> &'static str {
        "DirConnector"
    }
}

impl DirConnector {
    pub fn new() -> (DirReceiver, Self) {
        let (sender, receiver) = mpsc::unbounded();
        let receiver = DirReceiver::new(receiver);
        let this = Self::new_sendable(sender);
        (receiver, this)
    }

    pub fn new_sendable(connector: impl DirConnectable + 'static) -> Self {
        Self::new_internal(connector, None)
    }

    pub(crate) fn new_internal(
        connector: impl DirConnectable + 'static,
        receiver_task: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Self {
        Self { inner: Arc::new(connector), _receiver_task: receiver_task }
    }

    pub fn send(&self, dir: ServerEnd<fio::DirectoryMarker>) -> Result<(), ()> {
        self.inner.send(dir)
    }
}

impl DirConnectable for DirConnector {
    fn send(&self, channel: ServerEnd<fio::DirectoryMarker>) -> Result<(), ()> {
        self.inner.send(channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints;
    use fidl::handle::{HandleBased, Rights};
    use fidl_fuchsia_component_sandbox as fsandbox;

    // NOTE: sending-and-receiving tests are written in `receiver.rs`.

    /// Tests that a DirConnector can be cloned by cloning its FIDL token.
    /// and capabilities sent to the original and clone arrive at the same Receiver.
    #[fuchsia::test]
    async fn fidl_clone() {
        let (receiver, sender) = DirConnector::new();

        // Send a channel through the DirConnector.
        let (_ch1, ch2) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        sender.send(ch2).unwrap();

        // Convert the Sender to a FIDL token.
        let connector: fsandbox::DirConnector = sender.into();

        // Clone the Sender by cloning the token.
        let token_clone = fsandbox::DirConnector {
            token: connector.token.duplicate_handle(Rights::SAME_RIGHTS).unwrap(),
        };
        let connector_clone =
            match crate::Capability::try_from(fsandbox::Capability::DirConnector(token_clone))
                .unwrap()
            {
                crate::Capability::DirConnector(connector) => connector,
                capability @ _ => panic!("wrong type {capability:?}"),
            };

        // Send a channel through the cloned Sender.
        let (_ch1, ch2) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        connector_clone.send(ch2).unwrap();

        // The Receiver should receive two channels, one from each connector.
        for _ in 0..2 {
            let _ch = receiver.receive().await.unwrap();
        }
    }
}
