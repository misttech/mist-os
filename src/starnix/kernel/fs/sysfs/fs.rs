// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    auth::FsCred,
    fs::{
        cgroup::CgroupDirectoryNode,
        kobject::{KObjectHandle, KType},
        sysfs::{sysfs_power_directory, CpuClassDirectory, SysFsDirectory},
        CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsNodeInfo, FsNodeOps, FsStr, StaticDirectoryBuilder,
    },
    task::{CurrentTask, Kernel, NetstackDevicesDirectory},
    types::{errno::Errno, file_mode::mode, statfs, SYSFS_MAGIC},
};

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
    pub fn new_fs(kernel: &Arc<Kernel>, options: FileSystemOptions) -> FileSystemHandle {
        let fs = FileSystem::new(kernel, CacheMode::Cached(CacheConfig::default()), SysFs, options);
        let mut dir = StaticDirectoryBuilder::new(&fs);
        let dir_mode = mode!(IFDIR, 0o755);
        dir.subdir(b"fs", 0o755, |dir| {
            dir.subdir(b"selinux", 0o755, |_| ());
            dir.subdir(b"bpf", 0o755, |_| ());
            dir.node(
                b"cgroup",
                fs.create_node(
                    CgroupDirectoryNode::new(),
                    FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
                ),
            );
            dir.subdir(b"fuse", 0o755, |dir| dir.subdir(b"connections", 0o755, |_| ()));
        });
        dir.subdir(b"kernel", 0o755, |dir| {
            dir.subdir(b"tracing", 0o755, |_| ());
        });

        dir.entry(
            b"devices",
            SysFsDirectory::new(Arc::downgrade(&kernel.device_registry.root_kobject())),
            dir_mode,
        );
        dir.entry(b"block", kernel.device_registry.block_collection().ops(), dir_mode);
        dir.entry(b"class", kernel.device_registry.class_subsystem_kobject().ops(), dir_mode);

        // TODO(b/297438880): Remove this workaround after net devices are registered correctly.
        kernel.device_registry.class_subsystem_kobject().get_or_create_child(
            b"net",
            KType::Collection,
            |_| NetstackDevicesDirectory::new_sys_class_net(),
        );

        sysfs_power_directory(&mut dir, &fs, Arc::downgrade(kernel));

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

pub fn sys_fs(kern: &Arc<Kernel>, options: FileSystemOptions) -> &FileSystemHandle {
    kern.sys_fs.get_or_init(|| SysFs::new_fs(kern, options))
}

pub trait SysFsOps: FsNodeOps {
    fn kobject(&self) -> KObjectHandle;
}
