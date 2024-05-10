// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        device_mapper::create_device_mapper, kobject::DeviceMetadata,
        loop_device::create_loop_control_device, mem::DevRandom, simple_device_ops, DeviceMode,
    },
    device::{
        device_mapper::device_mapper_init, loop_device::loop_device_init, mem::mem_device_init,
        zram::zram_device_init,
    },
    fs::devpts::tty_device_init,
    fs::sysfs::DeviceDirectory,
    task::CurrentTask,
    vfs::fuse::open_fuse_device,
};
use starnix_sync::{FileOpsCore, LockBefore, Locked, Unlocked};
use starnix_uapi::device_type::DeviceType;

pub fn misc_device_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;
    let misc_class = registry.get_or_create_class("misc".into(), registry.virtual_bus());
    registry.add_and_register_device(
        locked,
        current_task,
        // TODO(https://fxbug.dev/322365477) consider making this configurable
        "hw_random".into(),
        DeviceMetadata::new("hw_random".into(), DeviceType::HW_RANDOM, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevRandom>,
    );
    registry.add_and_register_device(
        locked,
        current_task,
        "fuse".into(),
        DeviceMetadata::new("fuse".into(), DeviceType::FUSE, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        open_fuse_device,
    );
    registry.add_and_register_device(
        locked,
        current_task,
        "device-mapper".into(),
        DeviceMetadata::new("mapper/control".into(), DeviceType::DEVICE_MAPPER, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        create_device_mapper,
    );
    registry.add_and_register_device(
        locked,
        current_task,
        "loop-control".into(),
        DeviceMetadata::new("loop-control".into(), DeviceType::LOOP_CONTROL, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        create_loop_control_device,
    );
}

/// Initializes common devices in `Kernel`.
///
/// Adding device nodes to devtmpfs requires the current running task. The `Kernel` constructor does
/// not create an initial task, so this function should be triggered after a `CurrentTask` has been
/// initialized.
pub fn init_common_devices(locked: &mut Locked<'_, Unlocked>, system_task: &CurrentTask) {
    misc_device_init(locked, system_task);
    mem_device_init(locked, system_task);
    tty_device_init(locked, system_task);
    loop_device_init(locked, system_task);
    device_mapper_init(system_task);
    zram_device_init(locked, system_task);
}
