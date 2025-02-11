// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    fs_node_impl_symlink, FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps,
    SymlinkTarget,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::mode;

pub fn thread_self_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    ThreadSelfSymlink::new_node(current_task, fs)
}

/// A node that represents a symlink to `proc/<pid>/task/<tid>` where <pid> and <tid> are derived
/// from the task reading the symlink.
struct ThreadSelfSymlink;

impl ThreadSelfSymlink {
    pub fn new_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
        fs.create_node(
            current_task,
            Self,
            FsNodeInfo::new_factory(mode!(IFLNK, 0o777), FsCred::root()),
        )
    }
}

impl FsNodeOps for ThreadSelfSymlink {
    fs_node_impl_symlink!();

    fn readlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path(
            format!("{}/task/{}", current_task.get_pid(), current_task.get_tid()).into(),
        ))
    }
}
