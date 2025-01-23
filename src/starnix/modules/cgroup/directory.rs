// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of a cgroup directory.
//!
//! Contains core cgroup interface files, resource controller interface files, and directories for
//! sub-cgroups.
//!
//! Full details at https://docs.kernel.org/admin-guide/cgroup-v2.html

use std::ops::Deref;
use std::sync::{Arc, Weak};

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileOps, FsNode, FsNodeHandle, FsNodeOps, FsStr, VecDirectory};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error};

use crate::cgroup::CgroupOps;

#[derive(Debug)]
pub struct CgroupDirectory {
    /// The associated cgroup.
    cgroup: Weak<dyn CgroupOps>,
}

impl CgroupDirectory {
    pub fn new(cgroup: Weak<dyn CgroupOps>) -> CgroupDirectoryHandle {
        CgroupDirectoryHandle(Arc::new(Self { cgroup }))
    }
}

/// `CgroupDirectoryHandle` is needed to implement a trait for an Arc.
#[derive(Debug, Clone)]
pub struct CgroupDirectoryHandle(Arc<CgroupDirectory>);
impl CgroupDirectoryHandle {
    fn cgroup(&self) -> Result<Arc<dyn CgroupOps>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

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
        Ok(VecDirectory::new_file(self.cgroup()?.get_entries()))
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
        let child = self.cgroup()?.new_child(current_task, &node.fs(), name)?;
        node.update_info(|info| {
            info.link_count += 1;
        });
        Ok(child.directory_node.clone())
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
        let cgroup = self.cgroup()?;

        // Only cgroup directories can be removed. Cgroup interface files cannot be removed.
        let Some(child_dir) = child.downcast_ops::<CgroupDirectoryHandle>() else {
            return error!(EPERM);
        };
        let child_cgroup = child_dir.cgroup()?;

        let removed = cgroup.remove_child(name)?;
        assert!(Arc::ptr_eq(&(removed as Arc<dyn CgroupOps>), &child_cgroup));

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
        self.cgroup()?.get_node(name)
    }
}
