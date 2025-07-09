// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::connector::Connectable;
use crate::fidl::registry;
use crate::{Connector, ConversionError, DirEntry, RemotableCapability};
use fidl::handle::Status;
use std::sync::Arc;
use vfs::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
use vfs::execution_scope::ExecutionScope;
use vfs::ToObjectRequest;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

impl RemotableCapability for DirEntry {
    fn try_into_directory_entry(
        self,
        _scope: ExecutionScope,
    ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(self.to_directory_entry())
    }
}

impl From<DirEntry> for fsandbox::DirEntry {
    fn from(value: DirEntry) -> Self {
        fsandbox::DirEntry { token: registry::insert_token(value.into()) }
    }
}

impl From<DirEntry> for fsandbox::Capability {
    fn from(value: DirEntry) -> Self {
        fsandbox::Capability::DirEntry(value.into())
    }
}

impl Connectable for DirEntry {
    fn send(&self, message: crate::Message) -> Result<(), ()> {
        const FLAGS: fio::Flags = fio::Flags::PROTOCOL_SERVICE;
        FLAGS.to_object_request(message.channel).handle(|request| {
            self.open_entry(OpenRequest::new(
                ExecutionScope::new(),
                FLAGS,
                vfs::Path::dot(),
                request,
            ))
        });
        Ok(())
    }
}

impl GetEntryInfo for DirEntry {
    fn entry_info(&self) -> EntryInfo {
        self.entry.entry_info()
    }
}

impl DirectoryEntry for DirEntry {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        self.entry.clone().open_entry(request)
    }
}

impl DirEntry {
    /// Creates a [DirEntry] capability from a [vfs::directory::entry::DirectoryEntry].
    pub fn new(entry: Arc<dyn DirectoryEntry>) -> Self {
        Self { entry }
    }

    pub fn to_directory_entry(self) -> Arc<dyn DirectoryEntry> {
        self.entry
    }

    /// Opens the corresponding entry by forwarding `open_request`.
    pub fn open_entry(&self, open_request: OpenRequest<'_>) -> Result<(), Status> {
        self.entry.clone().open_entry(open_request)
    }

    pub fn dirent_type(&self) -> fio::DirentType {
        self.entry.entry_info().type_()
    }
}

impl From<Connector> for DirEntry {
    fn from(connector: Connector) -> Self {
        Self::new(vfs::service::endpoint(move |_scope, server_end| {
            let _ = connector.send_channel(server_end.into_zx_channel().into());
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;
    use fidl::endpoints::{self, ServerEnd};
    use fidl_fuchsia_io as fio;
    use test_util::Counter;
    use vfs::directory::entry::{EntryInfo, GetEntryInfo, OpenRequest};
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::remote::RemoteLike;
    use vfs::{ObjectRequestRef, ToObjectRequest};

    struct MockDir(Counter);
    impl DirectoryEntry for MockDir {
        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
            request.open_remote(self)
        }
    }
    impl GetEntryInfo for MockDir {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }
    }
    impl RemoteLike for MockDir {
        fn deprecated_open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _flags: fio::OpenFlags,
            _relative_path: Path,
            _server_end: ServerEnd<fio::NodeMarker>,
        ) {
            self.0.inc();
        }

        fn open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _relative_path: Path,
            _flags: fio::Flags,
            _object_request: ObjectRequestRef<'_>,
        ) -> Result<(), Status> {
            self.0.inc();
            Ok(())
        }
    }

    #[fuchsia::test]
    async fn into_fidl() {
        let mock_dir = Arc::new(MockDir(Counter::new(0)));
        let dir_entry = Capability::DirEntry(DirEntry::new(mock_dir.clone()));

        // Round-trip to fidl and back. The fidl representation is just a token, so we need to
        // convert it back to internal to do anything useful with it.
        let cap = fsandbox::Capability::from(dir_entry);
        let cap = Capability::try_from(cap).unwrap();
        let Capability::DirEntry(dir_entry) = cap else {
            panic!();
        };

        assert_eq!(mock_dir.0.get(), 0);
        let scope = ExecutionScope::new();
        const FLAGS: fio::Flags = fio::PERM_READABLE;
        let (_client, server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut object_request = FLAGS.to_object_request(server);
        let dir_entry = dir_entry.clone().try_into_directory_entry(scope.clone()).unwrap();
        dir_entry
            .open_entry(OpenRequest::new(scope.clone(), FLAGS, Path::dot(), &mut object_request))
            .unwrap();
        assert_eq!(mock_dir.0.get(), 1);
    }
}
