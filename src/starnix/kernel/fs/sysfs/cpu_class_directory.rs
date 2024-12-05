// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::KObject;
use crate::fs::tmpfs::TmpfsDirectory;
use crate::task::CurrentTask;
use crate::vfs::{
    fs_node_impl_dir_readonly, BytesFile, DirectoryEntryType, FileOps, FsNode, FsNodeHandle,
    FsNodeInfo, FsNodeOps, FsStr, FsString, VecDirectory, VecDirectoryEntry,
};

use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Weak;

pub struct CpuClassDirectory {}

impl CpuClassDirectory {
    pub fn new(_kobject: Weak<KObject>) -> Self {
        Self {}
    }
}

impl FsNodeOps for CpuClassDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        static CPUS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

        let cpus = CPUS.get_or_init(|| {
            let num = zx::system_get_num_cpus();
            (0..num).map(|i| format!("cpu{}", i)).collect::<Vec<String>>()
        });

        let mut entries = vec![
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: "online".into(),
                inode: None,
            },
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: "possible".into(),
                inode: None,
            },
        ];

        for cpu_name in cpus {
            entries.push(VecDirectoryEntry {
                entry_type: DirectoryEntryType::DIR,
                name: FsString::from(cpu_name.clone()),
                inode: None,
            })
        }

        // TODO(https://fxbug.dev/42072346): A workaround before binding FsNodeOps to each kobject.
        Ok(VecDirectory::new_file(entries))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match &**name {
            name if name.starts_with(b"cpu") => Ok(node.fs().create_node(
                current_task,
                TmpfsDirectory::new(),
                FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
            )),
            b"online" => Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(format!("0-{}\n", zx::system_get_num_cpus() - 1).into_bytes()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            b"possible" => Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(format!("0-{}\n", zx::system_get_num_cpus() - 1).into_bytes()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            )),
            _ => error!(ENOENT),
        }
    }
}
