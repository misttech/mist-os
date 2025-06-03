// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{KObject, KObjectHandle};
use crate::vfs::pseudo::pseudo_directory::{
    PseudoDirEntry, PseudoDirectory, PseudoDirectoryOps, PseudoNode,
};
use crate::vfs::{FsNodeInfo, FsStr};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use std::sync::{Arc, Weak};

pub struct KObjectDirectory {
    kobject: KObjectHandle,
}

impl KObjectDirectory {
    pub fn new(kobject: Weak<KObject>) -> Arc<PseudoDirectory> {
        let kobject = kobject.upgrade().expect("kobject is alive");
        let ops = Arc::new(Self { kobject });
        PseudoDirectory::new(ops)
    }
}

impl PseudoDirectoryOps for KObjectDirectory {
    fn get_node(&self, name: &FsStr) -> Result<PseudoNode, Errno> {
        self.kobject
            .get_child(name)
            .map(|child| {
                let ino = child.ino;
                let ops = child.ops();
                let info = FsNodeInfo::new(mode!(IFDIR, 0o755), FsCred::root());
                PseudoNode { ino, ops, info }
            })
            .ok_or_else(|| errno!(ENOENT))
    }

    fn list_entries(&self) -> Vec<PseudoDirEntry> {
        self.kobject
            .get_children_kobjects()
            .into_iter()
            .map(|child| PseudoDirEntry {
                name: child.name().into(),
                ino: child.ino,
                mode: mode!(IFDIR, 0o755),
            })
            .collect()
    }
}
