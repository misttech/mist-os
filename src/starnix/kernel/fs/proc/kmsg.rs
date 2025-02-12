// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, EventHandler, WaitCanceler, Waiter};
use crate::vfs::{
    fileops_impl_noop_sync, fileops_impl_seekless, FileObject, FileOps, FileSystemHandle,
    FsNodeHandle, FsNodeInfo, InputBuffer, OutputBuffer, SimpleFileNode,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{error, mode};

pub fn kmsg_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node(
        current_task,
        SimpleFileNode::new(|| Ok(KmsgFile)),
        FsNodeInfo::new_factory(mode!(IFREG, 0o100), FsCred::root()),
    )
}

struct KmsgFile;

impl FileOps for KmsgFile {
    fileops_impl_seekless!();
    fileops_impl_noop_sync!();

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let syslog = current_task.kernel().syslog.access(current_task).ok()?;
        Some(syslog.wait(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let syslog = current_task.kernel().syslog.access(current_task)?;
        let mut events = FdEvents::empty();
        if syslog.size_unread()? > 0 {
            events |= FdEvents::POLLIN;
        }
        Ok(events)
    }

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let syslog = current_task.kernel().syslog.access(current_task)?;
        file.blocking_op(locked, current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, |_| {
            let bytes_written = syslog.read(data)?;
            Ok(bytes_written as usize)
        })
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EIO)
    }
}
