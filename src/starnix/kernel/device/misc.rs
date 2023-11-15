// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        create_unknown_device, loop_device::create_loop_control_device, mem::DevRandom,
        simple_device_ops, uinput::create_uinput_device, DeviceMode,
    },
    fs::{fuse::DevFuse, kobject::KObjectDeviceAttribute},
    task::Kernel,
    types::DeviceType,
};

use std::sync::Arc;

pub fn misc_device_init(kernel: &Arc<Kernel>) {
    let misc_class =
        kernel.device_registry.add_class(b"misc", kernel.device_registry.virtual_bus());
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            misc_class.clone(),
            b"hwrng",
            b"hwrng",
            DeviceType::HW_RANDOM,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevRandom>,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            misc_class.clone(),
            b"fuse",
            b"fuse",
            DeviceType::FUSE,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevFuse>,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            misc_class.clone(),
            b"device-mapper",
            b"mapper/control",
            DeviceType::DEVICE_MAPPER,
            DeviceMode::Char,
        ),
        create_unknown_device,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            misc_class.clone(),
            b"loop-control",
            b"loop-control",
            DeviceType::LOOP_CONTROL,
            DeviceMode::Char,
        ),
        create_loop_control_device,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            misc_class,
            b"uinput",
            b"uinput",
            DeviceType::UINPUT,
            DeviceMode::Char,
        ),
        create_uinput_device,
    );
}
