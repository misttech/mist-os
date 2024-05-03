// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use core::fmt;
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_zircon::{self as zx, AsHandleRef};
use std::sync::Arc;
use vfs::{directory::entry::DirectoryEntry, remote::RemoteLike};

use crate::CapabilityTrait;

/// A capability that is a `fuchsia.io` directory.
///
/// The directory may optionally be backed by a future that serves its contents.
pub struct Directory {
    /// The FIDL representation of this [Directory].
    client_end: ClientEnd<fio::DirectoryMarker>,
}

impl Directory {
    /// Create a new [Directory] capability.
    ///
    /// Arguments:
    ///
    /// * `client_end` - A `fuchsia.io/Directory` client endpoint.
    pub fn new(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end }
    }

    /// Turn the [Directory] into a remote VFS node.
    pub(crate) fn into_remote(self) -> Arc<impl RemoteLike + DirectoryEntry> {
        let client_end = ClientEnd::<fio::DirectoryMarker>::from(self);
        vfs::remote::remote_dir(client_end.into_proxy().unwrap())
    }
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory").field("client_end", &self.client_end).finish()
    }
}

impl Clone for Directory {
    fn clone(&self) -> Self {
        // Call `fuchsia.io/Directory.Clone` without converting the ClientEnd into a proxy.
        // This is necessary because we the conversion consumes the ClientEnd, but we can't take
        // it out of non-mut `&self`.
        let (clone_client_end, clone_server_end) = zx::Channel::create();
        let raw_handle = self.client_end.as_handle_ref().raw_handle();
        // SAFETY: the channel is forgotten at the end of scope so it is not double closed.
        unsafe {
            let borrowed: zx::Channel = zx::Handle::from_raw(raw_handle).into();
            let directory = fio::DirectorySynchronousProxy::new(borrowed);
            let _ = directory.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, clone_server_end.into());
            std::mem::forget(directory.into_channel());
        }
        let client_end: ClientEnd<fio::DirectoryMarker> = clone_client_end.into();
        Self { client_end: client_end }
    }
}

impl CapabilityTrait for Directory {}

impl From<ClientEnd<fio::DirectoryMarker>> for Directory {
    fn from(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end }
    }
}

impl From<Directory> for ClientEnd<fio::DirectoryMarker> {
    /// Return a channel to the Directory and store the channel in
    /// the registry.
    fn from(directory: Directory) -> Self {
        let Directory { client_end } = directory;
        client_end
    }
}

impl From<Directory> for fsandbox::Capability {
    fn from(directory: Directory) -> Self {
        Self::Directory(directory.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Open;
    use fidl::endpoints::{create_endpoints, ServerEnd};
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;
    use futures::channel::mpsc;
    use futures::StreamExt;
    use vfs::{
        directory::{
            entry::{EntryInfo, OpenRequest},
            entry_container::Directory as VfsDirectory,
        },
        execution_scope::ExecutionScope,
        path::Path,
        pseudo_directory,
    };

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
                flags: fio::OpenFlags,
                relative_path: Path,
                _server_end: ServerEnd<fio::NodeMarker>,
            ) {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                self.0.clone().try_send(()).unwrap();
            }
        }

        let open = Open::new(Arc::new(MockDir(open_tx)));

        let fs = pseudo_directory! {
            "foo" => open.try_into_directory_entry().unwrap(),
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
