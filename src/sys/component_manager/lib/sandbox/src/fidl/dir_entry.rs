// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::connector::Connectable;
use crate::fidl::registry;
use crate::{Connector, ConversionError, DirEntry, RemotableCapability};
use fidl::handle::{Channel, Status};
use std::sync::Arc;
use vfs::directory::entry::{DirectoryEntry, OpenRequest};
use vfs::execution_scope::ExecutionScope;
use vfs::ToObjectRequest;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio};

impl RemotableCapability for DirEntry {
    fn try_into_directory_entry(
        self,
        _scope: ExecutionScope,
    ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(self.entry)
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
        self.open(
            ExecutionScope::new(),
            fio::OpenFlags::empty(),
            vfs::path::Path::dot(),
            message.channel,
        );
        Ok(())
    }
}

impl DirEntry {
    /// Creates a [DirEntry] capability from a [vfs::directory::entry::DirectoryEntry].
    pub fn new(entry: Arc<dyn DirectoryEntry>) -> Self {
        Self { entry }
    }

    /// Opens the corresponding entry.
    ///
    /// If `path` fails validation, the `server_end` will be closed with a corresponding
    /// epitaph and optionally an event.
    pub fn open(
        &self,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: impl ValidatePath,
        server_end: Channel,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            let path = path.validate()?;
            self.entry.clone().open_entry(OpenRequest::new(scope, flags, path, object_request))
        });
    }

    /// Forwards the open request.
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

pub trait ValidatePath {
    fn validate(self) -> Result<vfs::path::Path, Status>;
}

impl ValidatePath for vfs::path::Path {
    fn validate(self) -> Result<vfs::path::Path, Status> {
        Ok(self)
    }
}

impl ValidatePath for String {
    fn validate(self) -> Result<vfs::path::Path, Status> {
        vfs::path::Path::validate_and_split(self)
    }
}

impl ValidatePath for &str {
    fn validate(self) -> Result<vfs::path::Path, Status> {
        vfs::path::Path::validate_and_split(self)
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
        fn open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _flags: fio::OpenFlags,
            _relative_path: Path,
            _server_end: ServerEnd<fio::NodeMarker>,
        ) {
            self.0.inc();
        }

        fn open3(
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
        let flags = fio::OpenFlags::DIRECTORY;
        let (_client, server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut object_request = flags.to_object_request(server);
        let dir_entry = dir_entry.clone().try_into_directory_entry(scope.clone()).unwrap();
        dir_entry
            .open_entry(OpenRequest::new(
                scope.clone(),
                fio::OpenFlags::empty(),
                Path::dot(),
                &mut object_request,
            ))
            .unwrap();
        assert_eq!(mock_dir.0.get(), 1);
    }
}
