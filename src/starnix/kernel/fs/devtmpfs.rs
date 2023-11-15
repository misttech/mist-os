// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    device::DeviceMode,
    fs::{
        kobject::DeviceMetadata, tmpfs::TmpFs, DirEntryHandle, FileSystemHandle, FsStr, MountInfo,
    },
    task::Kernel,
    types::errno::Errno,
    types::{mode, DeviceType},
};
use std::sync::Arc;

pub fn dev_tmp_fs(kernel: &Arc<Kernel>) -> &FileSystemHandle {
    kernel.dev_tmp_fs.get_or_init(|| init_devtmpfs(kernel))
}

fn init_devtmpfs(kernel: &Arc<Kernel>) -> FileSystemHandle {
    let fs = TmpFs::new_fs(kernel);
    let root = fs.root();
    let current_task = kernel.kthreads.system_task();

    let mkdir = |name| {
        // This creates content inside the temporary FS. This doesn't depend on the mount
        // information.
        root.create_entry(current_task, &MountInfo::detached(), name, |dir, mount, name| {
            dir.mknod(
                current_task,
                mount,
                name,
                mode!(IFDIR, 0o755),
                DeviceType::NONE,
                FsCred::root(),
            )
        })
        .unwrap();
    };

    mkdir(b"shm");
    create_symlink(kernel, root, b"fd", b"/proc/self/fd").unwrap();
    fs
}

pub fn devtmpfs_create_device(
    kernel: &Arc<Kernel>,
    device: DeviceMetadata,
) -> Result<DirEntryHandle, Errno> {
    let current_task = kernel.kthreads.system_task();
    let mode = match device.mode {
        DeviceMode::Char => mode!(IFCHR, 0o666),
        DeviceMode::Block => mode!(IFBLK, 0o666),
    };
    // This creates content inside the temporary FS. This doesn't depend on the mount
    // information.
    dev_tmp_fs(kernel).root().create_entry(
        current_task,
        &MountInfo::detached(),
        &device.name,
        |dir, mount, name| {
            dir.mknod(current_task, mount, name, mode, device.device_type, FsCred::root())
        },
    )
}

pub fn devtmpfs_mkdir(kernel: &Arc<Kernel>, name: &FsStr) -> Result<DirEntryHandle, Errno> {
    let current_task = kernel.kthreads.system_task();
    // This creates content inside the temporary FS. This doesn't depend on the mount
    // information.
    dev_tmp_fs(kernel).root().create_entry(
        current_task,
        &MountInfo::detached(),
        name,
        |dir, mount, name| {
            dir.mknod(
                current_task,
                mount,
                name,
                mode!(IFDIR, 0o755),
                DeviceType::NONE,
                FsCred::root(),
            )
        },
    )
}

pub fn devtmpfs_remove_child(kernel: &Arc<Kernel>, name: &FsStr) {
    dev_tmp_fs(kernel).root().remove_child(name);
}

pub fn devtmpfs_create_symlink(
    kernel: &Arc<Kernel>,
    name: &FsStr,
    target: &FsStr,
) -> Result<DirEntryHandle, Errno> {
    create_symlink(kernel, dev_tmp_fs(kernel).root(), name, target)
}

fn create_symlink(
    kernel: &Arc<Kernel>,
    entry: &DirEntryHandle,
    name: &FsStr,
    target: &FsStr,
) -> Result<DirEntryHandle, Errno> {
    let current_task = kernel.kthreads.system_task();
    // This creates content inside the temporary FS. This doesn't depend on the mount
    // information.
    entry.create_entry(current_task, &MountInfo::detached(), name, |dir, mount, name| {
        dir.create_symlink(current_task, mount, name, target, FsCred::root())
    })
}
