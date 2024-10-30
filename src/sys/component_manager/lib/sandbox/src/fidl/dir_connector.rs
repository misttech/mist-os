// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::registry;
use crate::{ConversionError, DirConnector, DirReceiver};
use fidl::endpoints::ClientEnd;
use futures::channel::mpsc;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

impl DirConnector {
    pub(crate) fn new_with_owned_receiver(
        receiver_client: ClientEnd<fsandbox::DirReceiverMarker>,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        let receiver = DirReceiver::new(receiver);
        let receiver_task =
            fasync::Task::spawn(receiver.handle_receiver(receiver_client.into_proxy().unwrap()));
        Self::new_internal(sender, Some(Arc::new(receiver_task)))
    }
}

impl crate::RemotableCapability for DirConnector {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Err(ConversionError::NotSupported)
    }
}

impl From<DirConnector> for fsandbox::DirConnector {
    fn from(value: DirConnector) -> Self {
        fsandbox::DirConnector { token: registry::insert_token(value.into()) }
    }
}

impl From<DirConnector> for fsandbox::Capability {
    fn from(connector: DirConnector) -> Self {
        fsandbox::Capability::DirConnector(connector.into())
    }
}
