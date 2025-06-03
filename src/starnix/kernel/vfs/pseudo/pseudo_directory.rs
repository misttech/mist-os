// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fileops_impl_unbounded_seek,
    fs_node_impl_dir_readonly, DirectoryEntryType, DirentSink, FileObject, FileOps, FileSystem,
    FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, ino_t};
use std::collections::BTreeMap;
use std::sync::Arc;

type NodeOpsFactory = Arc<dyn Fn() -> Box<dyn FsNodeOps> + Send + Sync>;

#[derive(Clone)]
struct PseudoNodeEntry {
    mode: FileMode,
    creds: FsCred,
    node_ops_factory: NodeOpsFactory,
}

enum PseudoEntry {
    Node(PseudoNodeEntry),
    Builder(PseudoDirectoryBuilder),
}

#[derive(Default)]
pub struct PseudoDirectoryBuilder {
    entries: BTreeMap<&'static FsStr, PseudoEntry>,
}

impl PseudoDirectoryBuilder {
    pub fn node<F, O>(
        &mut self,
        name: impl Into<&'static FsStr>,
        mode: FileMode,
        creds: FsCred,
        ops: F,
    ) where
        F: Send + Sync + Fn() -> O + 'static,
        O: FsNodeOps,
    {
        let entry =
            PseudoNodeEntry { mode, creds, node_ops_factory: Arc::new(move || Box::new(ops())) };
        self.insert_node(name.into(), entry);
    }

    pub fn node_ops(
        &mut self,
        name: impl Into<&'static FsStr>,
        mode: FileMode,
        creds: FsCred,
        ops: impl Fn() -> Box<dyn FsNodeOps> + Send + Sync + 'static,
    ) {
        let entry = PseudoNodeEntry { mode, creds, node_ops_factory: Arc::new(move || ops()) };
        self.insert_node(name.into(), entry);
    }

    fn insert_node(&mut self, name: &'static FsStr, entry: PseudoNodeEntry) {
        let existing_entry = self.entries.insert(name, PseudoEntry::Node(entry));
        assert!(existing_entry.is_none());
    }

    pub fn subdir(&mut self, name: impl Into<&'static FsStr>) -> &mut PseudoDirectoryBuilder {
        let name = name.into();
        let entry = self
            .entries
            .entry(name)
            .or_insert_with(|| PseudoEntry::Builder(PseudoDirectoryBuilder::default()));
        if let PseudoEntry::Builder(ref mut builder) = entry {
            builder
        } else {
            panic!("expected a registry with name {name}");
        }
    }

    pub fn build(&self, fs: &FileSystem) -> Arc<PseudoDirectory> {
        let entries = self
            .entries
            .iter()
            .map(|(name, entry)| {
                let ino = fs.next_node_id();
                match entry {
                    PseudoEntry::Node(node) => (
                        *name,
                        PseudoDirectoryEntry {
                            ino,
                            mode: node.mode,
                            creds: node.creds,
                            node_ops_factory: node.node_ops_factory.clone(),
                        },
                    ),
                    PseudoEntry::Builder(registry) => {
                        let subdir = registry.build(fs);
                        let node_ops_factory =
                            Arc::new(move || -> Box<dyn FsNodeOps> { Box::new(subdir.clone()) });
                        (
                            *name,
                            PseudoDirectoryEntry {
                                ino,
                                mode: mode!(IFDIR, 755),
                                creds: FsCred::root(),
                                node_ops_factory,
                            },
                        )
                    }
                }
            })
            .collect();
        Arc::new(PseudoDirectory { entries })
    }

    pub fn build_root(&self, fs: &FileSystemHandle, mode: FileMode, creds: FsCred) {
        assert!(mode.is_dir(), "root directory must be a directory");
        let ops = self.build(fs);
        let root_ino = fs.next_node_id();
        fs.create_root_with_info(root_ino, ops, FsNodeInfo::new(mode, creds));
    }
}

struct PseudoDirectoryEntry {
    ino: ino_t,
    mode: FileMode,
    creds: FsCred,
    node_ops_factory: NodeOpsFactory,
}

pub struct PseudoDirectory {
    entries: BTreeMap<&'static FsStr, PseudoDirectoryEntry>,
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
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        self.entries
            .get(name)
            .map(|entry| {
                let ops = (entry.node_ops_factory)();
                let info = FsNodeInfo::new(entry.mode, entry.creds);
                node.fs().create_node(current_task, entry.ino, ops, info)
            })
            .ok_or_else(|| {
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
        for (name, node) in self.entries.iter().skip(sink.offset() as usize - 2) {
            sink.add(node.ino, sink.offset() + 1, DirectoryEntryType::from_mode(node.mode), name)?;
        }
        Ok(())
    }
}
