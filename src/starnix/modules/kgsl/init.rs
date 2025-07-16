// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::KgslFile;
use starnix_core::device::DeviceOps;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileOps, NamespaceNode};
use starnix_logging::log_info;
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;

#[derive(Clone)]
struct KgslDeviceBuilder {}

impl DeviceOps for KgslDeviceBuilder {
    fn open(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &NamespaceNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        log_info!("kgsl: open");
        KgslFile::new_file(current_task, id, &node.entry.node, flags)
    }
}

pub fn kgsl_device_init<L>(locked: &mut Locked<L>, current_task: &CurrentTask)
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    log_info!("kgsl: kgsl_device_init");

    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;
    let class = registry.objects.get_or_create_class("kgsl".into(), registry.objects.virtual_bus());
    let builder = KgslDeviceBuilder {};

    registry
        .register_dyn_device(locked, current_task, "kgsl-3d0".into(), class, builder)
        .expect("can register kgsl-3d0");
}
