// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::kobject::KObjectHandle,
    fs::sysfs::{
        cgroup::CgroupDirectoryNode, sysfs_kernel_directory, sysfs_power_directory,
        CpuClassDirectory, SysfsDirectory,
    },
    task::{CurrentTask, NetstackDevicesDirectory},
    vfs::{
        CacheConfig, CacheMode, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsNodeInfo, FsNodeOps, FsStr, PathBuilder, StaticDirectoryBuilder, SymlinkNode,
    },
};
use starnix_uapi::{auth::FsCred, errors::Errno, file_mode::mode, statfs, SYSFS_MAGIC};

pub const SYSFS_DEVICES: &'static FsStr = b"devices";
pub const SYSFS_BUS: &'static FsStr = b"bus";
pub const SYSFS_CLASS: &'static FsStr = b"class";
pub const SYSFS_BLOCK: &'static FsStr = b"block";

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

        let registry = &kernel.device_registry;
        dir.entry(current_task, SYSFS_DEVICES, registry.root_kobject().ops(), dir_mode);
        dir.entry(current_task, SYSFS_BUS, registry.bus_subsystem_kobject().ops(), dir_mode);
        dir.entry(current_task, SYSFS_BLOCK, registry.block_subsystem_kobject().ops(), dir_mode);
        dir.entry(current_task, SYSFS_CLASS, registry.class_subsystem_kobject().ops(), dir_mode);

        // TODO(b/297438880): Remove this workaround after net devices are registered correctly.
        kernel
            .device_registry
            .class_subsystem_kobject()
            .get_or_create_child(b"net", |_| NetstackDevicesDirectory::new_sys_class_net());

        sysfs_kernel_directory(current_task, &mut dir);
        sysfs_power_directory(current_task, &mut dir);

        // TODO(https://fxbug.dev/121327): Temporary fix of flakeness in tcp_socket_test.
        // Remove after registry.rs refactor is in place.
        registry
            .root_kobject()
            .get_or_create_child(b"system", SysfsDirectory::new)
            .get_or_create_child(b"cpu", CpuClassDirectory::new);

        dir.build_root();
        fs
    }
}

pub fn sys_fs(current_task: &CurrentTask, options: FileSystemOptions) -> &FileSystemHandle {
    current_task.kernel().sys_fs.get_or_init(|| SysFs::new_fs(current_task, options))
}

pub trait SysfsOps: FsNodeOps {
    fn kobject(&self) -> KObjectHandle;
}

/// Creates a path to the `to` kobject in the devices tree, relative to the `from` kobject from
/// a subsystem.
pub fn sysfs_create_link(from: KObjectHandle, to: KObjectHandle) -> SymlinkNode {
    let mut path = PathBuilder::new();
    path.prepend_element(&to.path());
    // Escape one more level from its subsystem to the root of sysfs.
    path.prepend_element(b"..");
    path.prepend_element(&from.path_to_root());
    // Build a symlink with the relative path.
    SymlinkNode::new(&path.build_relative())
}
