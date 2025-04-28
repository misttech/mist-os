// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{KObject, KObjectHandle};
use crate::task::CurrentTask;
use crate::vfs::{
    fs_node_impl_dir_readonly, DirectoryEntryType, FileOps, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, VecDirectory, VecDirectoryEntry,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Weak;

pub struct KObjectDirectory {
    kobject: Weak<KObject>,
}

impl KObjectDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }

    fn kobject(&self) -> KObjectHandle {
        self.kobject.upgrade().expect("Weak references to kobject must always be valid")
    }

    pub fn create_file_ops_entries(&self) -> Vec<VecDirectoryEntry> {
        self.kobject()
            .get_children_names()
            .into_iter()
            .map(|name| VecDirectoryEntry {
                entry_type: DirectoryEntryType::DIR,
                name,
                inode: None,
            })
            .collect()
    }
}

impl FsNodeOps for KObjectDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(self.create_file_ops_entries()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match self.kobject().get_child(name) {
            Some(child_kobject) => Ok(node.fs().create_node(
                current_task,
                child_kobject.ops(),
                FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
            )),
            None => error!(ENOENT),
        }
    }
}
