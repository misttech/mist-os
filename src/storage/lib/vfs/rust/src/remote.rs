// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A node which forwards open requests to a remote fuchsia.io server.

#[cfg(test)]
mod tests;

use crate::directory::entry::{DirectoryEntry, EntryInfo, GetEntryInfo, OpenRequest};
use crate::execution_scope::ExecutionScope;
use crate::path::Path;
use crate::{ObjectRequestRef, ToObjectRequest as _};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use std::sync::Arc;
use zx_status::Status;

pub trait RemoteLike {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    );

    fn open3(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status>;

    /// Returns whether the remote should be opened lazily for the given path.  If true, the remote
    /// won't be opened until the channel in the request is readable.  This request will *not* be
    /// considered lazy if the request requires an event such as OnRepresentation, and this method
    /// will by bypassed.
    fn lazy(&self, _path: &Path) -> bool {
        false
    }
}

/// Create a new [`Remote`] node that forwards open requests to the provided [`DirectoryProxy`],
/// effectively handing off the handling of any further requests to the remote fidl server.
pub fn remote_dir(dir: fio::DirectoryProxy) -> Arc<impl DirectoryEntry + RemoteLike> {
    Arc::new(RemoteDir { dir })
}

/// [`RemoteDir`] implements [`RemoteLike`]` which forwards open/open2 requests to a remote
/// directory.
struct RemoteDir {
    dir: fio::DirectoryProxy,
}

impl GetRemoteDir for RemoteDir {
    fn get_remote_dir(&self) -> Result<fio::DirectoryProxy, Status> {
        Ok(Clone::clone(&self.dir))
    }
}

/// A trait that can be implemented to return a directory proxy that should be used as a remote
/// directory.
pub trait GetRemoteDir {
    fn get_remote_dir(&self) -> Result<fio::DirectoryProxy, Status>;
}

impl<T: GetRemoteDir + Send + Sync + 'static> GetEntryInfo for T {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

impl<T: GetRemoteDir + Send + Sync + 'static> DirectoryEntry for T {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_remote(self)
    }
}

impl<T: GetRemoteDir> RemoteLike for T {
    fn open(
        self: Arc<Self>,
        _scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            let _ = self.get_remote_dir()?.open(
                flags,
                fio::ModeType::empty(),
                path.as_ref(),
                object_request.take().into_server_end(),
            );
            Ok(())
        });
    }

    fn open3(
        self: Arc<Self>,
        _scope: ExecutionScope,
        path: Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        // There is nowhere to propagate any errors since we take the `object_request`. This is okay
        // as the channel will be dropped and closed if the wire call fails.
        let _ = self.get_remote_dir()?.open3(
            path.as_ref(),
            flags,
            &object_request.options(),
            object_request.take().into_channel(),
        );
        Ok(())
    }
}
