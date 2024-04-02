// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        default_ioctl, fileops_impl_nonseekable, Anon, FileHandle, FileObject, FileOps,
    },
};
use starnix_logging::log_info;
use starnix_sync::{FileOpsCore, Locked, Unlocked, WriteOps};
use starnix_syscalls::{SyscallArg, SyscallResult};
use starnix_uapi::{errors::Errno, open_flags::OpenFlags};

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
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        data.read_each(&mut |bytes| {
            log_info!(tag = "stdio", "{}", String::from_utf8_lossy(bytes));
            Ok(bytes.len())
        })
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(file, current_task, request, arg)
    }
}
