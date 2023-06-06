// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    logging::not_implemented,
    task::{CurrentTask, EventHandler, WaitCanceler, Waiter},
    types::*,
};
use fuchsia_zircon as zx;

pub struct InotifyFileObject {}

impl InotifyFileObject {
    /// Allocate a new, empty inotify object.
    pub fn new_file(current_task: &CurrentTask, non_blocking: bool) -> FileHandle {
        let flags =
            OpenFlags::RDONLY | if non_blocking { OpenFlags::NONBLOCK } else { OpenFlags::empty() };
        Anon::new_file(current_task, Box::new(InotifyFileObject {}), flags)
    }
}

impl FileOps for InotifyFileObject {
    fileops_impl_nonseekable!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EINVAL)
    }

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        not_implemented!("InotifyFileObject.read() is stubbed.");
        Waiter::new().wait_until(current_task, zx::Time::INFINITE)?;
        error!(EAGAIN)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(waiter.fake_wait())
    }

    fn query_events(&self, _current_task: &CurrentTask) -> FdEvents {
        FdEvents::empty()
    }
}
