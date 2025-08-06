// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{CapabilityBound, DirReceiver};
use cm_types::RelativePath;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use futures::channel::mpsc;
use std::fmt::Debug;
use std::sync::Arc;

/// Types that implement [`DirConnectable`] let the holder send directory channels
/// to them.
pub trait DirConnectable: Send + Sync + Debug {
    // TODO: not all implementers use all parameters in this method. This
    // suggests the API is wrong and should be revised.
    fn send(
        &self,
        dir: ServerEnd<fio::DirectoryMarker>,
        subdir: RelativePath,
        rights: Option<fio::Operations>,
    ) -> Result<(), ()>;
}

impl DirConnectable for mpsc::UnboundedSender<ServerEnd<fio::DirectoryMarker>> {
    fn send(
        &self,
        dir: ServerEnd<fio::DirectoryMarker>,
        subdir: RelativePath,
        rights: Option<fio::Operations>,
    ) -> Result<(), ()> {
        assert_eq!(subdir, RelativePath::dot());
        assert_eq!(rights, None);
        self.unbounded_send(dir).map_err(|_| ())
    }
}

/// A capability to obtain a channel to a [fuchsia.io/Directory]. As the name suggests, this is
/// similar to [Connector], except the channel type is always [fuchsia.io/Directory], and vfs
/// nodes that wrap this capability should have the `DIRECTORY` entry_type.
#[derive(Debug, Clone)]
pub struct DirConnector {
    inner: Arc<dyn DirConnectable>,
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

    pub fn from_proxy(proxy: fio::DirectoryProxy, subdir: RelativePath, flags: fio::Flags) -> Self {
        Self::new_sendable(DirectoryProxyForwarder { proxy, subdir, flags })
    }

    pub fn new_sendable(connector: impl DirConnectable + 'static) -> Self {
        Self { inner: Arc::new(connector) }
    }

    pub fn send(
        &self,
        dir: ServerEnd<fio::DirectoryMarker>,
        subdir: RelativePath,
        rights: Option<fio::Operations>,
    ) -> Result<(), ()> {
        self.inner.send(dir, subdir, rights)
    }
}

impl DirConnectable for DirConnector {
    fn send(
        &self,
        channel: ServerEnd<fio::DirectoryMarker>,
        subdir: RelativePath,
        rights: Option<fio::Operations>,
    ) -> Result<(), ()> {
        self.inner.send(channel, subdir, rights)
    }
}

#[derive(Debug)]
struct DirectoryProxyForwarder {
    proxy: fio::DirectoryProxy,
    subdir: RelativePath,
    flags: fio::Flags,
}

impl DirConnectable for DirectoryProxyForwarder {
    fn send(
        &self,
        server_end: ServerEnd<fio::DirectoryMarker>,
        subdir: RelativePath,
        rights: Option<fio::Operations>,
    ) -> Result<(), ()> {
        let flags = if let Some(rights) = rights {
            fio::Flags::from_bits(rights.bits()).ok_or(())?
        } else {
            self.flags | fio::Flags::PROTOCOL_DIRECTORY
        };
        let mut combined_subdir = self.subdir.clone();
        let success = combined_subdir.extend(subdir);
        if !success {
            // The requested path is too long.
            return Err(());
        }
        self.proxy
            .open(
                &format!("{}", combined_subdir),
                flags,
                &fio::Options::default(),
                server_end.into_channel(),
            )
            .map_err(|_| ())
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
        sender.send(ch2, RelativePath::dot(), None).unwrap();

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
        connector_clone.send(ch2, RelativePath::dot(), None).unwrap();

        // The Receiver should receive two channels, one from each connector.
        for _ in 0..2 {
            let _ch = receiver.receive().await.unwrap();
        }
    }
}
