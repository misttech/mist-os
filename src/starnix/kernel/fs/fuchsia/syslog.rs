// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    logging::log,
    syscalls::SyscallResult,
    task::*,
    types::*,
};

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
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        data.read_each(&mut |bytes| {
            log!(level = info, tag = "stdio", "{}", String::from_utf8_lossy(bytes));
            Ok(bytes.len())
        })
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
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
