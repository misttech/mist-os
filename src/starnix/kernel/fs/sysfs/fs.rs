// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::sysfs::{build_cpu_class_directory, build_kernel_directory, build_power_directory};
use crate::task::{CurrentTask, Kernel};
use crate::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use crate::vfs::pseudo::simple_file::BytesFile;
use crate::vfs::pseudo::stub_empty_file::StubEmptyFile;
use crate::vfs::{
    CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsStr,
};
use ebpf_api::BPF_PROG_TYPE_FUSE;
use starnix_logging::bug_ref;
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::{statfs, SYSFS_MAGIC};

struct SysFs;
impl FileSystemOps for SysFs {
    fn statfs(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(SYSFS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "sysfs".into()
    }
}

impl SysFs {
    fn new_fs<L>(
        locked: &mut Locked<L>,
        kernel: &Kernel,
        options: FileSystemOptions,
    ) -> FileSystemHandle
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let fs = FileSystem::new(
            locked,
            kernel,
            CacheMode::Cached(CacheConfig::default()),
            SysFs,
            options,
        )
        .expect("sysfs constructed with valid options");

        fn empty_dir(_: &SimpleDirectoryMutator) {}

        let registry = &kernel.device_registry;
        let root = &registry.objects.root;
        fs.create_root(fs.allocate_ino(), root.clone());
        let dir = SimpleDirectoryMutator::new(fs.clone(), root.clone());

        let dir_mode = 0o755;
        dir.subdir("fs", dir_mode, |dir| {
            dir.subdir("selinux", dir_mode, empty_dir);
            dir.subdir("bpf", dir_mode, empty_dir);
            dir.subdir("cgroup", dir_mode, empty_dir);
            dir.subdir("fuse", dir_mode, |dir| {
                dir.subdir("connections", dir_mode, empty_dir);
                dir.subdir("features", dir_mode, |dir| {
                    dir.entry(
                        "fuse_bpf",
                        BytesFile::new_node(b"supported\n".to_vec()),
                        mode!(IFREG, 0o444),
                    );
                });
                dir.entry(
                    "bpf_prog_type_fuse",
                    BytesFile::new_node(format!("{}\n", BPF_PROG_TYPE_FUSE).into_bytes()),
                    mode!(IFREG, 0o444),
                );
            });
            dir.subdir("pstore", dir_mode, empty_dir);
        });

        dir.subdir("devices", dir_mode, empty_dir);
        dir.subdir("bus", dir_mode, empty_dir);
        dir.subdir("block", dir_mode, empty_dir);
        dir.subdir("class", dir_mode, empty_dir);
        dir.subdir("dev", dir_mode, |dir| {
            dir.subdir("char", dir_mode, empty_dir);
            dir.subdir("block", dir_mode, empty_dir);
        });

        dir.subdir("kernel", 0o755, |dir| {
            build_kernel_directory(kernel, dir);
        });
        dir.subdir("power", 0o755, |dir| {
            build_power_directory(kernel, dir);
        });

        dir.subdir("module", dir_mode, |dir| {
            dir.subdir("dm_verity", dir_mode, |dir| {
                dir.subdir("parameters", dir_mode, |dir| {
                    dir.entry(
                        "prefetch_cluster",
                        StubEmptyFile::new_node(
                            "/sys/module/dm_verity/paramters/prefetch_cluster",
                            bug_ref!("https://fxbug.dev/322893670"),
                        ),
                        mode!(IFREG, 0o644),
                    );
                });
            });
        });

        // TODO(https://fxbug.dev/425942145): Correctly implement system filesystem in sysfs
        dir.subdir("devices", dir_mode, |dir| {
            dir.subdir("system", dir_mode, |dir| {
                dir.subdir("cpu", dir_mode, build_cpu_class_directory);
            });
        });

        fs
    }
}

struct SysFsHandle(FileSystemHandle);

pub fn sys_fs(
    locked: &mut Locked<Unlocked>,
    current_task: &CurrentTask,
    _options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    Ok(get_sysfs(locked, current_task.kernel()))
}

pub fn get_sysfs<L>(locked: &mut Locked<L>, kernel: &Kernel) -> FileSystemHandle
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    kernel
        .expando
        .get_or_init(|| SysFsHandle(SysFs::new_fs(locked, kernel, FileSystemOptions::default())))
        .0
        .clone()
}
