// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of a cgroup directory.
//!
//! Contains core cgroup interface files, resource controller interface files, and directories for
//! sub-cgroups.
//!
//! Full details at https://docs.kernel.org/admin-guide/cgroup-v2.html

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::{Arc, Weak};

use starnix_core::task::{CgroupOps, CgroupRoot, CurrentTask};
use starnix_core::vfs::{
    BytesFile, DirectoryEntryType, FileOps, FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, FsString, VecDirectory, VecDirectoryEntry,
};
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, mode};

use crate::events::EventsFile;
use crate::freeze::FreezeFile;
use crate::kill::KillFile;
use crate::procs::ControlGroupNode;
use crate::Hierarchy;

const CONTROLLERS_FILE: &str = "cgroup.controllers";
const PROCS_FILE: &str = "cgroup.procs";
const FREEZE_FILE: &str = "cgroup.freeze";
const EVENTS_FILE: &str = "cgroup.events";
const KILL_FILE: &str = "cgroup.kill";

#[derive(Debug)]
pub struct CgroupDirectory {
    /// The associated cgroup.
    cgroup: Weak<dyn CgroupOps>,

    /// Weak reference to the hierarchy.
    hierarchy: Weak<Hierarchy>,

    /// Interface files of the current cgroup directory. Files can be added or removed when resource
    /// controllers are enabled and disabled on the cgroup, respectively.
    interface_files: Mutex<BTreeMap<FsString, FsNodeHandle>>,
}

impl CgroupDirectory {
    /// 2-step initialization to create a new root directory. `FileSystem` is instantiated with
    /// the `Directory`, which is then used to create the interface files of the `Directory` using
    /// `create_root_interface_files()`.
    pub fn new_root(cgroup: Weak<CgroupRoot>, hierarchy: Weak<Hierarchy>) -> CgroupDirectoryHandle {
        CgroupDirectoryHandle(Arc::new(Self {
            cgroup: cgroup as Weak<dyn CgroupOps>,
            interface_files: Mutex::new(BTreeMap::new()),
            hierarchy,
        }))
    }

    /// Can only be called on a newly initialized root directory created by `new_root`, and can only
    /// be called once. Creates interface files for the root directory, which can only be done after
    /// the `FileSystem` is initialized.
    pub fn create_root_interface_files(&self, current_task: &CurrentTask, fs: &FileSystemHandle) {
        let mut interface_files = self.interface_files.lock();
        assert!(interface_files.is_empty(), "init is only called once");
        interface_files.insert(
            PROCS_FILE.into(),
            fs.create_node(
                current_task,
                ControlGroupNode::new(self.cgroup.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            ),
        );
        interface_files.insert(
            CONTROLLERS_FILE.into(),
            fs.create_node(
                current_task,
                BytesFile::new_node(b"".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
        );
    }

    /// Creates a new non-root directory, along with its core interface files.
    pub fn new(
        cgroup: Weak<dyn CgroupOps>,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        hierarchy: Weak<Hierarchy>,
    ) -> CgroupDirectoryHandle {
        let interface_files = BTreeMap::from([
            (
                PROCS_FILE.into(),
                fs.create_node(
                    current_task,
                    ControlGroupNode::new(cgroup.clone()),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
                ),
            ),
            (
                CONTROLLERS_FILE.into(),
                fs.create_node(
                    current_task,
                    BytesFile::new_node(b"".to_vec()),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                ),
            ),
            (
                FREEZE_FILE.into(),
                fs.create_node(
                    current_task,
                    FreezeFile::new_node(cgroup.clone()),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
                ),
            ),
            (
                EVENTS_FILE.into(),
                fs.create_node(
                    current_task,
                    EventsFile::new_node(cgroup.clone()),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                ),
            ),
            (
                KILL_FILE.into(),
                fs.create_node(
                    current_task,
                    KillFile::new_node(cgroup.clone()),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o200), FsCred::root()),
                ),
            ),
        ]);
        CgroupDirectoryHandle(Arc::new(Self {
            cgroup,
            interface_files: Mutex::new(interface_files),
            hierarchy,
        }))
    }

    fn cgroup(&self) -> Result<Arc<dyn CgroupOps>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }

    fn hierarchy(&self) -> Result<Arc<Hierarchy>, Errno> {
        self.hierarchy.upgrade().ok_or_else(|| errno!(ENODEV))
    }

    #[cfg(test)]
    pub fn has_interface_files(&self) -> bool {
        !self.interface_files.lock().is_empty()
    }
}

/// `CgroupDirectoryHandle` is needed to implement a starnix_core trait for an `Arc<CgroupDirectory>`.
#[derive(Debug, Clone)]
pub struct CgroupDirectoryHandle(Arc<CgroupDirectory>);

impl Deref for CgroupDirectoryHandle {
    type Target = CgroupDirectory;

    fn deref(&self) -> &Self::Target {
        &self.0.deref()
    }
}

impl FsNodeOps for CgroupDirectoryHandle {
    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let children = self.cgroup()?.get_children()?;
        let nodes = self.hierarchy()?.get_nodes(&children);
        assert_eq!(children.len(), nodes.len());

        let children_entries =
            children.into_iter().zip(nodes.into_iter()).filter_map(|(cgroup, maybe_node)| {
                // Ignore cgroups that hasn't been created or has been deleted from the hierarchy.
                let Some(node) = maybe_node else {
                    return None;
                };
                let ino = node.info().ino;
                Some(VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name: cgroup.name().into(),
                    inode: Some(ino),
                })
            });

        let interface_files = self.interface_files.lock();
        let interface_entries = interface_files.iter().map(|(name, child)| VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: name.clone(),
            inode: Some(child.info().ino),
        });

        Ok(VecDirectory::new_file(children_entries.chain(interface_entries).collect()))
    }

    fn mkdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let hierarchy = self.hierarchy()?;
        let cgroup = self.cgroup()?.new_child(name)?;
        let directory = CgroupDirectory::new(
            Arc::downgrade(&cgroup) as Weak<dyn CgroupOps>,
            current_task,
            &node.fs(),
            self.hierarchy.clone(),
        );
        let child = hierarchy.add_node(&cgroup, directory, current_task, &node.fs());

        node.update_info(|info| {
            info.link_count += 1;
        });

        Ok(child)
    }

    fn mknod(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EACCES)
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        // Only cgroup directories can be removed. Cgroup interface files cannot be removed.
        let Some(child_node) = child.downcast_ops::<CgroupDirectoryHandle>() else {
            return error!(EPERM);
        };

        let hierarchy = self.hierarchy()?;
        let child_cgroup = child_node.cgroup()?;
        let removed = self.cgroup()?.remove_child(name)?;
        assert!(Arc::ptr_eq(&(removed.clone() as Arc<dyn CgroupOps>), &child_cgroup));

        hierarchy.remove_node(&removed)?;

        node.update_info(|info| {
            info.link_count -= 1;
        });

        Ok(())
    }

    fn create_symlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EPERM)
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let interface_files = self.interface_files.lock();
        if let Some(node) = interface_files.get(name) {
            Ok(node.clone())
        } else {
            let cgroup = self.cgroup()?.get_child(name)?;
            self.hierarchy()?.get_node(&cgroup)
        }
    }
}
