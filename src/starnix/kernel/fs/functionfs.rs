// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{
        fileops_impl_seekless, fs_node_impl_dir_readonly, fs_node_impl_not_dir, CacheMode,
        FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsNode, FsNodeInfo, FsNodeOps, FsStr, InputBuffer, MemoryDirectoryFile, MountInfo,
        OutputBuffer,
    },
};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked, WriteOps};
use starnix_uapi::{
    error, errors::Errno, file_mode::mode, open_flags::OpenFlags, statfs, vfs::default_statfs,
};
use std::sync::Arc;

// Control endpoint is always present in a mounted FunctionFS.
const CONTROL_ENDPOINT: &str = "ep0";

// Magic number of the file system, different from the magic used for Descriptors and Strings.
// Set to the same value as Linux.
const FUNCTIONFS_MAGIC: u32 = 0xa647361;

pub struct FunctionFs;
impl FunctionFs {
    pub fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        if options.source != "adb" {
            return error!(EINVAL);
        }

        let fs = FileSystem::new(current_task.kernel(), CacheMode::Permanent, FunctionFs, options);
        fs.set_root(FunctionFsRootDir {});

        // FunctionFS always initializes with the control endpoint.
        fs.root().create_entry(
            current_task,
            &MountInfo::detached(),
            CONTROL_ENDPOINT.into(),
            |_dir, _mount, _name| {
                let ops: Box<dyn FsNodeOps> = Box::new(Arc::new(FunctionFsControlEndpoint {}));
                Ok(fs.create_node(
                    current_task,
                    ops,
                    FsNodeInfo::new_factory(mode!(IFREG, 0o600), current_task.as_fscred()),
                ))
            },
        )?;

        Ok(fs)
    }
}

impl FileSystemOps for FunctionFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(default_statfs(FUNCTIONFS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        b"functionfs".into()
    }
}

struct FunctionFsRootDir;
impl FsNodeOps for FunctionFsRootDir {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MemoryDirectoryFile::new()))
    }
}

struct FunctionFsControlEndpoint;

impl FsNodeOps for Arc<FunctionFsControlEndpoint> {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }
}

impl FileOps for Arc<FunctionFsControlEndpoint> {
    fileops_impl_seekless!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        track_stub!(TODO("https://fxbug.dev/319502917"), "FunctionFsControlEndpoint::read");
        error!(EAGAIN)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        track_stub!(TODO("https://fxbug.dev/319502917"), "FunctionFsControlEndpoint::write");
        error!(EAGAIN)
    }
}
