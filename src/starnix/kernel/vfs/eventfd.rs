// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_sync::Mutex;

use crate::task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter};
use crate::vfs::buffers::{InputBuffer, InputBufferExt as _, OutputBuffer};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FileHandle, FileObject, FileOps,
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::FdEvents;

const DATA_SIZE: usize = 8;

pub enum EventFdType {
    Counter,
    Semaphore,
}

/// The eventfd file object has two modes of operation:
/// 1) Counter: Write adds to the value and read returns the value while setting it to zero.
/// 2) Semaphore: Write adds one to the counter and read decrements it and returns 1.
/// In both cases, if the value is 0, the read blocks or returns EAGAIN.
/// See https://man7.org/linux/man-pages/man2/eventfd.2.html

struct EventFdInner {
    value: u64,
    wait_queue: WaitQueue,
}

pub struct EventFdFileObject {
    inner: Mutex<EventFdInner>,
    eventfd_type: EventFdType,
}

pub fn new_eventfd(
    current_task: &CurrentTask,
    value: u32,
    eventfd_type: EventFdType,
    blocking: bool,
) -> FileHandle {
    let open_flags = if blocking { OpenFlags::RDWR } else { OpenFlags::RDWR | OpenFlags::NONBLOCK };
    Anon::new_file(
        current_task,
        Box::new(EventFdFileObject {
            inner: Mutex::new(EventFdInner {
                value: value.into(),
                wait_queue: WaitQueue::default(),
            }),
            eventfd_type,
        }),
        open_flags,
        "[eventfd]",
    )
}

impl FileOps for EventFdFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn write(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(locked, current_task, FdEvents::POLLOUT | FdEvents::POLLHUP, None, |_| {
            let written_data = data.read_to_array::<DATA_SIZE>()?;
            let add_value = u64::from_ne_bytes(written_data);
            if add_value == u64::MAX {
                return error!(EINVAL);
            }

            // The maximum value of the counter is u64::MAX - 1
            let mut inner = self.inner.lock();
            let headroom = u64::MAX - inner.value - 1;
            if headroom < add_value {
                return error!(EAGAIN);
            }
            inner.value += add_value;
            if inner.value > 0 {
                inner.wait_queue.notify_fd_events(FdEvents::POLLIN);
            }
            Ok(DATA_SIZE)
        })
    }

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(locked, current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, |_| {
            if data.available() < DATA_SIZE {
                return error!(EINVAL);
            }

            let mut inner = self.inner.lock();
            if inner.value == 0 {
                return error!(EAGAIN);
            }

            let return_value = match self.eventfd_type {
                EventFdType::Counter => {
                    let start_value = inner.value;
                    inner.value = 0;
                    start_value
                }
                EventFdType::Semaphore => {
                    inner.value -= 1;
                    1
                }
            };
            data.write_all(&return_value.to_ne_bytes())?;
            inner.wait_queue.notify_fd_events(FdEvents::POLLOUT);

            Ok(DATA_SIZE)
        })
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
        Some(self.inner.lock().wait_queue.wait_async_fd_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let inner = self.inner.lock();
        // TODO check for error and HUP events
        let mut events = FdEvents::empty();
        if inner.value > 0 {
            events |= FdEvents::POLLIN;
        }
        if inner.value < u64::MAX - 1 {
            events |= FdEvents::POLLOUT;
        }
        Ok(events)
    }
}
