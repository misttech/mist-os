// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    auth::FsCred,
    fs::{
        kobject::{KObject, KObjectHandle},
        sysfs::SysFsOps,
        tmpfs::TmpfsDirectory,
        *,
    },
    task::CurrentTask,
    types::*,
};

use fuchsia_zircon as zx;
use std::sync::{Arc, Weak};

pub struct CpuClassDirectory {
    kobject: Weak<KObject>,
}

impl CpuClassDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }
}

impl SysFsOps for CpuClassDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.kobject.clone().upgrade().expect("Weak references to kobject must always be valid")
    }
}

impl FsNodeOps for CpuClassDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        static CPUS: once_cell::sync::OnceCell<Vec<String>> = once_cell::sync::OnceCell::new();

        let cpus = CPUS.get_or_init(|| {
            let num = zx::system_get_num_cpus();
            (0..num).map(|i| format!("cpu{}", i)).collect::<Vec<String>>()
        });

        let mut entries = vec![VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"online".to_vec(),
            inode: None,
        }];

        for cpu_name in cpus {
            entries.push(VecDirectoryEntry {
                entry_type: DirectoryEntryType::DIR,
                name: cpu_name.as_bytes().to_vec(),
                inode: None,
            })
        }

        // TODO(fxbug.dev/121327): A workaround before binding FsNodeOps to each kobject.
        Ok(VecDirectory::new_file(entries))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Arc<FsNode>, Errno> {
        match name {
            name if name.starts_with(b"cpu") => Ok(node.fs().create_node(
                TmpfsDirectory::new(),
                FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
            )),
            b"online" => Ok(node.fs().create_node(
                BytesFile::new_node(format!("{}\n", 1).into_bytes()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            _ => error!(ENOENT),
        }
    }
}
