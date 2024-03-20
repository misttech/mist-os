// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::{
    device::{kobject::DeviceMetadata, DeviceMode},
    fs::sysfs::DeviceDirectory,
    task::CurrentTask,
    vfs::{FileOps, FsNode},
};
use starnix_logging::log_error;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::{device_type::DeviceType, error, errors::Errno, open_flags::OpenFlags};

fn create_gpu_device(
    _locked: &mut Locked<'_, DeviceOpen>,
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    log_error!("virtio-gpu unsupported");
    error!(ENOTSUP)
}

pub fn gpu_device_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;

    let starnix_class = registry.get_or_create_class("starnix".into(), registry.virtual_bus());

    let gpu_type: DeviceType =
        registry.register_dyn_chrdev(create_gpu_device).expect("gpu device register failed.");

    registry.add_device(
        locked,
        current_task,
        "virtio-gpu".into(),
        DeviceMetadata::new("virtio-gpu".into(), gpu_type, DeviceMode::Char),
        starnix_class,
        DeviceDirectory::new,
    );
}
