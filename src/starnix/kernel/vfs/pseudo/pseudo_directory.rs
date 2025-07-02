// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fileops_impl_unbounded_seek,
    fs_node_impl_dir_readonly, DirectoryEntryType, DirentSink, FileObject, FileOps, FsNode,
    FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::ino_t;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Arc;

pub struct PseudoDirEntry {
    pub ino: ino_t,
    pub mode: FileMode,
    pub name: FsString,
}

pub struct PseudoNode {
    pub ino: ino_t,
    pub ops: Box<dyn FsNodeOps>,
    pub info: FsNodeInfo,
}

pub trait PseudoDirectoryOps: Send + Sync + 'static {
    fn get_node(&self, name: &FsStr) -> Result<PseudoNode, Errno>;
    fn list_entries(&self) -> Vec<PseudoDirEntry>;
}

#[derive(Clone)]
pub struct PseudoDirectory {
    ops: Arc<dyn PseudoDirectoryOps>,
}

impl PseudoDirectory {
    pub fn new(ops: Arc<dyn PseudoDirectoryOps>) -> Arc<Self> {
        Arc::new(Self { ops })
    }
}

impl FsNodeOps for Arc<PseudoDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        self.ops.get_node(name).map(|pseudo_node| {
            node.fs().create_node(pseudo_node.ino, pseudo_node.ops, pseudo_node.info)
        })
    }
}

impl FileOps for PseudoDirectory {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();
    fileops_impl_unbounded_seek!();

    fn readdir(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // Skip through the entries until the current offset is reached.
        // Subtract 2 from the offset to account for `.` and `..`.
        let entries = self.ops.list_entries();
        for entry in entries.iter().skip(sink.offset() as usize - 2) {
            sink.add(
                entry.ino,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(entry.mode),
                entry.name.as_ref(),
            )?;
        }
        Ok(())
    }
}
