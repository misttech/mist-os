// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::qbg_battery_file::create_battery_profile_device;
use super::qbg_file::{create_qbg_device, QbgClassDirectory};
use starnix_core::device::kobject::DeviceMetadata;
use starnix_core::device::DeviceMode;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_sync::{FileOpsCore, LockBefore, Locked};
use starnix_uapi::device_type::DeviceType;

pub fn hvdcp_opti_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;

    // /dev/qbg
    registry.register_device(
        locked,
        current_task,
        "qbg".into(),
        DeviceMetadata::new("qbg".into(), DeviceType::new(484, 0), DeviceMode::Char),
        registry.objects.get_or_create_class_with_ops(
            "qbg".into(),
            registry.objects.virtual_bus(),
            QbgClassDirectory::new,
        ),
        DeviceDirectory::new,
        create_qbg_device,
    );

    // /dev/qbg_battery
    registry.register_device(
        locked,
        current_task,
        "qbg_battery".into(),
        DeviceMetadata::new("qbg_battery".into(), DeviceType::new(485, 0), DeviceMode::Char),
        registry.objects.get_or_create_class("qbg_battery".into(), registry.objects.virtual_bus()),
        DeviceDirectory::new,
        create_battery_profile_device,
    );
}
