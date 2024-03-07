// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A node which forwards open requests to a remote fuchsia.io server.

#[cfg(test)]
mod tests;

use {
    crate::{
        common::send_on_open_with_error,
        directory::entry::{DirectoryEntry, EntryInfo, OpenRequest},
        execution_scope::ExecutionScope,
        path::Path,
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
}

// TODO(b/325540563): consider adding `remote_lazy_dir` which takes a generic function that
// returns the directory proxy. This will be useful, for example, in session_manager.

/// Create a new [`Remote`] node that forwards open requests to the provided [`DirectoryProxy`],
/// effectively handing off the handling of any further requests to the remote fidl server.
pub fn remote_dir(dir: fio::DirectoryProxy) -> Arc<impl DirectoryEntry + RemoteLike> {
    Arc::new(RemoteDir { dir })
}

/// [`RemoteDir`] implements [`RemoteLike`]` which forwards open/open2 requests to a remote
/// directory.
pub struct RemoteDir {
    dir: fio::DirectoryProxy,
}

impl DirectoryEntry for RemoteDir {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_remote(self)
    }
}

impl RemoteLike for RemoteDir {
    fn open(
        self: Arc<Self>,
        _scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let _ = self.dir.open(flags, fio::ModeType::empty(), path.as_ref(), server_end);
    }
}
