// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A node which forwards open requests to a remote fuchsia.io server.

#[cfg(test)]
mod tests;

use {
    crate::{
        directory::entry::{DirectoryEntry, EntryInfo, OpenRequest},
        execution_scope::ExecutionScope,
        path::Path,
        ObjectRequestRef, ToObjectRequest,
    },
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    std::sync::Arc,
};

pub trait RemoteLike {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    );

    fn open2(
        self: Arc<Self>,
        _scope: ExecutionScope,
        _path: Path,
        _protocols: fio::ConnectionProtocols,
        _object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

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

impl<T: GetRemoteDir + Send + Sync + 'static> DirectoryEntry for T {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }

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

    // TODO(https://fxbug.dev/293947862): The Open2 method implies that `object_request` should be
    // closed with an epitaph on error, but this is not possible with the signature of Open2. We
    // could duplicate the channel pre-emptively, but this would incur an additional syscall for
    // every open2 call regardless of success or failure.
    //
    // We should document that the object_request might not be closed with an epitaph in some cases.
    // Specifically, where we fail to connect to a remote resource, or the server terminates before
    // the request can be completed.
    fn open2(
        self: Arc<Self>,
        _scope: ExecutionScope,
        path: Path,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        // There is nowhere to propagate any errors since we take the `object_request`. This is okay
        // as the channel will be dropped and closed if the wire call fails.
        let _ = self.get_remote_dir()?.open2(
            path.as_ref(),
            &protocols,
            object_request.take().into_channel(),
        );
        Ok(())
    }
}
