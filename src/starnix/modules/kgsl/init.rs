// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::KgslFile;
use starnix_core::device::DeviceOps;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileOps, FsNode};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;

#[derive(Clone)]
struct KgslDeviceBuilder {}

impl DeviceOps for KgslDeviceBuilder {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        KgslFile::new_file(current_task, id, node, flags)
    }
}

pub fn kgsl_device_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;
    let builder = KgslDeviceBuilder {};

    registry
        .register_dyn_device(
            locked,
            current_task,
            "kgsl-3d0".into(),
            registry.objects.graphics_class(),
            DeviceDirectory::new,
            builder,
        )
        .expect("can register kgsl-3d0");
}
