// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::kobject::{KObject, KObjectHandle},
    fs::sysfs::{sysfs_create_link, SysfsOps},
    task::CurrentTask,
    vfs::{
        fs_node_impl_dir_readonly, DirectoryEntryType, FileOps, FsNode, FsNodeHandle, FsNodeInfo,
        FsNodeOps, FsStr, VecDirectory, VecDirectoryEntry,
    },
};
use starnix_sync::{Locked, ReadOps};
use starnix_uapi::{auth::FsCred, error, errors::Errno, file_mode::mode, open_flags::OpenFlags};
use std::sync::Weak;

pub struct ClassCollectionDirectory {
    kobject: Weak<KObject>,
}

impl ClassCollectionDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }
}

impl SysfsOps for ClassCollectionDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.kobject.upgrade().expect("Weak references to kobject must always be valid")
    }
}

impl FsNodeOps for ClassCollectionDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, ReadOps>,
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
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let kobject = self.kobject();
        match kobject.get_child(name) {
            Some(child_kobject) => Ok(node.fs().create_node(
                current_task,
                sysfs_create_link(kobject.clone(), child_kobject),
                FsNodeInfo::new_factory(mode!(IFLNK, 0o777), FsCred::root()),
            )),
            None => error!(ENOENT),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        device::kobject::KObject,
        fs::sysfs::{ClassCollectionDirectory, SysfsDirectory},
        task::CurrentTask,
        testing::{create_fs, create_kernel_and_task},
        vfs::{FileSystemHandle, FsStr, LookupContext, NamespaceNode, SymlinkMode},
    };
    use starnix_uapi::errors::Errno;
    use std::sync::Arc;

    fn lookup_node(
        task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<NamespaceNode, Errno> {
        let root = NamespaceNode::new_anonymous(fs.root().clone());
        task.lookup_path(&mut LookupContext::new(SymlinkMode::NoFollow), root, name)
    }

    #[::fuchsia::test]
    async fn class_collection_directory_contains_device_links() {
        let (kernel, current_task) = create_kernel_and_task();
        let root_kobject = KObject::new_root(Default::default());
        root_kobject.get_or_create_child("0".into(), SysfsDirectory::new);
        root_kobject.get_or_create_child("0".into(), SysfsDirectory::new);
        let test_fs =
            create_fs(&kernel, ClassCollectionDirectory::new(Arc::downgrade(&root_kobject)));

        let device_entry =
            lookup_node(&current_task, &test_fs, "0".into()).expect("device 0 directory");
        assert!(device_entry.entry.node.is_lnk());
    }
}
