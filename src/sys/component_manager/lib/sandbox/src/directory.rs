// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use core::fmt;
use fidl::endpoints::{create_endpoints, ClientEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use std::sync::Arc;
use vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, remote::RemoteLike};

use crate::{registry, CapabilityTrait, Open};

/// A capability that is a `fuchsia.io` directory.
///
/// The directory may optionally be backed by a future that serves its contents.
pub struct Directory {
    /// The FIDL representation of this [Directory].
    ///
    /// Invariant: Always Some when the Directory is outside of the registry.
    client_end: Option<ClientEnd<fio::DirectoryMarker>>,
}

impl Directory {
    /// Create a new [Directory] capability.
    ///
    /// Arguments:
    ///
    /// * `client_end` - A `fuchsia.io/Directory` client endpoint.
    pub fn new(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end: Some(client_end) }
    }

    /// Create a new [Directory] capability that will open entries using the [Open] capability.
    ///
    /// Arguments:
    ///
    /// * `open_flags` - The flags that will be used to open a new connection from the [Open]
    ///   capability.
    pub fn from_open(open: Open, open_flags: fio::OpenFlags, scope: ExecutionScope) -> Self {
        let (client_end, server_end) = create_endpoints::<fio::DirectoryMarker>();
        scope.clone().spawn(async move {
            // Wait for the client endpoint to be written or closed. These are the only two
            // operations the client could do that warrants our attention.
            let server_end = fasync::Channel::from_channel(server_end.into_channel());
            let on_signal_fut = fasync::OnSignals::new(
                &server_end,
                zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
            );
            let signals = on_signal_fut.await.unwrap();
            if signals & zx::Signals::CHANNEL_READABLE != zx::Signals::NONE {
                open.open(
                    scope.clone(),
                    open_flags,
                    vfs::path::Path::dot(),
                    server_end.into_zx_channel().into(),
                );
            }
        });
        Self::new(client_end)
    }

    /// Sets this directory's client end to the provided one.
    ///
    /// This should only be used to put a remoted client end back into the Directory
    /// after it is removed from the registry.
    pub(crate) fn set_client_end(&mut self, client_end: ClientEnd<fio::DirectoryMarker>) {
        self.client_end = Some(client_end)
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
        let raw_handle = self.client_end.as_ref().unwrap().as_handle_ref().raw_handle();
        // SAFETY: the channel is forgotten at the end of scope so it is not double closed.
        unsafe {
            let borrowed: zx::Channel = zx::Handle::from_raw(raw_handle).into();
            let directory = fio::DirectorySynchronousProxy::new(borrowed);
            let _ = directory.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, clone_server_end.into());
            std::mem::forget(directory.into_channel());
        }
        let client_end: ClientEnd<fio::DirectoryMarker> = clone_client_end.into();
        Self { client_end: Some(client_end) }
    }
}

impl CapabilityTrait for Directory {}

impl From<ClientEnd<fio::DirectoryMarker>> for Directory {
    fn from(client_end: ClientEnd<fio::DirectoryMarker>) -> Self {
        Directory { client_end: Some(client_end) }
    }
}

impl From<Directory> for ClientEnd<fio::DirectoryMarker> {
    /// Returns the `fuchsia.io.Directory` client stored in this Directory, taking it out,
    /// and moves the capability into the registry.
    ///
    /// The client end is put back when the Directory is removed from the registry.
    fn from(mut directory: Directory) -> Self {
        let client_end = directory.client_end.take().expect("BUG: missing client end");

        // Move this capability into the registry.
        let koid = client_end.get_koid().unwrap();
        registry::insert(directory.into(), koid);

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
    use fidl::endpoints::ServerEnd;
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;
    use futures::channel::mpsc;
    use futures::StreamExt;
    use vfs::{
        directory::{
            entry::{EntryInfo, OpenRequest},
            entry_container::Directory as VfsDirectory,
        },
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
