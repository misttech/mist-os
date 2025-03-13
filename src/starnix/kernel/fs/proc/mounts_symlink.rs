// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{fs_node_impl_symlink, FsNode, FsNodeOps, SymlinkTarget};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errors::Errno;

/// A node that represents a link to `self/mounts`.
pub struct MountsSymlink;

impl MountsSymlink {
    pub fn new_node() -> impl FsNodeOps {
        Self {}
    }
}

impl FsNodeOps for MountsSymlink {
    fs_node_impl_symlink!();

    fn readlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path("self/mounts".into()))
    }
}
