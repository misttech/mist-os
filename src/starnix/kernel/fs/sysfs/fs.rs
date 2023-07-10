// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    auth::FsCred,
    fs::{
        cgroup::CgroupDirectoryNode,
        kobject::*,
        sysfs::{CpuClassDirectory, SysFsDirectory},
        *,
    },
    task::*,
    types::*,
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
        let fs = FileSystem::new(kernel, CacheMode::Cached, SysFs, options);
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

        // TODO(https://fxbug.dev/130408) include concrete block devices
        let virtual_block_class = {
            kernel.device_registry.read().virtual_bus().get_or_create_child(
                b"block",
                KType::Class,
                SysFsDirectory::new,
            )
        };
        dir.entry(b"block", SysFsDirectory::new(Arc::downgrade(&virtual_block_class)), dir_mode);

        dir.entry(
            b"devices",
            SysFsDirectory::new(Arc::downgrade(&kernel.device_registry.read().root_kobject())),
            dir_mode,
        );

        dir.subdir(b"class", 0o755, |dir| {
            dir.entry(
                b"net",
                NetstackDevicesDirectory::new_sys_class_net(kernel.netstack_devices.clone()),
                dir_mode,
            );
        });

        // TODO(fxbug.dev/121327): Temporary fix of flakeness in tcp_socket_test.
        // Remove after registry.rs refactor is in place.
        kernel
            .device_registry
            .read()
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
