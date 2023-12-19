// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::kobject::{KObject, KObjectHandle},
    fs::sysfs::SysfsOps,
    task::CurrentTask,
    vfs::{
        fs_node_impl_dir_readonly, DirectoryEntryType, FileOps, FsNode, FsNodeHandle, FsNodeInfo,
        FsNodeOps, FsStr, VecDirectory, VecDirectoryEntry,
    },
};
use starnix_uapi::{auth::FsCred, error, errors::Errno, file_mode::mode, open_flags::OpenFlags};
use std::sync::Weak;

pub struct SysfsDirectory {
    kobject: Weak<KObject>,
}

impl SysfsDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }
}

impl SysfsOps for SysfsDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.kobject.upgrade().expect("Weak references to kobject must always be valid")
    }
}

impl FsNodeOps for SysfsDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.kobject()
                .get_children_names()
                .into_iter()
                .map(|name| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name,
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
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
