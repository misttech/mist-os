// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{KObject, KObjectHandle};
use crate::fs::sysfs::sysfs_create_link;
use crate::task::CurrentTask;
use crate::vfs::{
    fs_node_impl_dir_readonly, DirectoryEntryType, FileOps, FsNode, FsNodeHandle, FsNodeOps, FsStr,
    VecDirectory, VecDirectoryEntry,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Weak;

pub struct KObjectSymlinkDirectory {
    kobject: Weak<KObject>,
}

impl KObjectSymlinkDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }

    fn kobject(&self) -> KObjectHandle {
        self.kobject.upgrade().expect("Weak references to kobject must always be valid")
    }
}

impl FsNodeOps for KObjectSymlinkDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.kobject()
                .get_children_names()
                .into_iter()
                .map(|name| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::LNK,
                    name,
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let kobject = self.kobject();
        match kobject.get_child(name) {
            Some(child_kobject) => {
                let (link, info) = sysfs_create_link(kobject, child_kobject, FsCred::root());
                Ok(node.fs().create_node(current_task, link, info))
            }
            None => error!(ENOENT),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::device::kobject::KObject;
    use crate::fs::sysfs::{KObjectDirectory, KObjectSymlinkDirectory};
    use crate::task::CurrentTask;
    use crate::testing::{create_fs, create_kernel_task_and_unlocked};
    use crate::vfs::{FileSystemHandle, FsStr, LookupContext, NamespaceNode, SymlinkMode};
    use starnix_sync::{Locked, Unlocked};
    use starnix_uapi::errors::Errno;
    use std::sync::Arc;

    fn lookup_node(
        locked: &mut Locked<'_, Unlocked>,
        task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<NamespaceNode, Errno> {
        let root = NamespaceNode::new_anonymous(fs.root().clone());
        task.lookup_path(locked, &mut LookupContext::new(SymlinkMode::NoFollow), root, name)
    }

    #[::fuchsia::test]
    async fn kobject_symlink_directory_contains_device_links() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_kobject = KObject::new_root(Default::default());
        root_kobject.get_or_create_child("0".into(), KObjectDirectory::new);
        root_kobject.get_or_create_child("0".into(), KObjectDirectory::new);
        let test_fs =
            create_fs(&kernel, KObjectSymlinkDirectory::new(Arc::downgrade(&root_kobject)));

        let device_entry = lookup_node(&mut locked, &current_task, &test_fs, "0".into())
            .expect("device 0 directory");
        assert!(device_entry.entry.node.is_lnk());
    }
}
