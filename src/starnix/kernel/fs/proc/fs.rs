// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::proc_directory::ProcDirectory;
use crate::task::{CurrentTask, Kernel};
use crate::vfs::{
    CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsStr,
};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::errors::Errno;
use starnix_uapi::{statfs, PROC_SUPER_MAGIC};
use std::sync::Arc;

struct ProcFsHandle(FileSystemHandle);

pub fn get_proc_fs(kernel: &Arc<Kernel>) -> Option<FileSystemHandle> {
    kernel.expando.peek::<ProcFsHandle>().map(|h| h.0.clone())
}

/// Returns `kernel`'s procfs instance, initializing it if needed.
pub fn proc_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    Ok(current_task
        .kernel()
        .expando
        .get_or_init(|| ProcFsHandle(ProcFs::new_fs(current_task, options)))
        .0
        .clone())
}

/// `ProcFs` is a filesystem that exposes runtime information about a `Kernel` instance.
struct ProcFs;
impl FileSystemOps for Arc<ProcFs> {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(PROC_SUPER_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "proc".into()
    }
}

impl ProcFs {
    /// Creates a new instance of `ProcFs` for the given `kernel`.
    pub fn new_fs(current_task: &CurrentTask, options: FileSystemOptions) -> FileSystemHandle {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(&kernel, CacheMode::Uncached, Arc::new(ProcFs), options)
            .expect("procfs constructed with valid options");
        fs.set_root(ProcDirectory::new(current_task, &fs));
        fs
    }
}
