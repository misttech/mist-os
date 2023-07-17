// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    device::DeviceMode,
    fs::{kobject::DeviceMetadata, tmpfs::*, *},
    task::*,
    types::*,
};
use std::sync::Arc;

pub fn dev_tmp_fs(kernel: &Arc<Kernel>) -> &FileSystemHandle {
    kernel.dev_tmp_fs.get_or_init(|| init_devtmpfs(kernel))
}

fn init_devtmpfs(kernel: &Arc<Kernel>) -> FileSystemHandle {
    let fs = TmpFs::new_fs(kernel);
    let root = fs.root();

    let mkdir = |name| {
        root.create_node(
            kernel.kthreads.system_task(),
            name,
            mode!(IFDIR, 0o755),
            DeviceType::NONE,
            FsCred::root(),
        )
        .unwrap();
    };

    mkdir(b"shm");

    root.create_symlink(kernel.kthreads.system_task(), b"fd", b"/proc/self/fd", FsCred::root())
        .unwrap();

    fs
}

pub fn devtmpfs_create_device(
    kernel: &Arc<Kernel>,
    device: DeviceMetadata,
) -> Result<DirEntryHandle, Errno> {
    let mode = match device.mode {
        DeviceMode::Char => mode!(IFCHR, 0o666),
        DeviceMode::Block => mode!(IFBLK, 0o666),
    };
    dev_tmp_fs(kernel).root().create_node(
        kernel.kthreads.system_task(),
        &device.name,
        mode,
        device.device_type,
        FsCred::root(),
    )
}

pub fn devtmpfs_mkdir(kernel: &Arc<Kernel>, name: &FsStr) -> Result<DirEntryHandle, Errno> {
    dev_tmp_fs(kernel).root().create_node(
        kernel.kthreads.system_task(),
        name,
        mode!(IFDIR, 0o755),
        DeviceType::NONE,
        FsCred::root(),
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
    dev_tmp_fs(kernel).root().create_symlink(
        kernel.kthreads.system_task(),
        name,
        target,
        FsCred::root(),
    )
}
