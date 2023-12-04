// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::{CurrentTask, NetstackDevicesDirectory},
    vfs::{
        cgroup::CgroupDirectoryNode,
        kobject::{KObjectHandle, KType},
        sysfs::{sysfs_kernel_directory, sysfs_power_directory, CpuClassDirectory, SysFsDirectory},
        CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsNodeInfo, FsNodeOps, FsStr, StaticDirectoryBuilder,
    },
};
use starnix_uapi::{auth::FsCred, errors::Errno, file_mode::mode, statfs, SYSFS_MAGIC};
use std::sync::Arc;

struct SysFs;
impl FileSystemOps for SysFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(statfs::default(SYSFS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        b"sysfs"
    }
}

impl SysFs {
    pub fn new_fs(current_task: &CurrentTask, options: FileSystemOptions) -> FileSystemHandle {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Cached(CacheConfig::default()), SysFs, options);
        let mut dir = StaticDirectoryBuilder::new(&fs);
        let dir_mode = mode!(IFDIR, 0o755);
        dir.subdir(current_task, b"fs", 0o755, |dir| {
            dir.subdir(current_task, b"selinux", 0o755, |_| ());
            dir.subdir(current_task, b"bpf", 0o755, |_| ());
            dir.node(
                b"cgroup",
                fs.create_node(
                    current_task,
                    CgroupDirectoryNode::new(),
                    FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
                ),
            );
            dir.subdir(current_task, b"fuse", 0o755, |dir| {
                dir.subdir(current_task, b"connections", 0o755, |_| ())
            });
        });

        dir.entry(
            current_task,
            b"devices",
            SysFsDirectory::new(Arc::downgrade(&kernel.device_registry.root_kobject())),
            dir_mode,
        );
        dir.entry(
            current_task,
            b"block",
            kernel.device_registry.block_collection().ops(),
            dir_mode,
        );
        dir.entry(
            current_task,
            b"class",
            kernel.device_registry.class_subsystem_kobject().ops(),
            dir_mode,
        );

        // TODO(b/297438880): Remove this workaround after net devices are registered correctly.
        kernel.device_registry.class_subsystem_kobject().get_or_create_child(
            b"net",
            KType::Collection,
            |_| NetstackDevicesDirectory::new_sys_class_net(),
        );

        sysfs_kernel_directory(current_task, &mut dir);
        sysfs_power_directory(current_task, &mut dir);

        // TODO(fxbug.dev/121327): Temporary fix of flakeness in tcp_socket_test.
        // Remove after registry.rs refactor is in place.
        kernel
            .device_registry
            .root_kobject()
            .get_or_create_child(b"system", KType::Bus, SysFsDirectory::new)
            .get_or_create_child(b"cpu", KType::Class, CpuClassDirectory::new);

        dir.build_root();
        fs
    }
}

pub fn sys_fs(current_task: &CurrentTask, options: FileSystemOptions) -> &FileSystemHandle {
    current_task.kernel().sys_fs.get_or_init(|| SysFs::new_fs(current_task, options))
}

pub trait SysFsOps: FsNodeOps {
    fn kobject(&self) -> KObjectHandle;
}
