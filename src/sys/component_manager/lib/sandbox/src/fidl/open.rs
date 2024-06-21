// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::registry;
use crate::{ConversionError, Open, RemotableCapability};
use fidl_fuchsia_component_sandbox as fsandbox;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;

impl RemotableCapability for Open {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(self.entry)
    }
}

impl From<Open> for fsandbox::Open {
    fn from(value: Open) -> Self {
        fsandbox::Open { token: registry::insert_token(value.into()) }
    }
}

impl From<Open> for fsandbox::Capability {
    fn from(open: Open) -> Self {
        fsandbox::Capability::Open(open.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;
    use fidl::endpoints::{self, ServerEnd};
    use test_util::Counter;
    use vfs::directory::entry::{EntryInfo, OpenRequest};
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::remote::RemoteLike;
    use vfs::ToObjectRequest;
    use {fidl_fuchsia_io as fio, fuchsia_zircon as zx};

    struct MockDir(Counter);
    impl DirectoryEntry for MockDir {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }

        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
            request.open_remote(self)
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
    }

    #[fuchsia::test]
    async fn into_fidl() {
        let mock_dir = Arc::new(MockDir(Counter::new(0)));
        let open = Capability::Open(Open::new(mock_dir.clone()));

        // Round-trip to fidl and back. The fidl representation is just a token, so we need to
        // convert it back to internal to do anything useful with it.
        let cap = fsandbox::Capability::from(open);
        let cap = Capability::try_from(cap).unwrap();
        let Capability::Open(open) = cap else {
            panic!();
        };

        assert_eq!(mock_dir.0.get(), 0);
        let scope = ExecutionScope::new();
        let flags = fio::OpenFlags::DIRECTORY;
        let (_client, server) = endpoints::create_endpoints::<fio::DirectoryMarker>();
        let mut object_request = flags.to_object_request(server);
        let dir_entry = open.clone().try_into_directory_entry().unwrap();
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
