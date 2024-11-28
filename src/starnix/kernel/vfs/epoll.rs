// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{
    CurrentTask, EnqueueEventHandler, EventHandler, ReadyItem, ReadyItemKey, WaitCanceler,
    WaitQueue, Waiter,
};
use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FileHandle,
    FileObject, FileOps, WeakFileHandle,
};

use itertools::Itertools;
use starnix_logging::log_warn;
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked, Mutex};
use starnix_uapi::errors::{Errno, EBADF, EINTR, ETIMEDOUT};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::{EpollEvent, FdEvents};
use starnix_uapi::{errno, error};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Maximum depth of epoll instances monitoring one another.
/// From https://man7.org/linux/man-pages/man2/epoll_ctl.2.html
const MAX_NESTED_DEPTH: u32 = 5;

/// WaitObject represents a FileHandle that is being waited upon.
/// The `data` field is a user defined quantity passed in
/// via `sys_epoll_ctl`. Typically C programs could use this
/// to store a pointer to the data that needs to be processed
/// after an event.
struct WaitObject {
    target: WeakFileHandle,
    events: FdEvents,
    data: u64,
    wait_canceler: Option<WaitCanceler>,
}

impl WaitObject {
    // TODO(https://fxbug.dev/42142887) we should not report an error if the file was closed while it was
    // registered for epoll(). Either the file needs to be removed from our lists when it is closed,
    // we need to ignore/remove WaitObjects when the file is gone, or (more likely) both because of
    // race conditions removing the file object.
    fn target(&self) -> Result<FileHandle, Errno> {
        self.target.upgrade().ok_or_else(|| errno!(EBADF))
    }
}

/// EpollKey acts as an key to a map of WaitObject.
/// In reality it is a pointer to a FileHandle object.
pub type EpollKey = usize;

fn as_epoll_key(file: &FileHandle) -> EpollKey {
    Arc::as_ptr(file) as EpollKey
}

/// EpollFileObject represents the FileObject used to
/// implement epoll_create1/epoll_ctl/epoll_pwait.
#[derive(Default)]
pub struct EpollFileObject {
    waiter: Waiter,
    /// Mutable state of this epoll object.
    state: Mutex<EpollState>,
    /// trigger_list is a FIFO of events that have
    /// happened, but have not yet been processed.
    trigger_list: Arc<Mutex<VecDeque<ReadyItem>>>,
}

#[derive(Default)]
struct EpollState {
    /// Any file tracked by this epoll instance
    /// will exist as a key in `wait_objects`.
    wait_objects: HashMap<ReadyItemKey, WaitObject>,
    /// processing_list is a FIFO of events that are being
    /// processed.
    ///
    /// Objects from the `EpollFileObject`'s `trigger_list` are moved into this
    /// list so that we can handle triggered events without holding its lock
    /// longer than we need to. This reduces contention with waited-on objects
    /// that tries to notify this epoll object on subscribed events.
    processing_list: VecDeque<ReadyItem>,
    /// rearm_list is the list of event that need to
    /// be waited upon prior to actually waiting in
    /// EpollFileObject::wait. They cannot be re-armed
    /// before that, because, if the client process has
    /// not cleared the wait condition, they would just
    /// be immediately triggered.
    rearm_list: Vec<ReadyItem>,
    /// A list of waiters waiting for events from this
    /// epoll instance.
    waiters: WaitQueue,
}

impl EpollFileObject {
    /// Allocate a new, empty epoll object.
    pub fn new_file(current_task: &CurrentTask) -> FileHandle {
        let epoll = Box::new(EpollFileObject::default());

        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = epoll.state.lock();
            let _l2 = epoll.trigger_list.lock();
        }

        Anon::new_file(current_task, epoll, OpenFlags::RDWR)
    }

    fn new_wait_handler(&self, key: ReadyItemKey) -> EventHandler {
        EventHandler::Enqueue(EnqueueEventHandler {
            key,
            queue: self.trigger_list.clone(),
            sought_events: FdEvents::all(),
            mappings: Default::default(),
        })
    }

    fn wait_on_file<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        key: ReadyItemKey,
        wait_object: &mut WaitObject,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let target = wait_object.target()?;

        // First start the wait. If an event happens after this, we'll get it.
        self.wait_on_file_edge_triggered(locked, current_task, key, wait_object)?;

        // Now check the events. If an event happened before this, we'll detect it here. There's
        // now no race window where an event would be missed.
        //
        // That said, if an event happens between the wait and the query_events, we'll get two
        // notifications. We handle this by deduping on the epoll_wait end.
        let events = target.query_events(locked, current_task)?;
        if !(events & wait_object.events).is_empty() {
            self.waiter.wake_immediately(events, self.new_wait_handler(key));
            if let Some(wait_canceler) = wait_object.wait_canceler.take() {
                wait_canceler.cancel();
            } else {
                log_warn!("wait canceler should have been set by `wait_on_file_edge_triggered`");
            }
        }
        Ok(())
    }

    fn wait_on_file_edge_triggered<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        key: ReadyItemKey,
        wait_object: &mut WaitObject,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        wait_object.wait_canceler = wait_object.target()?.wait_async(
            locked,
            current_task,
            &self.waiter,
            wait_object.events,
            self.new_wait_handler(key),
        );
        if wait_object.wait_canceler.is_none() {
            return error!(EPERM);
        }
        Ok(())
    }

    /// Checks if this EpollFileObject monitors the `epoll_file_object` at `epoll_file_handle`.
    fn check_monitors(&self, epoll_file_handle: &FileHandle, depth_left: u32) -> Result<(), Errno> {
        if depth_left == 0 {
            return error!(EINVAL);
        }

        let state = self.state.lock();
        for nested_object in state.wait_objects.values() {
            match nested_object.target()?.downcast_file::<EpollFileObject>() {
                None => continue,
                Some(target) => {
                    if Arc::ptr_eq(&nested_object.target()?, epoll_file_handle) {
                        return error!(ELOOP);
                    }
                    target.check_monitors(epoll_file_handle, depth_left - 1)?;
                }
            }
        }

        Ok(())
    }

    /// Asynchronously wait on certain events happening on a FileHandle.
    pub fn add<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        file: &FileHandle,
        epoll_file_handle: &FileHandle,
        epoll_event: EpollEvent,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // Check if adding this file would cause a cycle at a max depth of 5.
        if let Some(epoll_to_add) = file.downcast_file::<EpollFileObject>() {
            // We need to check for `MAX_NESTED_DEPTH - 1` because adding `epoll_to_add` to self
            // would result in a total depth of one more.
            epoll_to_add.check_monitors(epoll_file_handle, MAX_NESTED_DEPTH - 1)?;
        }

        let mut state = self.state.lock();
        let key = as_epoll_key(file).into();
        match state.wait_objects.entry(key) {
            Entry::Occupied(_) => error!(EEXIST),
            Entry::Vacant(entry) => {
                let wait_object = entry.insert(WaitObject {
                    target: Arc::downgrade(file),
                    events: epoll_event.events() | FdEvents::POLLHUP | FdEvents::POLLERR,
                    data: epoll_event.data(),
                    wait_canceler: None,
                });
                self.wait_on_file(locked, current_task, key, wait_object)
            }
        }
    }

    /// Modify the events we are looking for on a Filehandle.
    pub fn modify<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        file: &FileHandle,
        epoll_event: EpollEvent,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut state = self.state.lock();
        let key = as_epoll_key(file);
        state.rearm_list.retain(|x| x.key != key.into());
        match state.wait_objects.entry(key.into()) {
            Entry::Occupied(mut entry) => {
                let wait_object = entry.get_mut();
                if let Some(wait_canceler) = wait_object.wait_canceler.take() {
                    wait_canceler.cancel();
                }
                wait_object.events = epoll_event.events() | FdEvents::POLLHUP | FdEvents::POLLERR;
                // If the new epoll event doesn't include EPOLLWAKEUP, we need to take down the
                // wake lease. This ensures that the system doesn't stay awake unnecessarily when
                // the event no longer requires it to be awake.
                if wait_object.events.contains(FdEvents::EPOLLWAKEUP)
                    && !epoll_event.events().contains(FdEvents::EPOLLWAKEUP)
                {
                    current_task.kernel().suspend_resume_manager.remove_epoll(key);
                }
                self.wait_on_file(locked, current_task, key.into(), wait_object)
            }
            Entry::Vacant(_) => error!(ENOENT),
        }
    }

    /// Cancel an asynchronous wait on an object. Events triggered before
    /// calling this will still be delivered.
    pub fn delete(&self, file: &FileHandle) -> Result<(), Errno> {
        let mut state = self.state.lock();
        let key = as_epoll_key(file).into();
        if let Some(mut wait_object) = state.wait_objects.remove(&key) {
            if let Some(wait_canceler) = wait_object.wait_canceler.take() {
                wait_canceler.cancel();
            }
            state.rearm_list.retain(|x| x.key != key);
            Ok(())
        } else {
            error!(ENOENT)
        }
    }

    /// Stores events from the Epoll's trigger list to the parameter `pending_list`. This does not
    /// actually invoke the waiter which is how items are added to the trigger list. The caller
    /// will have to do that before calling if needed.
    ///
    /// If an event in the trigger list is stale, the event will be re-added to the waiter.
    ///
    /// Returns true if any events were added. False means there was nothing in the trigger list.
    fn process_triggered_events<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        pending_list: &mut Vec<ReadyItem>,
        max_events: usize,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut state = self.state.lock();
        // Move all the elements from `self.trigger_list` to this intermediary
        // queue that we handle events from. This reduces the time spent holding
        // `self.trigger_list`'s lock which reduces contention with objects that
        // this epoll object has subscribed for notifications from.
        state.processing_list.append(&mut *self.trigger_list.lock());
        while pending_list.len() < max_events && !state.processing_list.is_empty() {
            if let Some(pending) = state.processing_list.pop_front() {
                if let Some(wait) = state.wait_objects.get_mut(&pending.key) {
                    // The weak pointer to the FileObject target can be gone if the file was closed
                    // out from under us. If this happens it is not an error: ignore it and
                    // continue.
                    if let Some(target) = wait.target.upgrade() {
                        let ready = ReadyItem {
                            key: pending.key,
                            events: target.query_events(locked, current_task)?,
                        };
                        if ready.events.intersects(wait.events) {
                            pending_list.push(ready);
                        } else {
                            // Another thread already handled this event, wait for another one.
                            // Files can be legitimately closed out from under us so bad file
                            // descriptors are not an error.
                            match self.wait_on_file(locked, current_task, pending.key, wait) {
                                Err(err) if err == EBADF => {} // File closed.
                                Err(err) => return Err(err),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Waits until an event exists in `pending_list` or until `timeout` has
    /// been reached.
    fn wait_until_pending_event<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        max_events: usize,
        mut wait_deadline: zx::MonotonicInstant,
    ) -> Result<Vec<ReadyItem>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut pending_list = Vec::new();

        loop {
            self.process_triggered_events(locked, current_task, &mut pending_list, max_events)?;

            if pending_list.len() == max_events {
                break; // No input events or output list full, nothing more we can do.
            }

            if !pending_list.is_empty() {
                // We now know we have at least one event to return. We shouldn't return
                // immediately, in case there are more events available, but the next loop should
                // wait with a 0 timeout to prevent further blocking.
                wait_deadline = zx::MonotonicInstant::ZERO;
            }

            // Loop back to check if there are more items in the Waiter's queue. Every wait_until()
            // call will process a single event. In order to drain as many events as we can that
            // are synchronously available, keep trying until it reports empty.
            //
            // The handlers in the waits cause items to be appended to trigger_list. See the closure
            // in `wait_on_file` to see how this happens.
            //
            // This wait may return EINTR for nonzero timeouts which is not an error. We must be
            // careful not to lose events if this happens.
            //
            // The first time through this loop we'll use the timeout passed into this function so
            // can get EINTR. But since we haven't done anything or accumulated any results yet it's
            // OK to immediately return and no information will be lost.
            match self.waiter.wait_until(locked, current_task, wait_deadline) {
                Err(err) if err == ETIMEDOUT => break,
                Err(err) if err == EINTR => {
                    // Terminating early will lose any events in the pending_list so that should
                    // only be for unrecoverable errors (not EINTR). The only time there should be a
                    // nonzero wait_deadline (and hence the ability to encounter EINTR) is when the
                    // pending list is empty.
                    debug_assert!(
                        pending_list.is_empty(),
                        "Got EINTR from wait of {}ns with {} items pending.",
                        wait_deadline.into_nanos(),
                        pending_list.len()
                    );
                    return Err(err);
                }
                // TODO check if this is supposed to actually fail!
                result => result?,
            }
        }

        Ok(pending_list)
    }

    /// Blocking wait on all waited upon events with a timeout.
    pub fn wait<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        max_events: usize,
        deadline: zx::MonotonicInstant,
    ) -> Result<Vec<EpollEvent>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // First we start waiting again on wait objects that have
        // previously been triggered.
        {
            let mut state = self.state.lock();
            let rearm_list = std::mem::take(&mut state.rearm_list);
            for to_wait in rearm_list.iter() {
                // TODO handle interrupts here
                let w = state.wait_objects.get_mut(&to_wait.key).unwrap();
                if let ReadyItemKey::Usize(key) = to_wait.key {
                    current_task.kernel().suspend_resume_manager.remove_epoll(key)
                };
                self.wait_on_file(locked, current_task, to_wait.key, w)?;
            }
        }

        let pending_list =
            self.wait_until_pending_event(locked, current_task, max_events, deadline)?;

        // Process the pending list and add processed ReadyItem
        // entries to the rearm_list for the next wait.
        let mut result = vec![];
        let mut state = self.state.lock();
        for pending_event in pending_list.iter().unique_by(|e| e.key) {
            // The wait could have been deleted by here,
            // so ignore the None case.
            if let Some(wait) = state.wait_objects.get_mut(&pending_event.key) {
                let reported_events = pending_event.events & wait.events;
                result.push(EpollEvent::new(reported_events, wait.data));

                // Files marked with `EPOLLONESHOT` should only notify
                // once and need to be rearmed manually with epoll_ctl_mod().
                if wait.events.contains(FdEvents::EPOLLONESHOT) {
                    continue;
                }
                if wait.events.contains(FdEvents::EPOLLET) {
                    // The file can be closed while registered for epoll which is not an error.
                    // We do not expect other errors from waiting.
                    match self.wait_on_file_edge_triggered(
                        locked,
                        current_task,
                        pending_event.key,
                        wait,
                    ) {
                        Err(err) if err == EBADF => {} // File closed, ignore.
                        Err(err) => log_warn!("Unexpected wait result {:#?}", err),
                        _ => {}
                    }
                    continue;
                }
                // When this is the first time epoll_wait on this epoll fd, create and
                // hold a wake lease until the next epoll_wait.
                if wait.events.contains(FdEvents::EPOLLWAKEUP) {
                    if let ReadyItemKey::Usize(key) = pending_event.key {
                        current_task.kernel().suspend_resume_manager.add_epoll(key)
                    }
                }

                state.rearm_list.push(pending_event.clone());
            }
        }

        // Notify waiters of unprocessed events.
        if !state.processing_list.is_empty() || !self.trigger_list.lock().is_empty() {
            state.waiters.notify_fd_events(FdEvents::POLLIN);
        }

        Ok(result)
    }

    /// Drop the wake lease associated with the `file`.
    pub fn drop_lease(&self, current_task: &CurrentTask, file: &FileHandle) {
        let mut guard = self.state.lock();
        let key = as_epoll_key(file);
        if let Entry::Occupied(_) = guard.wait_objects.entry(key.into()) {
            current_task.kernel().suspend_resume_manager.remove_epoll(key);
        }
    }

    /// Activate the wake lease associated with the `file`.
    ///
    /// `baton_lease` is passed by reference to ensure that the lease remains on hold until
    /// this function returns.
    pub fn activate_lease(
        &self,
        current_task: &CurrentTask,
        file: &FileHandle,
        _baton_lease: &zx::Handle,
    ) -> Result<(), Errno> {
        let key = as_epoll_key(file);
        current_task.kernel().suspend_resume_manager.add_epoll(key);
        Ok(())
    }
}

impl FileOps for EpollFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();
    fileops_impl_dataless!();

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.state.lock().waiters.wait_async_fd_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let mut events = FdEvents::empty();
        let state = self.state.lock();
        if state.processing_list.is_empty() && self.trigger_list.lock().is_empty() {
            events |= FdEvents::POLLIN;
        }
        Ok(events)
    }

    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) {
        let guard = self.state.lock();
        for (key, _wait_object) in guard.wait_objects.iter() {
            if let ReadyItemKey::Usize(key) = key {
                current_task.kernel().suspend_resume_manager.remove_epoll(*key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::fuchsia::create_fuchsia_pipe;
    use crate::task::Waiter;
    use crate::testing::{create_kernel_task_and_unlocked, create_task};
    use crate::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
    use crate::vfs::eventfd::{new_eventfd, EventFdType};
    use crate::vfs::pipe::new_pipe;
    use crate::vfs::socket::{SocketDomain, SocketType, UnixSocket};
    use starnix_lifecycle::AtomicUsizeCounter;
    use starnix_sync::Unlocked;
    use starnix_uapi::vfs::{EpollEvent, FdEvents};
    use syncio::Zxio;
    use zx::{
        HandleBased, {self as zx},
    };

    #[::fuchsia::test]
    async fn test_epoll_read_ready() {
        static WRITE_COUNT: AtomicUsizeCounter = AtomicUsizeCounter::new(0);
        const EVENT_DATA: u64 = 42;

        let (kernel, _init_task, mut locked) = create_kernel_task_and_unlocked();
        let current_task = create_task(&mut locked, &kernel, "main-task");

        let (pipe_out, pipe_in) = new_pipe(&current_task).unwrap();

        let test_string = "hello starnix".to_string();
        let test_len = test_string.len();

        let epoll_file_handle = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();
        epoll_file
            .add(
                &mut locked,
                &current_task,
                &pipe_out,
                &epoll_file_handle,
                EpollEvent::new(FdEvents::POLLIN, EVENT_DATA),
            )
            .unwrap();

        let thread = kernel.kthreads.spawner().spawn_and_get_result({
            let test_string = test_string.clone();
            move |locked, task| {
                let bytes_written = pipe_in
                    .write(locked, &task, &mut VecInputBuffer::new(test_string.as_bytes()))
                    .unwrap();
                assert_eq!(bytes_written, test_len);
                WRITE_COUNT.add(bytes_written);
            }
        });
        let events = epoll_file
            .wait(&mut locked, &current_task, 10, zx::MonotonicInstant::INFINITE)
            .unwrap();
        thread.await.expect("join");
        assert_eq!(1, events.len());
        let event = &events[0];
        assert!(event.events().contains(FdEvents::POLLIN));
        assert_eq!(event.data(), EVENT_DATA);

        let mut buffer = VecOutputBuffer::new(test_len);
        let bytes_read = pipe_out.read(&mut locked, &current_task, &mut buffer).unwrap();
        assert_eq!(bytes_read, WRITE_COUNT.get());
        assert_eq!(bytes_read, test_len);
        assert_eq!(buffer.data(), test_string.as_bytes());
    }

    #[::fuchsia::test]
    async fn test_epoll_ready_then_wait() {
        const EVENT_DATA: u64 = 42;

        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let (pipe_out, pipe_in) = new_pipe(&current_task).unwrap();

        let test_string = "hello starnix".to_string();
        let test_bytes = test_string.as_bytes();
        let test_len = test_bytes.len();

        assert_eq!(
            pipe_in
                .write(&mut locked, &current_task, &mut VecInputBuffer::new(test_bytes))
                .unwrap(),
            test_bytes.len()
        );

        let epoll_file_handle = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();
        epoll_file
            .add(
                &mut locked,
                &current_task,
                &pipe_out,
                &epoll_file_handle,
                EpollEvent::new(FdEvents::POLLIN, EVENT_DATA),
            )
            .unwrap();

        let events = epoll_file
            .wait(&mut locked, &current_task, 10, zx::MonotonicInstant::INFINITE)
            .unwrap();
        assert_eq!(1, events.len());
        let event = &events[0];
        assert!(event.events().contains(FdEvents::POLLIN));
        assert_eq!(event.data(), EVENT_DATA);

        let mut buffer = VecOutputBuffer::new(test_len);
        let bytes_read = pipe_out.read(&mut locked, &current_task, &mut buffer).unwrap();
        assert_eq!(bytes_read, test_len);
        assert_eq!(buffer.data(), test_bytes);
    }

    #[::fuchsia::test]
    async fn test_epoll_ctl_cancel() {
        for do_cancel in [true, false] {
            let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
            let event = new_eventfd(&current_task, 0, EventFdType::Counter, true);
            let waiter = Waiter::new();

            let epoll_file_handle = EpollFileObject::new_file(&current_task);
            let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();
            const EVENT_DATA: u64 = 42;
            epoll_file
                .add(
                    &mut locked,
                    &current_task,
                    &event,
                    &epoll_file_handle,
                    EpollEvent::new(FdEvents::POLLIN, EVENT_DATA),
                )
                .unwrap();

            if do_cancel {
                epoll_file.delete(&event).unwrap();
            }

            let wait_canceler = event
                .wait_async(
                    &mut locked,
                    &current_task,
                    &waiter,
                    FdEvents::POLLIN,
                    EventHandler::None,
                )
                .expect("wait_async");
            if do_cancel {
                wait_canceler.cancel();
            }

            let add_val = 1u64;

            assert_eq!(
                event
                    .write(
                        &mut locked,
                        &current_task,
                        &mut VecInputBuffer::new(&add_val.to_ne_bytes())
                    )
                    .unwrap(),
                std::mem::size_of::<u64>()
            );

            let events = epoll_file
                .wait(&mut locked, &current_task, 10, zx::MonotonicInstant::ZERO)
                .unwrap();

            if do_cancel {
                assert_eq!(0, events.len());
            } else {
                assert_eq!(1, events.len());
                let event = &events[0];
                assert!(event.events().contains(FdEvents::POLLIN));
                assert_eq!(event.data(), EVENT_DATA);
            }
        }
    }

    #[::fuchsia::test]
    async fn test_multiple_events() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (client1, server1) = zx::Socket::create_stream();
        let (client2, server2) = zx::Socket::create_stream();
        let pipe1 = create_fuchsia_pipe(&current_task, client1, OpenFlags::RDWR)
            .expect("create_fuchsia_pipe");
        let pipe2 = create_fuchsia_pipe(&current_task, client2, OpenFlags::RDWR)
            .expect("create_fuchsia_pipe");
        let server1_zxio = Zxio::create(server1.into_handle()).expect("Zxio::create");
        let server2_zxio = Zxio::create(server2.into_handle()).expect("Zxio::create");

        let poll = |locked: &mut Locked<'_, Unlocked>| {
            let epoll_object = EpollFileObject::new_file(&current_task);
            let epoll_file = epoll_object.downcast_file::<EpollFileObject>().unwrap();
            epoll_file
                .add(
                    locked,
                    &current_task,
                    &pipe1,
                    &epoll_object,
                    EpollEvent::new(FdEvents::POLLIN, 1),
                )
                .expect("epoll_file.add");
            epoll_file
                .add(
                    locked,
                    &current_task,
                    &pipe2,
                    &epoll_object,
                    EpollEvent::new(FdEvents::POLLIN, 2),
                )
                .expect("epoll_file.add");
            epoll_file.wait(locked, &current_task, 2, zx::MonotonicInstant::ZERO).expect("wait")
        };

        let fds = poll(&mut locked);
        assert!(fds.is_empty());

        assert_eq!(server1_zxio.write(&[0]).expect("write"), 1);

        let fds = poll(&mut locked);
        assert_eq!(fds.len(), 1);
        assert_eq!(fds[0].events(), FdEvents::POLLIN);
        assert_eq!(fds[0].data(), 1);
        assert_eq!(
            pipe1.read(&mut locked, &current_task, &mut VecOutputBuffer::new(64)).expect("read"),
            1
        );

        let fds = poll(&mut locked);
        assert!(fds.is_empty());

        assert_eq!(server2_zxio.write(&[0]).expect("write"), 1);

        let fds = poll(&mut locked);
        assert_eq!(fds.len(), 1);
        assert_eq!(fds[0].events(), FdEvents::POLLIN);
        assert_eq!(fds[0].data(), 2);
        assert_eq!(
            pipe2.read(&mut locked, &current_task, &mut VecOutputBuffer::new(64)).expect("read"),
            1
        );

        let fds = poll(&mut locked);
        assert!(fds.is_empty());
    }

    #[::fuchsia::test]
    async fn test_cancel_after_notify() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let event = new_eventfd(&current_task, 0, EventFdType::Counter, true);
        let epoll_file_handle = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();

        // Add a thing
        const EVENT_DATA: u64 = 42;
        epoll_file
            .add(
                &mut locked,
                &current_task,
                &event,
                &epoll_file_handle,
                EpollEvent::new(FdEvents::POLLIN, EVENT_DATA),
            )
            .unwrap();

        // Make the thing send a notification, wait for it
        let add_val = 1u64;
        assert_eq!(
            event
                .write(&mut locked, &current_task, &mut VecInputBuffer::new(&add_val.to_ne_bytes()))
                .unwrap(),
            std::mem::size_of::<u64>()
        );

        assert_eq!(
            epoll_file
                .wait(&mut locked, &current_task, 10, zx::MonotonicInstant::ZERO)
                .unwrap()
                .len(),
            1
        );

        // Remove the thing
        epoll_file.delete(&event).unwrap();

        // Wait for new notifications
        assert_eq!(
            epoll_file
                .wait(&mut locked, &current_task, 10, zx::MonotonicInstant::ZERO)
                .unwrap()
                .len(),
            0
        );
        // That shouldn't crash
    }

    #[::fuchsia::test]
    async fn test_add_then_modify() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let (socket1, _socket2) = UnixSocket::new_pair(
            &current_task,
            SocketDomain::Unix,
            SocketType::Stream,
            OpenFlags::RDWR,
        )
        .expect("Failed to create socket pair.");

        let epoll_file_handle = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();

        const EVENT_DATA: u64 = 42;
        epoll_file
            .add(
                &mut locked,
                &current_task,
                &socket1,
                &epoll_file_handle,
                EpollEvent::new(FdEvents::POLLIN, EVENT_DATA),
            )
            .unwrap();
        assert_eq!(
            epoll_file
                .wait(&mut locked, &current_task, 10, zx::MonotonicInstant::ZERO)
                .unwrap()
                .len(),
            0
        );

        let read_write_event = FdEvents::POLLIN | FdEvents::POLLOUT;
        epoll_file
            .modify(
                &mut locked,
                &current_task,
                &socket1,
                EpollEvent::new(read_write_event, EVENT_DATA),
            )
            .unwrap();
        let triggered_events =
            epoll_file.wait(&mut locked, &current_task, 10, zx::MonotonicInstant::ZERO).unwrap();
        assert_eq!(1, triggered_events.len());
        let event = &triggered_events[0];
        assert_eq!(event.events(), FdEvents::POLLOUT);
        assert_eq!(event.data(), EVENT_DATA);
    }
}
