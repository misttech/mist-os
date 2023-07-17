// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{magma::MagmaFile, DeviceMode},
    fs::{
        kobject::{KObjectDeviceAttribute, KType},
        sysfs::SysFsDirectory,
        FileOps, FsNode,
    },
    task::{CurrentTask, Kernel},
    types::*,
};

use std::sync::Arc;

fn create_magma_device(
    current_task: &CurrentTask,
    id: DeviceType,
    node: &FsNode,
    flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    MagmaFile::new_file(current_task, id, node, flags)
}

pub fn magma_device_init(kernel: &Arc<Kernel>) {
    let starnix_class = kernel.device_registry.virtual_bus().get_or_create_child(
        b"starnix",
        KType::Class,
        SysFsDirectory::new,
    );

    let magma_type: DeviceType = kernel
        .device_registry
        .register_dyn_chrdev(create_magma_device)
        .expect("magma device register failed.");

    kernel.add_chr_device(
        starnix_class,
        KObjectDeviceAttribute::new(b"magma0", b"magma0", magma_type, DeviceMode::Char),
    );
}
