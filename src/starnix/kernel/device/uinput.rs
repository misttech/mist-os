// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{fs::*, syscalls::*, task::*, types::*};

pub fn create_uinput_device(
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(UinputDevice::new()))
}

struct UinputDevice {}

impl UinputDevice {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileOps for UinputDevice {
    fileops_impl_seekless!();
    fileops_impl_dataless!();

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(file, current_task, request, arg)
    }
}
