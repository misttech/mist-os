// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::KObjectHandle;
use crate::fs::sysfs::{
    sysfs_kernel_directory, sysfs_power_directory, CpuClassDirectory, KObjectDirectory,
};
use crate::task::{CurrentTask, Kernel, NetstackDevicesDirectory};
use crate::vfs::{
    BytesFile, CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps,
    FileSystemOptions, FsNodeInfo, FsStr, PathBuilder, StaticDirectoryBuilder, StubEmptyFile,
    SymlinkNode,
};
use ebpf_api::BPF_PROG_TYPE_FUSE;
use starnix_logging::bug_ref;
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::{ino_t, statfs, SYSFS_MAGIC};
use std::sync::Arc;

pub const SYSFS_DEVICES: &str = "devices";
pub const SYSFS_BUS: &str = "bus";
pub const SYSFS_CLASS: &str = "class";
pub const SYSFS_BLOCK: &str = "block";
pub const SYSFS_DEV: &str = "dev";

struct SysFs;
impl FileSystemOps for SysFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
    pub fn new_fs(current_task: &CurrentTask, options: FileSystemOptions) -> FileSystemHandle {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Cached(CacheConfig::default()), SysFs, options)
            .expect("sysfs constructed with valid options");
        let mut dir = StaticDirectoryBuilder::new(&fs);
        let dir_mode = mode!(IFDIR, 0o755);
        dir.subdir(current_task, "fs", 0o755, |dir| {
            dir.subdir(current_task, "selinux", 0o755, |_| ());
            dir.subdir(current_task, "bpf", 0o755, |_| ());
            dir.subdir(current_task, "cgroup", 0o755, |_| ());
            dir.subdir(current_task, "fuse", 0o755, |dir| {
                dir.subdir(current_task, "connections", 0o755, |_| ());
                dir.subdir(current_task, "features", 0o755, |dir| {
                    dir.node(
                        "fuse_bpf",
                        fs.create_node(
                            current_task,
                            BytesFile::new_node(b"supported\n".to_vec()),
                            FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                        ),
                    );
                });
                dir.node(
                    "bpf_prog_type_fuse",
                    fs.create_node(
                        current_task,
                        BytesFile::new_node(format!("{}\n", BPF_PROG_TYPE_FUSE).into_bytes()),
                        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                    ),
                );
            });
            dir.subdir(current_task, "nmfs", 0o755, |_| ());
        });

        let registry = &kernel.device_registry;
        dir.entry(current_task, SYSFS_DEVICES, registry.objects.devices.ops(), dir_mode);
        dir.entry(current_task, SYSFS_BUS, registry.objects.bus.ops(), dir_mode);
        dir.entry(current_task, SYSFS_BLOCK, registry.objects.block.ops(), dir_mode);
        dir.entry(current_task, SYSFS_CLASS, registry.objects.class.ops(), dir_mode);
        dir.entry(current_task, SYSFS_DEV, registry.objects.dev.ops(), dir_mode);

        // TODO(b/297438880): Remove this workaround after net devices are registered correctly.
        kernel
            .device_registry
            .objects
            .class
            .get_or_create_child("net".into(), |_| NetstackDevicesDirectory::new_sys_class_net());

        sysfs_kernel_directory(current_task, &mut dir);
        sysfs_power_directory(current_task, &mut dir);

        dir.subdir(current_task, "module", 0o755, |dir| {
            dir.subdir(current_task, "dm_verity", 0o755, |dir| {
                dir.subdir(current_task, "parameters", 0o755, |dir| {
                    dir.entry(
                        current_task,
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

        // TODO(https://fxbug.dev/42072346): Temporary fix of flakeness in tcp_socket_test.
        // Remove after registry.rs refactor is in place.
        registry
            .objects
            .devices
            .get_or_create_child("system".into(), KObjectDirectory::new)
            .get_or_create_child("cpu".into(), CpuClassDirectory::new);

        dir.build_root();
        fs
    }
}

struct SysFsHandle(FileSystemHandle);

pub fn get_sys_fs(kernel: &Arc<Kernel>) -> Option<FileSystemHandle> {
    kernel.expando.peek::<SysFsHandle>().map(|h| h.0.clone())
}

pub fn sys_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    Ok(current_task
        .kernel()
        .expando
        .get_or_init(|| SysFsHandle(SysFs::new_fs(current_task, options)))
        .0
        .clone())
}

/// Creates a path to the `to` kobject in the devices tree, relative to the `from` kobject from
/// a subsystem.
pub fn sysfs_create_link(
    from: KObjectHandle,
    to: KObjectHandle,
    owner: FsCred,
) -> (SymlinkNode, impl FnOnce(ino_t) -> FsNodeInfo) {
    let mut path = PathBuilder::new();
    path.prepend_element(to.path().as_ref());
    // Escape one more level from its subsystem to the root of sysfs.
    path.prepend_element("..".into());

    let path_to_root = from.path_to_root();
    if !path_to_root.is_empty() {
        path.prepend_element(path_to_root.as_ref());
    }

    // Build a symlink with the relative path.
    SymlinkNode::new(path.build_relative().as_ref(), owner)
}
