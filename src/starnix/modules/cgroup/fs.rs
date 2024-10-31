// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsStr,
};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::errors::Errno;
use starnix_uapi::{statfs, CGROUP2_SUPER_MAGIC, CGROUP_SUPER_MAGIC};

use crate::cgroup::CgroupDirectoryNode;

pub struct CgroupV1Fs;
impl CgroupV1Fs {
    pub fn new_fs(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let fs = FileSystem::new(current_task.kernel(), CacheMode::Permanent, CgroupV1Fs, options)?;
        let root = FsNode::new_root(CgroupDirectoryNode::new());
        fs.set_root_node(root);
        Ok(fs)
    }
}
impl FileSystemOps for CgroupV1Fs {
    fn name(&self) -> &'static FsStr {
        b"cgroup".into()
    }
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(CGROUP_SUPER_MAGIC))
    }
}

pub struct CgroupV2Fs;
impl CgroupV2Fs {
    pub fn new_fs(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let fs = FileSystem::new(current_task.kernel(), CacheMode::Permanent, CgroupV2Fs, options)?;
        let root = FsNode::new_root(CgroupDirectoryNode::new());
        fs.set_root_node(root);
        Ok(fs)
    }
}

impl FileSystemOps for CgroupV2Fs {
    fn name(&self) -> &'static FsStr {
        b"cgroup2".into()
    }
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(CGROUP2_SUPER_MAGIC))
    }
}
