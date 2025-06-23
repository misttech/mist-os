// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::task::{CurrentTask, EventHandler, Syslog, SyslogAccess, WaitCanceler, Waiter};
use starnix_core::vfs::{
    fileops_impl_noop_sync, fileops_impl_seekless, fs_node_impl_not_dir, AppendLockGuard,
    CheckAccessReason, FileObject, FileOps, FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, InputBuffer, OutputBuffer,
};
use starnix_sync::{FileOpsCore, Locked, RwLock};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::Access;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::syslog::SyslogAction;
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{error, mode};

pub fn kmsg_file(fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node_and_allocate_node_id(
        KmsgNode,
        FsNodeInfo::new(mode!(IFREG, 0o400), FsCred::root()),
    )
}

struct KmsgNode;

impl FsNodeOps for KmsgNode {
    fs_node_impl_not_dir!();

    fn check_access(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        access: Access,
        info: &RwLock<FsNodeInfo>,
        reason: CheckAccessReason,
    ) -> Result<(), Errno> {
        node.default_check_access_impl(current_task, access, reason, info.read())?;
        Syslog::validate_access(current_task, SyslogAccess::ProcKmsg(SyslogAction::Open))
    }

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(KmsgFile))
    }

    fn truncate(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        // O_TRUNC succeeds on /proc/kmsg but does nothing.
        Ok(())
    }
}

struct KmsgFile;

impl FileOps for KmsgFile {
    fileops_impl_seekless!();
    fileops_impl_noop_sync!();

    fn wait_async(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let syslog = current_task
            .kernel()
            .syslog
            .access(current_task, SyslogAccess::ProcKmsg(SyslogAction::SizeUnread))
            .ok()?;
        Some(syslog.wait(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let syslog = current_task
            .kernel()
            .syslog
            .access(current_task, SyslogAccess::ProcKmsg(SyslogAction::SizeUnread))?;
        let mut events = FdEvents::empty();
        if syslog.size_unread()? > 0 {
            events |= FdEvents::POLLIN;
        }
        Ok(events)
    }

    fn read(
        &self,
        locked: &mut Locked<FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let syslog = current_task
            .kernel()
            .syslog
            .access(current_task, SyslogAccess::ProcKmsg(SyslogAction::Read))?;
        file.blocking_op(locked, current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, |_| {
            let bytes_written = syslog.read(data)?;
            Ok(bytes_written as usize)
        })
    }

    fn write(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EIO)
    }
}
