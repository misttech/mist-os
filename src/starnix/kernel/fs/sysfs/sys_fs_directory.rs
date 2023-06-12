// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    auth::FsCred,
    fs::{
        kobject::{KObject, KObjectHandle},
        sysfs::SysFsOps,
        *,
    },
    task::CurrentTask,
    types::*,
};

use std::sync::Weak;

pub struct SysFsDirectory {
    kobject: Weak<KObject>,
}

impl SysFsDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }
}

impl SysFsOps for SysFsDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.kobject.clone().upgrade().expect("Weak references to kobject must always be valid")
    }
}

impl FsNodeOps for SysFsDirectory {
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
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match self.kobject().get_child(name) {
            Some(child_kobject) => Ok(node.fs().create_node_box(
                child_kobject.ops(),
                FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
            )),
            None => error!(ENOENT),
        }
    }
}
