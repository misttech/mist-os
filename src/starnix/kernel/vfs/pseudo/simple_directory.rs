// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fileops_impl_unbounded_seek,
    fs_node_impl_dir_readonly, DirectoryEntryType, DirentSink, FileObject, FileOps,
    FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString,
};
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct SimpleDirectoryMutator {
    fs: FileSystemHandle,
    directory: Arc<SimpleDirectory>,
}

impl SimpleDirectoryMutator {
    pub fn new(fs: FileSystemHandle, directory: Arc<SimpleDirectory>) -> Self {
        Self { fs, directory }
    }

    pub fn node(&self, name: FsString, node: FsNodeHandle) {
        self.directory.entries.lock().insert(name, node);
    }

    pub fn entry(&self, name: &str, ops: impl Into<Box<dyn FsNodeOps>>, mode: FileMode) {
        let node =
            self.fs.create_node_and_allocate_node_id(ops, FsNodeInfo::new(mode, FsCred::root()));
        self.node(name.into(), node);
    }

    pub fn entry_etc(
        &self,
        name: FsString,
        ops: impl Into<Box<dyn FsNodeOps>>,
        mode: FileMode,
        dev: DeviceType,
        creds: FsCred,
    ) {
        let mut info = FsNodeInfo::new(mode, creds);
        info.rdev = dev;
        let node = self.fs.create_node_and_allocate_node_id(ops, info);
        self.node(name, node);
    }

    pub fn subdir(&self, name: &str, mode: u32, build_subdir: impl Fn(&Self)) {
        let dir = {
            let mut entries = self.directory.entries.lock();
            let name: &FsStr = name.into();
            if let Some(node) = entries.get(name) {
                assert!(node.info().mode == mode!(IFDIR, mode));
                let dir = node
                    .downcast_ops::<Arc<SimpleDirectory>>()
                    .expect("subdir is a SimpleDirectory");
                dir.clone()
            } else {
                let dir = SimpleDirectory::new();
                let info = FsNodeInfo::new(mode!(IFDIR, mode), FsCred::root());
                let node = self.fs.create_node_and_allocate_node_id(dir.clone(), info);
                entries.insert(name.into(), node);
                dir
            }
        };
        let mutator = SimpleDirectoryMutator::new(self.fs.clone(), dir);
        build_subdir(&mutator);
    }
}

pub struct SimpleDirectory {
    entries: Mutex<BTreeMap<FsString, FsNodeHandle>>,
}

impl SimpleDirectory {
    pub fn new() -> Arc<Self> {
        Arc::new(SimpleDirectory { entries: Default::default() })
    }
}

impl FsNodeOps for Arc<SimpleDirectory> {
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
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let entries = self.entries.lock();
        entries.get(name).cloned().ok_or_else(|| {
            errno!(
                ENOENT,
                format!(
                    "looking for {name} in {:?}",
                    entries.keys().map(|e| e.to_string()).collect::<Vec<_>>()
                )
            )
        })
    }
}

impl FileOps for SimpleDirectory {
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
        let entries = self.entries.lock();
        for (name, node) in entries.iter().skip(sink.offset() as usize - 2) {
            sink.add(
                node.ino,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(node.info().mode),
                name.as_ref(),
            )?;
        }
        Ok(())
    }
}
