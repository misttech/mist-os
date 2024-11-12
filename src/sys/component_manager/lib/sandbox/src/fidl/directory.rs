// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{ConversionError, Directory, RemotableCapability};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_io as fio;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use vfs::execution_scope::ExecutionScope;
use vfs::remote::RemoteLike;

impl Directory {
    /// Turn the [Directory] into a remote VFS node.
    pub(crate) fn into_remote(self) -> Arc<impl RemoteLike + DirectoryEntry> {
        let client_end = ClientEnd::<fio::DirectoryMarker>::from(self);
        vfs::remote::remote_dir(client_end.into_proxy().unwrap())
    }
}

impl RemotableCapability for Directory {
    fn try_into_directory_entry(
        self,
        _scope: ExecutionScope,
    ) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(self.into_remote())
    }
}

// These tests only run on target because the vfs library is not generally available on host.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::DirEntry;
    use fidl::endpoints::{create_endpoints, ServerEnd};
    use fidl::handle::Status;
    use fidl_fuchsia_io as fio;
    use futures::channel::mpsc;
    use futures::StreamExt;
    use vfs::directory::entry::{EntryInfo, GetEntryInfo, OpenRequest};
    use vfs::directory::entry_container::Directory as VfsDirectory;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::pseudo_directory;

    fn serve_vfs_dir(root: Arc<impl VfsDirectory>) -> ClientEnd<fio::DirectoryMarker> {
        let scope = ExecutionScope::new();
        let (client, server) = create_endpoints::<fio::DirectoryMarker>();
        root.open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            vfs::path::Path::dot(),
            ServerEnd::new(server.into_channel()),
        );
        client
    }

    #[fuchsia::test]
    async fn test_clone() {
        let (open_tx, mut open_rx) = mpsc::channel::<()>(1);

        struct MockDir(mpsc::Sender<()>);
        impl DirectoryEntry for MockDir {
            fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
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
                flags: fio::OpenFlags,
                relative_path: Path,
                _server_end: ServerEnd<fio::NodeMarker>,
            ) {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                self.0.clone().try_send(()).unwrap();
            }
        }

        let dir_entry = DirEntry::new(Arc::new(MockDir(open_tx)));

        let scope = ExecutionScope::new();
        let fs = pseudo_directory! {
            "foo" => dir_entry.try_into_directory_entry(scope).unwrap(),
        };

        // Create a Directory capability, and a clone.
        let dir = Directory::from(serve_vfs_dir(fs));
        let dir_clone = dir.clone();

        // Open the original directory.
        let client_end: ClientEnd<fio::DirectoryMarker> = dir.into();
        let dir_proxy = client_end.into_proxy().unwrap();
        let (_client_end, server_end) = create_endpoints::<fio::NodeMarker>();
        dir_proxy
            .open(fio::OpenFlags::DIRECTORY, fio::ModeType::empty(), "foo", server_end)
            .unwrap();

        // The Open capability should receive the Open request.
        open_rx.next().await.unwrap();

        // Open the clone.
        let client_end: ClientEnd<fio::DirectoryMarker> = dir_clone.into();
        let clone_proxy = client_end.into_proxy().unwrap();
        let (_client_end, server_end) = create_endpoints::<fio::NodeMarker>();
        clone_proxy
            .open(fio::OpenFlags::DIRECTORY, fio::ModeType::empty(), "foo", server_end)
            .unwrap();

        // The Open capability should receive the Open request from the clone.
        open_rx.next().await.unwrap();
    }
}
