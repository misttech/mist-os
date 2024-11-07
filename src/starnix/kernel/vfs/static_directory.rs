// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    unbounded_seek, DirectoryEntryType, DirentSink, FileObject, FileOps, FileSystem,
    FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, SeekTarget,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, gid_t, off_t, uid_t};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Builds an implementation of [`FsNodeOps`] that serves as a directory of static and immutable
/// entries.
pub struct StaticDirectoryBuilder<'a> {
    fs: &'a Arc<FileSystem>,
    mode: FileMode,
    creds: FsCred,
    entry_creds: FsCred,
    entries: BTreeMap<&'static FsStr, FsNodeHandle>,
}

impl<'a> StaticDirectoryBuilder<'a> {
    /// Creates a new builder using the given [`FileSystem`] to acquire inode numbers.
    pub fn new(fs: &'a FileSystemHandle) -> Self {
        Self {
            fs,
            mode: mode!(IFDIR, 0o777),
            creds: FsCred::root(),
            entry_creds: FsCred::root(),
            entries: BTreeMap::new(),
        }
    }

    /// Set the creds used for future entries.
    pub fn entry_creds(&mut self, creds: FsCred) {
        self.entry_creds = creds;
    }

    /// Adds an entry to the directory. Panics if an entry with the same name was already added.
    pub fn entry(
        &mut self,
        current_task: &CurrentTask,
        name: &'static str,
        ops: impl Into<Box<dyn FsNodeOps>>,
        mode: FileMode,
    ) {
        let ops = ops.into();
        self.entry_dev(current_task, name, ops, mode, DeviceType::NONE);
    }

    /// Adds an entry to the directory. Panics if an entry with the same name was already added.
    pub fn entry_dev(
        &mut self,
        current_task: &CurrentTask,
        name: &'static str,
        ops: impl Into<Box<dyn FsNodeOps>>,
        mode: FileMode,
        dev: DeviceType,
    ) {
        let ops = ops.into();
        let node = self.fs.create_node(current_task, ops, |id| {
            let mut info = FsNodeInfo::new(id, mode, self.entry_creds.clone());
            info.rdev = dev;
            info
        });
        self.node(name, node);
    }

    pub fn subdir(
        &mut self,
        current_task: &CurrentTask,
        name: &'static str,
        mode: u32,
        build_subdir: impl Fn(&mut Self),
    ) {
        let mut subdir = Self::new(self.fs);
        build_subdir(&mut subdir);
        subdir.set_mode(mode!(IFDIR, mode));
        self.node(name, subdir.build(current_task));
    }

    /// Adds an [`FsNode`] entry to the directory, which already has an inode number and file mode.
    /// Panics if an entry with the same name was already added.
    pub fn node(&mut self, name: &'static str, node: FsNodeHandle) {
        assert!(
            self.entries.insert(name.into(), node).is_none(),
            "adding a duplicate entry into a StaticDirectory",
        );
    }

    /// Set the mode of the directory. The type must always be IFDIR.
    pub fn set_mode(&mut self, mode: FileMode) {
        assert!(mode.is_dir());
        self.mode = mode;
    }

    pub fn dir_creds(&mut self, creds: FsCred) {
        self.creds = creds;
    }

    /// Builds an [`FsNode`] that serves as a directory of the entries added to this builder.
    pub fn build(self, current_task: &CurrentTask) -> FsNodeHandle {
        self.fs.create_node(
            current_task,
            Arc::new(StaticDirectory { entries: self.entries }),
            FsNodeInfo::new_factory(self.mode, self.creds),
        )
    }

    /// Build the node associated with the static directory and makes it the root of the
    /// filesystem.
    pub fn build_root(self) {
        let node = FsNode::new_root_with_properties(
            Arc::new(StaticDirectory { entries: self.entries }),
            |info| {
                info.uid = self.creds.uid;
                info.gid = self.creds.gid;
                info.mode = self.mode;
            },
        );
        self.fs.set_root_node(node);
    }

    /// Builds [`FsNodeOps`] and [`FsFileOps`] for this directory.
    pub fn build_ops(self) -> (Box<dyn FsNodeOps>, Box<dyn FileOps>) {
        let directory = Arc::new(StaticDirectory { entries: self.entries });
        (Box::new(directory.clone()), Box::new(directory))
    }
}

pub struct StaticDirectory {
    entries: BTreeMap<&'static FsStr, FsNodeHandle>,
}

impl StaticDirectory {
    pub fn force_chown(&self, current_task: &CurrentTask, uid: Option<uid_t>, gid: Option<gid_t>) {
        for (_, node) in self.entries.iter() {
            node.update_info(|info| {
                info.chown(uid, gid);
            });

            let Some(static_dir) = node.downcast_ops::<Arc<StaticDirectory>>() else {
                continue;
            };
            static_dir.force_chown(current_task, uid, gid);
        }
    }
}

impl FsNodeOps for Arc<StaticDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        self.entries.get(name).cloned().ok_or_else(|| {
            errno!(
                ENOENT,
                format!(
                    "looking for {name} in {:?}",
                    self.entries.keys().map(|e| e.to_string()).collect::<Vec<_>>()
                )
            )
        })
    }
}

impl FileOps for StaticDirectory {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        unbounded_seek(current_offset, target)
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // Skip through the entries until the current offset is reached.
        // Subtract 2 from the offset to account for `.` and `..`.
        for (name, node) in self.entries.iter().skip(sink.offset() as usize - 2) {
            sink.add(
                node.node_id,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(node.info().mode),
                name,
            )?;
        }
        Ok(())
    }
}
