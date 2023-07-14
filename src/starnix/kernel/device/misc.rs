// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::loopback::LoopControlDevice, device::mem::DevRandom, fs::fuse::DevFuse, fs::*, task::*,
    types::*,
};

pub fn create_misc_device(
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
