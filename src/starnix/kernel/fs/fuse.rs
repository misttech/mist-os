// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, fs_args, CacheMode, FdNumber, FileObject, FileOps, FileSystem,
        FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeOps, FsStr,
    },
    task::CurrentTask,
    types::{errno, error, statfs, Errno, OpenFlags},
};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct FuseState;

#[derive(Debug, Default)]
pub struct DevFuse {
    state: Arc<FuseState>,
}

impl FileOps for DevFuse {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(ENOTSUP)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(ENOTSUP)
    }
}

pub fn new_fuse_fs(
    task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let mut mount_options = fs_args::generic_parse_mount_options(&options.params);
    let fd = fs_args::parse::<FdNumber>(
        mount_options.remove(b"fd" as &FsStr).ok_or_else(|| errno!(EINVAL))?,
    )?;
    let state =
        task.files.get(fd)?.downcast_file::<DevFuse>().ok_or_else(|| errno!(EINVAL))?.state.clone();

    let fs = FileSystem::new(task.kernel(), CacheMode::Uncached, FuseFs::new(state), options);
    fs.set_root_node(FsNode::new_root(FuseNode {}));
    Ok(fs)
}

#[derive(Debug)]
struct FuseFs {
    _state: Arc<FuseState>,
}

impl FuseFs {
    fn new(state: Arc<FuseState>) -> Self {
        Self { _state: state }
    }
}

impl FileSystemOps for FuseFs {
    fn rename(
        &self,
        _fs: &FileSystem,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno> {
        error!(ENOTSUP)
    }
    fn name(&self) -> &'static FsStr {
        b"fuse"
    }
}

#[derive(Debug)]
struct FuseNode;

impl FsNodeOps for FuseNode {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        error!(ENOTSUP)
    }
}
