// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        create_unknown_device, kobject::DeviceMetadata, loop_device::create_loop_control_device,
        mem::DevRandom, simple_device_ops, uinput::create_uinput_device, DeviceMode,
    },
    fs::sysfs::DeviceDirectory,
    task::CurrentTask,
    vfs::fuse::DevFuse,
};
use starnix_uapi::device_type::DeviceType;

pub fn misc_device_init(current_task: &CurrentTask) {
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;
    let misc_class = registry.get_or_create_class(b"misc", registry.virtual_bus());
    registry.add_and_register_device(
        current_task,
        b"hwrng",
        DeviceMetadata::new(b"hwrng", DeviceType::HW_RANDOM, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevRandom>,
    );
    registry.add_and_register_device(
        current_task,
        b"fuse",
        DeviceMetadata::new(b"fuse", DeviceType::FUSE, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevFuse>,
    );
    registry.add_and_register_device(
        current_task,
        b"device-mapper",
        DeviceMetadata::new(b"mapper/control", DeviceType::DEVICE_MAPPER, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        create_unknown_device,
    );
    registry.add_and_register_device(
        current_task,
        b"loop-control",
        DeviceMetadata::new(b"loop-control", DeviceType::LOOP_CONTROL, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        create_loop_control_device,
    );
    registry.add_and_register_device(
        current_task,
        b"uinput",
        DeviceMetadata::new(b"uinput", DeviceType::UINPUT, DeviceMode::Char),
        misc_class,
        DeviceDirectory::new,
        create_uinput_device,
    );
}
