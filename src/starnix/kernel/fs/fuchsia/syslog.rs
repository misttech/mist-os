// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::logging::log;
use crate::syscalls::SyscallResult;
use crate::task::*;
use crate::types::*;

pub struct SyslogFile;

impl SyslogFile {
    pub fn new_file(current_task: &CurrentTask) -> FileHandle {
        Anon::new_file(current_task, Box::new(SyslogFile), OpenFlags::RDWR)
    }
}

impl FileOps for SyslogFile {
    fileops_impl_nonseekable!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        data.read_each(&mut |bytes| {
            log!(level = info, tag = "stdio", "{}", String::from_utf8_lossy(bytes));
            Ok(bytes.len())
        })
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        request: u32,
        _user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(request)
    }
}
