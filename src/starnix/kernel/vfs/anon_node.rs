// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security;
use crate::task::{CurrentTask, Kernel};
use crate::vfs::{
    fs_node_impl_not_dir, CacheMode, FileHandle, FileObject, FileOps, FileSystem, FileSystemHandle,
    FileSystemOps, FileSystemOptions, FsNode, FsNodeInfo, FsNodeOps, FsStr, FsString,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{error, ino_t, statfs, ANON_INODE_FS_MAGIC};
use std::sync::Arc;

#[derive(Default)]
pub struct Anon {
    /// If this instance represents an `anon_inode` then `name` holds the type-name of the node,
    /// e.g. "inotify", "sync_file", "[usereventfd]", etc.
    name: Option<&'static str>,
}

impl FsNodeOps for Anon {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        error!(ENOSYS)
    }

    fn internal_name(&self, _node: &FsNode) -> Option<FsString> {
        self.name.map(|name| format!("anon_inode:{}", name).into())
    }
}

impl Anon {
    pub fn new_file_extended(
        current_task: &CurrentTask,
        ops: Box<dyn FileOps>,
        flags: OpenFlags,
        name: &'static str,
        info: impl FnOnce(ino_t) -> FsNodeInfo,
    ) -> FileHandle {
        let kernel = current_task.kernel();
        let fs = anon_fs(&kernel);
        let node = fs.create_node(current_task, Anon { name: Some(name) }, info);
        security::fs_node_init_anon(current_task, &node, name);
        FileObject::new_anonymous(current_task, ops, node, flags)
    }

    pub fn new_file(
        current_task: &CurrentTask,
        ops: Box<dyn FileOps>,
        flags: OpenFlags,
        name: &'static str,
    ) -> FileHandle {
        Self::new_file_extended(
            current_task,
            ops,
            flags,
            name,
            FsNodeInfo::new_factory(FileMode::from_bits(0o600), current_task.as_fscred()),
        )
    }
}

struct AnonFs;
impl FileSystemOps for AnonFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(ANON_INODE_FS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "anon".into()
    }
}
pub fn anon_fs(kernel: &Arc<Kernel>) -> FileSystemHandle {
    struct AnonFsHandle(FileSystemHandle);

    kernel
        .expando
        .get_or_init(|| {
            AnonFsHandle(
                FileSystem::new(kernel, CacheMode::Uncached, AnonFs, FileSystemOptions::default())
                    .expect("anonfs constructed with valid options"),
            )
        })
        .0
        .clone()
}
