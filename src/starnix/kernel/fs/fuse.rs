// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps,
        FsNode, FsNodeHandle, FsNodeOps, FsStr,
    },
    task::CurrentTask,
    types::{error, statfs, Errno, OpenFlags},
};

pub struct DevFuse;

impl FileOps for DevFuse {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOTSUP)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOTSUP)
    }
}

pub fn new_fuse_fs(task: &CurrentTask, _data: &FsStr) -> FileSystemHandle {
    let fs = FileSystem::new(task.kernel(), FuseFs {});
    fs.set_root_node(FsNode::new_root(FuseNode {}));
    fs
}

struct FuseFs;

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
}

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
