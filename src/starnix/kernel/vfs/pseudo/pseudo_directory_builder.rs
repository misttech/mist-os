// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::pseudo::pseudo_directory::{
    PseudoDirEntry, PseudoDirectory, PseudoDirectoryOps, PseudoNode,
};
use crate::vfs::{FileSystem, FileSystemHandle, FsNodeInfo, FsNodeOps, FsStr};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::{errno, ino_t};
use std::collections::BTreeMap;
use std::sync::Arc;

pub type NodeOpsFactory = Arc<dyn Fn() -> Box<dyn FsNodeOps> + Send + Sync>;

#[derive(Clone)]
pub struct PseudoNodeEntry {
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
                let ino = fs.allocate_ino();
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
        let ops = Arc::new(BtreePseudoDirectory { entries });
        PseudoDirectory::new(ops)
    }

    pub fn build_root(&self, fs: &FileSystemHandle, mode: FileMode, creds: FsCred) {
        assert!(mode.is_dir(), "root directory must be a directory");
        let ops = self.build(fs);
        let root_ino = fs.allocate_ino();
        fs.create_root_with_info(root_ino, ops, FsNodeInfo::new(mode, creds));
    }
}

pub struct PseudoDirectoryEntry {
    pub ino: ino_t,
    pub mode: FileMode,
    pub creds: FsCred,
    pub node_ops_factory: NodeOpsFactory,
}

pub struct BtreePseudoDirectory {
    entries: BTreeMap<&'static FsStr, PseudoDirectoryEntry>,
}

impl PseudoDirectoryOps for BtreePseudoDirectory {
    fn get_node(&self, name: &FsStr) -> Result<PseudoNode, Errno> {
        let entry = self.entries.get(name).ok_or_else(|| errno!(ENOENT))?;
        let ops = (entry.node_ops_factory)();
        let ino = entry.ino;
        let info = FsNodeInfo::new(entry.mode, entry.creds);
        Ok(PseudoNode { ops, ino, info })
    }

    fn list_entries(&self) -> Vec<PseudoDirEntry> {
        self.entries
            .iter()
            .map(|(name, entry)| PseudoDirEntry {
                ino: entry.ino,
                mode: entry.mode,
                name: (*name).into(),
            })
            .collect()
    }
}
