// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{simple_device_ops, DeviceMode},
    fs::{fileops_impl_dataless, fileops_impl_seekless, kobject::KObjectDeviceAttribute, FileOps},
    task::CurrentTask,
};
use starnix_uapi::device_type::{DeviceType, ZRAM_MAJOR};

#[derive(Default)]
pub struct DevZram;

impl FileOps for DevZram {
    fileops_impl_seekless!();
    fileops_impl_dataless!();
}

pub fn zram_device_init(system_task: &CurrentTask) {
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let virtual_block_class = registry.add_class(b"block", registry.virtual_bus());
    registry.add_and_register_device(
        system_task,
        KObjectDeviceAttribute::new(
            virtual_block_class,
            b"zram0",
            b"zram0",
            DeviceType::new(ZRAM_MAJOR, 0),
            DeviceMode::Block,
        ),
        simple_device_ops::<DevZram>,
    );
}
