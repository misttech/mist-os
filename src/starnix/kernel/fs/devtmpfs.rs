// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::DeviceMode,
    fs::{
        kobject::DeviceMetadata, tmpfs::TmpFs, DirEntryHandle, FileSystemHandle, FsStr, MountInfo,
    },
    task::CurrentTask,
};
use starnix_uapi::{auth::FsCred, device_type::DeviceType, errors::Errno, file_mode::mode};

pub fn dev_tmp_fs(current_task: &CurrentTask) -> &FileSystemHandle {
    current_task.kernel().dev_tmp_fs.get_or_init(|| init_devtmpfs(current_task))
}

fn init_devtmpfs(current_task: &CurrentTask) -> FileSystemHandle {
    let kernel = current_task.kernel();
    let fs = TmpFs::new_fs(kernel);
    let root = fs.root();

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
    create_symlink(current_task, root, b"fd", b"/proc/self/fd").unwrap();
    fs
}

pub fn devtmpfs_create_device(
    current_task: &CurrentTask,
    device: DeviceMetadata,
) -> Result<DirEntryHandle, Errno> {
    let mode = match device.mode {
        DeviceMode::Char => mode!(IFCHR, 0o666),
        DeviceMode::Block => mode!(IFBLK, 0o666),
    };
    // This creates content inside the temporary FS. This doesn't depend on the mount
    // information.
    dev_tmp_fs(current_task).root().create_entry(
        current_task,
        &MountInfo::detached(),
        &device.name,
        |dir, mount, name| {
            dir.mknod(current_task, mount, name, mode, device.device_type, FsCred::root())
        },
    )
}

pub fn devtmpfs_mkdir(current_task: &CurrentTask, name: &FsStr) -> Result<DirEntryHandle, Errno> {
    // This creates content inside the temporary FS. This doesn't depend on the mount
    // information.
    dev_tmp_fs(current_task).root().create_entry(
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

pub fn devtmpfs_remove_child(current_task: &CurrentTask, name: &FsStr) {
    dev_tmp_fs(current_task).root().remove_child(name);
}

pub fn devtmpfs_create_symlink(
    current_task: &CurrentTask,
    name: &FsStr,
    target: &FsStr,
) -> Result<DirEntryHandle, Errno> {
    create_symlink(current_task, dev_tmp_fs(current_task).root(), name, target)
}

fn create_symlink(
    current_task: &CurrentTask,
    entry: &DirEntryHandle,
    name: &FsStr,
    target: &FsStr,
) -> Result<DirEntryHandle, Errno> {
    // This creates content inside the temporary FS. This doesn't depend on the mount
    // information.
    entry.create_entry(current_task, &MountInfo::detached(), name, |dir, mount, name| {
        dir.create_symlink(current_task, mount, name, target, FsCred::root())
    })
}
