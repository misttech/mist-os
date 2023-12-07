// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        create_unknown_device, kobject::KObjectDeviceAttribute,
        loop_device::create_loop_control_device, mem::DevRandom, simple_device_ops,
        uinput::create_uinput_device, DeviceMode,
    },
    task::CurrentTask,
    vfs::fuse::DevFuse,
};
use starnix_uapi::device_type::DeviceType;

pub fn misc_device_init(current_task: &CurrentTask) {
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;
    let misc_class = registry.add_class(b"misc", registry.virtual_bus());
    registry.add_and_register_device(
        current_task,
        KObjectDeviceAttribute::new(
            None,
            misc_class.clone(),
            b"hwrng",
            b"hwrng",
            DeviceType::HW_RANDOM,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevRandom>,
    );
    registry.add_and_register_device(
        current_task,
        KObjectDeviceAttribute::new(
            None,
            misc_class.clone(),
            b"fuse",
            b"fuse",
            DeviceType::FUSE,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevFuse>,
    );
    registry.add_and_register_device(
        current_task,
        KObjectDeviceAttribute::new(
            None,
            misc_class.clone(),
            b"device-mapper",
            b"mapper/control",
            DeviceType::DEVICE_MAPPER,
            DeviceMode::Char,
        ),
        create_unknown_device,
    );
    registry.add_and_register_device(
        current_task,
        KObjectDeviceAttribute::new(
            None,
            misc_class.clone(),
            b"loop-control",
            b"loop-control",
            DeviceType::LOOP_CONTROL,
            DeviceMode::Char,
        ),
        create_loop_control_device,
    );
    registry.add_and_register_device(
        current_task,
        KObjectDeviceAttribute::new(
            None,
            misc_class,
            b"uinput",
            b"uinput",
            DeviceType::UINPUT,
            DeviceMode::Char,
        ),
        create_uinput_device,
    );
}
