// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::registry;
use crate::{ConversionError, DirConnector, DirReceiver};
use cm_types::{Name, RelativePath};
use fidl::endpoints::ClientEnd;
use futures::channel::mpsc;
use std::sync::Arc;
use vfs::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
use vfs::execution_scope::ExecutionScope;
use vfs::object_request::ObjectRequestRef;
use vfs::remote::RemoteLike;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_async as fasync};

impl DirConnector {
    pub(crate) fn new_with_fidl_receiver(
        receiver_client: ClientEnd<fsandbox::DirReceiverMarker>,
        scope: &fasync::Scope,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        let receiver = DirReceiver::new(receiver);
        // Exits when ServerEnd<DirReceiver> is closed
        scope.spawn(receiver.handle_receiver(receiver_client.into_proxy()));
        Self::new_sendable(sender)
    }
}

impl RemoteLike for DirConnector {
    fn open(
        self: Arc<Self>,
        _scope: ExecutionScope,
        mut path: vfs::path::Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        let mut relative_path = RelativePath::dot();
        while let Some(segment) = path.next() {
            let name = Name::new(segment).map_err(|_e|
                // The VFS path isn't valid according to RelativePath.
                zx::Status::INVALID_ARGS)?;
            let success = relative_path.push(name);
            if !success {
                // The path is too long
                return Err(zx::Status::INVALID_ARGS);
            }
        }
        let operations = fio::Operations::from_bits_retain(flags.bits());
        self.send(object_request.take().into_server_end(), relative_path, Some(operations))
            .map_err(|_| zx::Status::INTERNAL)
    }
}

impl DirectoryEntry for DirConnector {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.open_remote(self)
    }
}

impl GetEntryInfo for DirConnector {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

impl crate::RemotableCapability for DirConnector {
    fn try_into_directory_entry(
        self,
        _scope: ExecutionScope,
    ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(Arc::new(self))
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
