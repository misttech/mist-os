// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{
    CurrentTask, EventHandler, SignalHandler, SignalHandlerInner, ThreadGroup, WaitCanceler, Waiter,
};
use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FileHandle,
    FileObject, FileOps,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::pid_t;
use starnix_uapi::vfs::FdEvents;
use std::sync::Arc;
use zx::{self as zx, AsHandleRef};

pub struct PidFdFileObject {
    // In principle, we need some way to designate a Task that is durable for
    // the lifetime of the `PidFdFileObject`. In practice, we never actually
    // reuse pids and have no mechanism for tracking which pids have been freed.
    //
    // For now, we designate the Task using the pid itself. If/when we start
    // reusing pids, we'll need to reconsider this design.
    //
    // See `PidTable::allocate_pid` for a related comment.
    pid: pid_t,

    // Receives a notification when the tracked process terminates.
    terminated_event: Arc<zx::EventPair>,
}

impl PidFdFileObject {
    fn get_signals_from_events(events: FdEvents) -> zx::Signals {
        if events.contains(FdEvents::POLLIN) {
            zx::Signals::EVENTPAIR_PEER_CLOSED
        } else {
            zx::Signals::NONE
        }
    }

    fn get_events_from_signals(signals: zx::Signals) -> FdEvents {
        let mut events = FdEvents::empty();

        if signals.contains(zx::Signals::EVENTPAIR_PEER_CLOSED) {
            events |= FdEvents::POLLIN;
        }

        events
    }
}

pub fn new_pidfd(current_task: &CurrentTask, proc: &ThreadGroup, flags: OpenFlags) -> FileHandle {
    Anon::new_file(
        current_task,
        Box::new(PidFdFileObject {
            pid: proc.leader,
            terminated_event: Arc::new(proc.drop_notifier.event()),
        }),
        flags,
    )
}

impl FileOps for PidFdFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_dataless!();
    fileops_impl_noop_sync!();

    fn as_pid(&self, _file: &FileObject) -> Result<pid_t, Errno> {
        Ok(self.pid)
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let signal_handler = SignalHandler {
            inner: SignalHandlerInner::ZxHandle(PidFdFileObject::get_events_from_signals),
            event_handler: handler,
            err_code: None,
        };
        let canceler = waiter
            .wake_on_zircon_signals(
                self.terminated_event.as_ref(),
                PidFdFileObject::get_signals_from_events(events),
                signal_handler,
            )
            .unwrap(); // errors cannot happen unless the kernel is out of memory
        Some(WaitCanceler::new_event_pair(Arc::downgrade(&self.terminated_event), canceler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        match self
            .terminated_event
            .wait_handle(zx::Signals::EVENTPAIR_PEER_CLOSED, zx::MonotonicInstant::ZERO)
        {
            Err(zx::Status::TIMED_OUT) => Ok(FdEvents::empty()),
            Ok(zx::Signals::EVENTPAIR_PEER_CLOSED) => Ok(FdEvents::POLLIN),
            result => unreachable!("unexpected result: {result:?}"),
        }
    }
}
