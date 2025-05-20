// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "512"]

use bootreason::get_console_ramoops;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    BytesFile, CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
    FsNodeInfo, FsStr, StaticDirectoryBuilder,
};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::{statfs, PSTOREFS_MAGIC};

struct PstoreFsHandle {
    fs_handle: FileSystemHandle,
}

pub fn pstore_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let handle = current_task.kernel().expando.get_or_try_init(|| {
        Ok(PstoreFsHandle { fs_handle: PstoreFs::new_fs(current_task, options)? })
    })?;
    Ok(handle.fs_handle.clone())
}

pub struct PstoreFs;

impl FileSystemOps for PstoreFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(PSTOREFS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        "pstorefs".into()
    }
}

impl PstoreFs {
    pub fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Permanent, PstoreFs, options)?;
        let mut dir = StaticDirectoryBuilder::new(&fs);

        if let Some(ramoops_contents) = get_console_ramoops() {
            let ramoops_contents_0 = ramoops_contents.clone();
            dir.node(
                "console-ramoops-0",
                fs.create_node(
                    current_task,
                    BytesFile::new_node(ramoops_contents_0),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o440), FsCred::root()),
                ),
            );
            dir.node(
                "console-ramoops",
                fs.create_node(
                    current_task,
                    BytesFile::new_node(ramoops_contents),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o440), FsCred::root()),
                ),
            );
        }

        dir.build_root();

        Ok(fs)
    }
}
