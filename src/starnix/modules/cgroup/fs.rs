// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsStr,
};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::errors::Errno;
use starnix_uapi::{statfs, CGROUP2_SUPER_MAGIC, CGROUP_SUPER_MAGIC};

use crate::cgroup::CgroupRoot;
use std::sync::Arc;

pub struct CgroupV1Fs {
    #[allow(dead_code)]
    // `root` is not accessed, but is needed to keep the cgroup hierarchy alive.
    root: Arc<CgroupRoot>,
}

impl CgroupV1Fs {
    pub fn new_fs(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let weak_kernel = Arc::downgrade(&kernel);
        let root = CgroupRoot::new(weak_kernel);
        let fs = FileSystem::new(
            &kernel,
            CacheMode::Uncached,
            CgroupV1Fs { root: root.clone() },
            options,
        )?;
        root.init(current_task, &fs);
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

pub struct CgroupV2Fs {
    // `root` is not accessed, but is needed to keep the cgroup hierarchy alive.
    pub root: Arc<CgroupRoot>,
}

impl CgroupV2Fs {
    pub fn new_fs(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let weak_kernel = Arc::downgrade(&kernel);
        let root = CgroupRoot::new(weak_kernel);
        let fs = FileSystem::new(
            &kernel,
            CacheMode::Uncached,
            CgroupV2Fs { root: root.clone() },
            options,
        )?;
        root.init(current_task, &fs);
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
