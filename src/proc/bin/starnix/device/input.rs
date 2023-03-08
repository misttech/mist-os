// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceOps;
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::logging::*;
use crate::syscalls::SyscallResult;
use crate::task::CurrentTask;
use crate::types::*;

use std::sync::Arc;

pub struct InputFile {}

impl InputFile {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {})
    }
}

impl DeviceOps for Arc<InputFile> {
    fn open(
        &self,
        _current_task: &CurrentTask,
        _dev: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(Arc::clone(self)))
    }
}

impl FileOps for Arc<InputFile> {
    fileops_impl_nonseekable!();

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _request: u32,
        _user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        not_implemented!(current_task, "ioctl() on input device");
        error!(EOPNOTSUPP)
    }

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        not_implemented!(current_task, "read() on input device");
        error!(EOPNOTSUPP)
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        not_implemented!(current_task, "write() on input device");
        error!(EOPNOTSUPP)
    }
}
