// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::MagmaFile;
use starnix_core::{
    device::{kobject::DeviceMetadata, DeviceMode},
    fs::sysfs::DeviceDirectory,
    task::CurrentTask,
    vfs::{FileOps, FsNode},
};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::{device_type::DeviceType, errors::Errno, open_flags::OpenFlags};

fn create_magma_device(
    _locked: &mut Locked<'_, DeviceOpen>,
    current_task: &CurrentTask,
    id: DeviceType,
    node: &FsNode,
    flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    MagmaFile::new_file(current_task, id, node, flags)
}

pub fn magma_device_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;

    let starnix_class = registry.get_or_create_class("starnix".into(), registry.virtual_bus());

    let magma_type: DeviceType =
        registry.register_dyn_chrdev(create_magma_device).expect("magma device register failed.");

    registry.add_device(
        locked,
        current_task,
        "magma0".into(),
        DeviceMetadata::new("magma0".into(), magma_type, DeviceMode::Char),
        starnix_class,
        DeviceDirectory::new,
    );
}
