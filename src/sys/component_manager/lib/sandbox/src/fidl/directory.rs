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
mod tests {}
