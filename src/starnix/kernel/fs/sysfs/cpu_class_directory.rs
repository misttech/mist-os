// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{KObject, KObjectHandle};
use crate::fs::tmpfs::TmpfsDirectory;
use crate::task::CurrentTask;
use crate::vfs::pseudo::simple_file::BytesFile;
use crate::vfs::pseudo::static_directory::StaticDirectoryBuilder;
use crate::vfs::pseudo::vec_directory::{VecDirectory, VecDirectoryEntry};
use crate::vfs::{
    fs_node_impl_dir_readonly, DirectoryEntryType, FileOps, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, FsString,
};

use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::Weak;

pub struct CpuClassDirectory {
    _kobject: Weak<KObject>,
}

impl CpuClassDirectory {
    pub fn new(_kobject: Weak<KObject>) -> Self {
        Self { _kobject }
    }

    fn kobject(&self) -> KObjectHandle {
        self._kobject.upgrade().expect("Weak references to kobject must always be valid")
    }
}

impl FsNodeOps for CpuClassDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
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
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::DIR,
                name: "vulnerabilities".into(),
                inode: None,
            },
            VecDirectoryEntry {
                entry_type: DirectoryEntryType::DIR,
                name: "cpufreq".into(),
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
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match &**name {
            b"online" => Ok(node.fs().create_node_and_allocate_node_id(
                BytesFile::new_node(format!("0-{}\n", zx::system_get_num_cpus() - 1).into_bytes()),
                FsNodeInfo::new(mode!(IFREG, 0o444), FsCred::root()),
            )),
            b"possible" => Ok(node.fs().create_node_and_allocate_node_id(
                BytesFile::new_node(format!("0-{}\n", zx::system_get_num_cpus() - 1).into_bytes()),
                FsNodeInfo::new(mode!(IFREG, 0o444), FsCred::root()),
            )),
            b"vulnerabilities" => {
                let mut child_kobject: Option<std::sync::Arc<KObject>> =
                    self.kobject().get_child(name);
                if let Some(kobject) = child_kobject.take() {
                    Ok(node.fs().create_node_and_allocate_node_id(
                        kobject.ops(),
                        FsNodeInfo::new(mode!(IFDIR, 0o755), FsCred::root()),
                    ))
                } else {
                    error!(ENOENT)
                }
            }
            b"cpufreq" => {
                let fs = &node.fs();
                let mut builder = StaticDirectoryBuilder::new(fs);
                builder.set_mode(mode!(IFDIR, 0o755));
                builder.subdir("policy0", 0o755, |dir| {
                    dir.subdir("stats", 0o755, |dir| {
                        dir.entry("reset", BytesFile::new_node(b"".to_vec()), mode!(IFREG, 0o200));
                    });
                });

                Ok(builder.build())
            }
            // This "cpu" match mush be ordered after "cpufrq". Since "cpufreq" also starts with
            // "cpu", it would be incorrectly matched as a generic CPU device if placed before.
            name if name.starts_with(b"cpu") => Ok(node.fs().create_node_and_allocate_node_id(
                TmpfsDirectory::new(),
                FsNodeInfo::new(mode!(IFDIR, 0o755), FsCred::root()),
            )),
            _ => error!(ENOENT),
        }
    }
}
