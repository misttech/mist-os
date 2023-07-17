// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{loopback::LoopControlDevice, mem::DevRandom, DeviceMode},
    fs::{
        fuse::DevFuse,
        kobject::{KObjectDeviceAttribute, KType},
        sysfs::SysFsDirectory,
        *,
    },
    task::*,
    types::*,
};

use std::sync::Arc;

fn create_misc_device(
    current_task: &CurrentTask,
    id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(match id {
        DeviceType::HW_RANDOM => Box::new(DevRandom),
        DeviceType::FUSE => Box::<DevFuse>::default(),
        DeviceType::LOOP_CONTROL => {
            Box::new(LoopControlDevice::new(current_task.kernel().loop_device_registry.clone()))
        }
        _ => return error!(ENODEV),
    })
}

pub fn misc_device_init(kernel: &Arc<Kernel>) {
    kernel
        .device_registry
        .register_chrdev_major(MISC_MAJOR, create_misc_device)
        .expect("misc device register failed.");

    let device_attrs = KObjectDeviceAttribute::new_from_vec(
        vec![
            (b"hwrng", b"hwrng", DeviceType::HW_RANDOM),
            (b"fuse", b"fuse", DeviceType::FUSE),
            (b"device-mapper", b"mapper/control", DeviceType::DEVICE_MAPPER),
            (b"loop-control", b"loop-control", DeviceType::LOOP_CONTROL),
        ],
        DeviceMode::Char,
    );
    let misc_class = kernel.device_registry.virtual_bus().get_or_create_child(
        b"misc",
        KType::Class,
        SysFsDirectory::new,
    );
    kernel.add_chr_devices(misc_class, device_attrs);
}
