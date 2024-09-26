// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FileHandle,
    FileOps,
};
use starnix_uapi::errors::Errno;
use starnix_uapi::io_uring_params;
use starnix_uapi::open_flags::OpenFlags;

#[derive(Default)]
pub struct IoUringFileObject {}

impl IoUringFileObject {
    pub fn new_file(
        current_task: &CurrentTask,
        _entries: u32,
        _user_params: io_uring_params,
    ) -> Result<FileHandle, Errno> {
        let object = Box::new(IoUringFileObject::default());
        Ok(Anon::new_file(current_task, object, OpenFlags::RDWR))
    }
}

impl FileOps for IoUringFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();
    fileops_impl_dataless!();
}
