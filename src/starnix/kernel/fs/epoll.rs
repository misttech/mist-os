// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use itertools::Itertools;
use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    sync::{Arc, Weak},
};

use crate::{
    arch::uapi::epoll_event,
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    lock::RwLock,
    logging::*,
    task::*,
    types::*,
};

/// Maximum depth of epoll instances monitoring one another.
/// From https://man7.org/linux/man-pages/man2/epoll_ctl.2.html
const MAX_NESTED_DEPTH: u32 = 5;

/// WaitObject represents a FileHandle that is being waited upon.
/// The `data` field is a user defined quantity passed in
/// via `sys_epoll_ctl`. Typically C programs could use this
/// to store a pointer to the data that needs to be processed
/// after an event.
struct WaitObject {
    target: Weak<FileObject>,
    events: FdEvents,
    data: u64,
    wait_canceler: Option<WaitCanceler>,
}

impl WaitObject {
    // TODO(fxbug.dev/64296) we should not report an error if the file was closed while it was
    // registered for epoll(). Either the file needs to be removed from our lists when it is closed,
    // we need to ignore/remove WaitObjects when the file is gone, or (more likely) both because of
    // race conditions removing the file object.
    fn target(&self) -> Result<FileHandle, Errno> {
        self.target.upgrade().ok_or_else(|| errno!(EBADF))
    }
}

/// EpollKey acts as an key to a map of WaitObject.
/// In reality it is a pointer to a FileHandle object.
type EpollKey = usize;

fn as_epoll_key(file: &FileHandle) -> EpollKey {
    Arc::as_ptr(file) as EpollKey
}

/// ReadyObject represents an event on a waited upon object.
#[derive(Clone, Debug)]
struct ReadyObject {
    key: EpollKey,
    observed: FdEvents,
}

/// EpollFileObject represents the FileObject used to
/// implement epoll_create1/epoll_ctl/epoll_pwait.
pub struct EpollFileObject {
    waiter: Waiter,
    /// Mutable state of this epoll object.
    state: Arc<RwLock<EpollState>>,
}

struct EpollState {
    /// Any file tracked by this epoll instance
    /// will exist as a key in `wait_objects`.
    wait_objects: HashMap<EpollKey, WaitObject>,
    /// trigger_list is a FIFO of events that have
    /// happened, but have not yet been processed.
    trigger_list: VecDeque<ReadyObject>,
    /// rearm_list is the list of event that need to
    /// be waited upon prior to actually waiting in
    /// EpollFileObject::wait. They cannot be re-armed
    /// before that, because, if the client process has
    /// not cleared the wait condition, they would just
    /// be immediately triggered.
    rearm_list: Vec<ReadyObject>,
    /// A list of waiters waiting for events from this
    /// epoll instance.
    waiters: WaitQueue,
}

impl EpollFileObject {
    /// Allocate a new, empty epoll object.
    pub fn new_file(current_task: &CurrentTask) -> FileHandle {
        Anon::new_file(
            current_task,
            Box::new(EpollFileObject {
                waiter: Waiter::new(),
                state: Arc::new(RwLock::new(EpollState {
                    wait_objects: HashMap::default(),
                    trigger_list: VecDeque::new(),
                    rearm_list: Vec::new(),
                    waiters: WaitQueue::default(),
                })),
            }),
            OpenFlags::RDWR,
        )
    }

    fn new_wait_handler(&self, key: EpollKey) -> EventHandler {
        let state = self.state.clone();
        Box::new(move |observed: FdEvents| {
            state.write().trigger_list.push_back(ReadyObject { key, observed })
        })
    }

    fn wait_on_file(
        &self,
        current_task: &CurrentTask,
        key: EpollKey,
        wait_object: &mut WaitObject,
    ) -> Result<(), Errno> {
        let target = wait_object.target()?;

        // First start the wait. If an event happens after this, we'll get it.
        self.wait_on_file_edge_triggered(current_task, key, wait_object)?;

        // Now check the events. If an event happened before this, we'll detect it here. There's
        // now no race window where an event would be missed.
        //
        // That said, if an event happens between the wait and the query_events, we'll get two
        // notifications. We handle this by deduping on the epoll_wait end.
        let events = target.query_events(current_task)?;
        if !(events & wait_object.events).is_empty() {
            self.waiter.wake_immediately(events, self.new_wait_handler(key));
            wait_object
                .wait_canceler
                .as_ref()
                .expect("canceler must have been set by `wait_on_file_edge_triggered`")
                .cancel();
        }
        Ok(())
    }

    fn wait_on_file_edge_triggered(
        &self,
        current_task: &CurrentTask,
        key: EpollKey,
        wait_object: &mut WaitObject,
    ) -> Result<(), Errno> {
        wait_object.wait_canceler = wait_object.target()?.wait_async(
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
    fn monitors(&self, epoll_file_handle: &FileHandle, depth_left: u32) -> Result<bool, Errno> {
        if depth_left == 0 {
            return Ok(true);
        }

        let state = self.state.read();
        for nested_object in state.wait_objects.values() {
            match nested_object.target()?.downcast_file::<EpollFileObject>() {
                None => continue,
                Some(target) => {
                    if target.monitors(epoll_file_handle, depth_left - 1)?
                        || Arc::ptr_eq(&nested_object.target()?, epoll_file_handle)
                    {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    /// Asynchronously wait on certain events happening on a FileHandle.
    pub fn add(
        &self,
        current_task: &CurrentTask,
        file: &FileHandle,
        epoll_file_handle: &FileHandle,
        mut epoll_event: epoll_event,
    ) -> Result<(), Errno> {
        epoll_event.events |= FdEvents::POLLHUP.bits();
        epoll_event.events |= FdEvents::POLLERR.bits();

        // Check if adding this file would cause a cycle at a max depth of 5.
        if let Some(epoll_to_add) = file.downcast_file::<EpollFileObject>() {
            if epoll_to_add.monitors(epoll_file_handle, MAX_NESTED_DEPTH)? {
                return error!(ELOOP);
            }
        }

        let mut state = self.state.write();
        let key = as_epoll_key(file);
        match state.wait_objects.entry(key) {
            Entry::Occupied(_) => error!(EEXIST),
            Entry::Vacant(entry) => {
                let wait_object = entry.insert(WaitObject {
                    target: Arc::downgrade(file),
                    events: FdEvents::from_bits_truncate(epoll_event.events),
                    data: epoll_event.data,
                    wait_canceler: None,
                });
                self.wait_on_file(current_task, key, wait_object)
            }
        }
    }

    /// Modify the events we are looking for on a Filehandle.
    pub fn modify(
        &self,
        current_task: &CurrentTask,
        file: &FileHandle,
        mut epoll_event: epoll_event,
    ) -> Result<(), Errno> {
        epoll_event.events |= FdEvents::POLLHUP.bits();
        epoll_event.events |= FdEvents::POLLERR.bits();

        let mut state = self.state.write();
        let key = as_epoll_key(file);
        state.rearm_list.retain(|x| x.key != key);
        match state.wait_objects.entry(key) {
            Entry::Occupied(mut entry) => {
                let wait_object = entry.get_mut();
                if let Some(wait_canceler) = wait_object.wait_canceler.take() {
                    wait_canceler.cancel();
                }
                wait_object.events = FdEvents::from_bits_truncate(epoll_event.events);
                self.wait_on_file(current_task, key, wait_object)
            }
            Entry::Vacant(_) => error!(ENOENT),
        }
    }

    /// Cancel an asynchronous wait on an object. Events triggered before
    /// calling this will still be delivered.
    pub fn delete(&self, file: &FileHandle) -> Result<(), Errno> {
        let mut state = self.state.write();
        let key = as_epoll_key(file);
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
    fn process_triggered_events(
        &self,
        current_task: &CurrentTask,
        pending_list: &mut Vec<ReadyObject>,
        max_events: usize,
    ) -> Result<(), Errno> {
        let mut state = self.state.write();
        while pending_list.len() < max_events && !state.trigger_list.is_empty() {
            if let Some(pending) = state.trigger_list.pop_front() {
                if let Some(wait) = state.wait_objects.get_mut(&pending.key) {
                    // The weak pointer to the FileObject target can be gone if the file was closed
                    // out from under us. If this happens it is not an error: ignore it and
                    // continue.
                    if let Some(target) = wait.target.upgrade() {
                        let observed = target.query_events(current_task)?;
                        let ready = ReadyObject { key: pending.key, observed };
                        if observed.intersects(wait.events) {
                            pending_list.push(ready);
                        } else {
                            // Another thread already handled this event, wait for another one.
                            // Files can be legitimately closed out from under us so bad file
                            // descriptors are not an error.
                            match self.wait_on_file(current_task, pending.key, wait) {
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
    fn wait_until_pending_event(
        &self,
        current_task: &CurrentTask,
        pending_list: &mut Vec<ReadyObject>,
        max_events: usize,
        input_wait_deadline: zx::Time,
    ) -> Result<(), Errno> {
        // Avoid nonzero deadlines if there are already extracted events (see EINTR handling below).
        debug_assert!(input_wait_deadline.into_nanos() == 0 || pending_list.is_empty());

        let mut wait_deadline = input_wait_deadline;

        loop {
            // The handlers in the waits cause items to be appended to trigger_list. See the closure
            // in `wait_on_file` to see how this happens.
            //
            // This wait may return EINTR for nonzero timeouts which is not an error. We must be
            // careful not to lose events if this happens.
            //
            // The first time through this loop we'll use the timeout passed into this function so
            // can get EINTR. But since we haven't done anything or accumulated any results yet it's
            // OK to immediately return and no information will be lost.
            match self.waiter.wait_until(current_task, wait_deadline) {
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

            self.process_triggered_events(current_task, pending_list, max_events)?;

            if pending_list.len() == max_events {
                break; // No input events or output list full, nothing more we can do.
            }

            if !pending_list.is_empty() {
                // We now know we have at least one event to return. We shouldn't return
                // immediately, in case there are more events available, but the next loop should
                // wait with a 0 timeout to prevent further blocking.
                wait_deadline = zx::Time::ZERO;
            }

            // Loop back to check if there are more items in the Waiter's queue. Every wait_until()
            // call will process a single event. In order to drain as many events as we can that
            // are synchronously available, keep trying until it reports empty.
        }

        Ok(())
    }

    /// Blocking wait on all waited upon events with a timeout.
    pub fn wait(
        &self,
        current_task: &CurrentTask,
        max_events: usize,
        mut deadline: zx::Time,
    ) -> Result<Vec<epoll_event>, Errno> {
        // First we start waiting again on wait objects that have
        // previously been triggered.
        {
            let mut state = self.state.write();
            let rearm_list = std::mem::take(&mut state.rearm_list);
            for to_wait in rearm_list.iter() {
                // TODO handle interrupts here
                let w = state.wait_objects.get_mut(&to_wait.key).unwrap();
                self.wait_on_file(current_task, to_wait.key, w)?;
            }
        }

        // Process any events that are already available in the triggered queue.
        // TODO(tbodt) fold this into the wait_until_pending_event loop
        let mut pending_list = vec![];
        self.process_triggered_events(current_task, &mut pending_list, max_events)?;
        if !pending_list.is_empty() {
            // TODO(tbodt) delete this block
            // If there are events synchronously available, don't actually wait for any more.
            // We still need to call wait_until_pending_event() (this time with a 0 deadline) to
            // process any events currently pending in the Waiter that haven't been added to our
            // triggered queue yet.
            deadline = zx::Time::ZERO;
        }

        // Note: wait_until_pending_event() can be interrupted with EINTR. We must be careful not to
        // lose state if that happens. The only state that can be lost are items already in the
        // pending list, and the code above sets the deadline to 0 in that case which will remove
        // the wait and avoid EINTR.
        self.wait_until_pending_event(current_task, &mut pending_list, max_events, deadline)?;

        // Process the pending list and add processed ReadyObject
        // entries to the rearm_list for the next wait.
        let mut result = vec![];
        let mut state = self.state.write();
        for pending_event in pending_list.iter().unique_by(|e| e.key) {
            // The wait could have been deleted by here,
            // so ignore the None case.
            if let Some(wait) = state.wait_objects.get_mut(&pending_event.key) {
                let reported_events = pending_event.observed.bits() & wait.events.bits();
                result.push(epoll_event::new(reported_events, wait.data));

                // Files marked with `EPOLLONESHOT` should only notify
                // once and need to be rearmed manually with epoll_ctl_mod().
                if wait.events.bits() & EPOLLONESHOT != 0 {
                    continue;
                }
                if wait.events.bits() & EPOLLET != 0 {
                    // The file can be closed while registered for epoll which is not an error.
                    // We do not expect other errors from waiting.
                    match self.wait_on_file_edge_triggered(current_task, pending_event.key, wait) {
                        Err(err) if err == EBADF => {} // File closed, ignore.
                        Err(err) => log_warn!("Unexpected wait result {:#?}", err),
                        _ => {}
                    }
                } else {
                    state.rearm_list.push(pending_event.clone());
                }
            }
        }

        // Notify waiters of unprocessed events.
        if !state.trigger_list.is_empty() {
            state.waiters.notify_fd_events(FdEvents::POLLIN);
        }

        Ok(result)
    }
}

impl FileOps for EpollFileObject {
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
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EINVAL)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.state.read().waiters.wait_async_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let mut events = FdEvents::empty();
        if self.state.read().trigger_list.is_empty() {
            events |= FdEvents::POLLIN;
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{
        buffers::{VecInputBuffer, VecOutputBuffer},
        fuchsia::create_fuchsia_pipe,
        pipe::new_pipe,
        socket::{SocketDomain, SocketType, UnixSocket},
        FdEvents,
    };
    use fuchsia_zircon::HandleBased;
    use std::sync::atomic::{AtomicU64, Ordering};
    use syncio::Zxio;

    use crate::testing::*;

    #[::fuchsia::test]
    async fn test_epoll_read_ready() {
        static WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
        const EVENT_DATA: u64 = 42;

        let (kernel, _init_task) = create_kernel_and_task();
        let current_task = create_task(&kernel, "main-task");
        let writer_task = create_task(&kernel, "writer-task");

        let (pipe_out, pipe_in) = new_pipe(&current_task).unwrap();

        let test_string = "hello starnix".to_string();
        let test_len = test_string.len();

        let epoll_file_handle = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();
        epoll_file
            .add(
                &current_task,
                &pipe_out,
                &epoll_file_handle,
                epoll_event::new(FdEvents::POLLIN.bits(), EVENT_DATA),
            )
            .unwrap();

        let test_string_copy = test_string.clone();
        let thread = std::thread::spawn(move || {
            let bytes_written = pipe_in
                .write(&writer_task, &mut VecInputBuffer::new(test_string_copy.as_bytes()))
                .unwrap();
            assert_eq!(bytes_written, test_len);
            WRITE_COUNT.fetch_add(bytes_written as u64, Ordering::Relaxed);
        });
        let events = epoll_file.wait(&current_task, 10, zx::Time::INFINITE).unwrap();
        let _ = thread.join();
        assert_eq!(1, events.len());
        let event = &events[0];
        assert!(FdEvents::from_bits_truncate(event.events).contains(FdEvents::POLLIN));
        let data = event.data;
        assert_eq!(EVENT_DATA, data);

        let mut buffer = VecOutputBuffer::new(test_len);
        let bytes_read = pipe_out.read(&current_task, &mut buffer).unwrap();
        assert_eq!(bytes_read as u64, WRITE_COUNT.load(Ordering::Relaxed));
        assert_eq!(bytes_read, test_len);
        assert_eq!(buffer.data(), test_string.as_bytes());
    }

    #[::fuchsia::test]
    async fn test_epoll_ready_then_wait() {
        const EVENT_DATA: u64 = 42;

        let (_kernel, current_task) = create_kernel_and_task();

        let (pipe_out, pipe_in) = new_pipe(&current_task).unwrap();

        let test_string = "hello starnix".to_string();
        let test_bytes = test_string.as_bytes();
        let test_len = test_bytes.len();

        assert_eq!(
            pipe_in.write(&current_task, &mut VecInputBuffer::new(test_bytes)).unwrap(),
            test_bytes.len()
        );

        let epoll_file_handle = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();
        epoll_file
            .add(
                &current_task,
                &pipe_out,
                &epoll_file_handle,
                epoll_event::new(FdEvents::POLLIN.bits(), EVENT_DATA),
            )
            .unwrap();

        let events = epoll_file.wait(&current_task, 10, zx::Time::INFINITE).unwrap();
        assert_eq!(1, events.len());
        let event = &events[0];
        assert!(FdEvents::from_bits_truncate(event.events).contains(FdEvents::POLLIN));
        let data = event.data;
        assert_eq!(EVENT_DATA, data);

        let mut buffer = VecOutputBuffer::new(test_len);
        let bytes_read = pipe_out.read(&current_task, &mut buffer).unwrap();
        assert_eq!(bytes_read, test_len);
        assert_eq!(buffer.data(), test_bytes);
    }

    #[::fuchsia::test]
    async fn test_epoll_ctl_cancel() {
        for do_cancel in [true, false] {
            let (_kernel, current_task) = create_kernel_and_task();
            let event = new_eventfd(&current_task, 0, EventFdType::Counter, true);
            let waiter = Waiter::new();

            let epoll_file_handle = EpollFileObject::new_file(&current_task);
            let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();
            const EVENT_DATA: u64 = 42;
            epoll_file
                .add(
                    &current_task,
                    &event,
                    &epoll_file_handle,
                    epoll_event::new(FdEvents::POLLIN.bits(), EVENT_DATA),
                )
                .unwrap();

            if do_cancel {
                epoll_file.delete(&event).unwrap();
            }

            let callback_count = Arc::new(AtomicU64::new(0));
            let callback_count_clone = callback_count.clone();
            let handler = move |_observed: FdEvents| {
                callback_count_clone.fetch_add(1, Ordering::Relaxed);
            };
            let wait_canceler = event
                .wait_async(&current_task, &waiter, FdEvents::POLLIN, Box::new(handler))
                .expect("wait_async");
            if do_cancel {
                wait_canceler.cancel();
            }

            let add_val = 1u64;
            assert_eq!(
                event
                    .write(&current_task, &mut VecInputBuffer::new(&add_val.to_ne_bytes()))
                    .unwrap(),
                std::mem::size_of::<u64>()
            );

            let events = epoll_file.wait(&current_task, 10, zx::Time::ZERO).unwrap();

            if do_cancel {
                assert_eq!(0, events.len());
            } else {
                assert_eq!(1, events.len());
                let event = &events[0];
                assert!(FdEvents::from_bits_truncate(event.events).contains(FdEvents::POLLIN));
                let data = event.data;
                assert_eq!(EVENT_DATA, data);
            }
        }
    }

    #[::fuchsia::test]
    async fn test_multiple_events() {
        let (_kernel, current_task) = create_kernel_and_task();
        let (client1, server1) = zx::Socket::create_stream();
        let (client2, server2) = zx::Socket::create_stream();
        let pipe1 = create_fuchsia_pipe(&current_task, client1, OpenFlags::RDWR)
            .expect("create_fuchsia_pipe");
        let pipe2 = create_fuchsia_pipe(&current_task, client2, OpenFlags::RDWR)
            .expect("create_fuchsia_pipe");
        let server1_zxio = Zxio::create(server1.into_handle()).expect("Zxio::create");
        let server2_zxio = Zxio::create(server2.into_handle()).expect("Zxio::create");

        let poll = || {
            let epoll_object = EpollFileObject::new_file(&current_task);
            let epoll_file = epoll_object.downcast_file::<EpollFileObject>().unwrap();
            epoll_file
                .add(
                    &current_task,
                    &pipe1,
                    &epoll_object,
                    epoll_event::new(FdEvents::POLLIN.bits(), 1),
                )
                .expect("epoll_file.add");
            epoll_file
                .add(
                    &current_task,
                    &pipe2,
                    &epoll_object,
                    epoll_event::new(FdEvents::POLLIN.bits(), 2),
                )
                .expect("epoll_file.add");
            epoll_file.wait(&current_task, 2, zx::Time::ZERO).expect("wait")
        };

        let fds = poll();
        assert!(fds.is_empty());

        assert_eq!(server1_zxio.write(&[0]).expect("write"), 1);

        let fds = poll();
        assert_eq!(fds.len(), 1);
        assert_eq!(FdEvents::from_bits_truncate(fds[0].events), FdEvents::POLLIN);
        let data = fds[0].data;
        assert_eq!(data, 1);
        assert_eq!(pipe1.read(&current_task, &mut VecOutputBuffer::new(64)).expect("read"), 1);

        let fds = poll();
        assert!(fds.is_empty());

        assert_eq!(server2_zxio.write(&[0]).expect("write"), 1);

        let fds = poll();
        assert_eq!(fds.len(), 1);
        assert_eq!(FdEvents::from_bits_truncate(fds[0].events), FdEvents::POLLIN);
        let data = fds[0].data;
        assert_eq!(data, 2);
        assert_eq!(pipe2.read(&current_task, &mut VecOutputBuffer::new(64)).expect("read"), 1);

        let fds = poll();
        assert!(fds.is_empty());
    }

    #[::fuchsia::test]
    async fn test_cancel_after_notify() {
        let (_kernel, current_task) = create_kernel_and_task();
        let event = new_eventfd(&current_task, 0, EventFdType::Counter, true);
        let epoll_file_handle = EpollFileObject::new_file(&current_task);
        let epoll_file = epoll_file_handle.downcast_file::<EpollFileObject>().unwrap();

        // Add a thing
        const EVENT_DATA: u64 = 42;
        epoll_file
            .add(
                &current_task,
                &event,
                &epoll_file_handle,
                epoll_event::new(FdEvents::POLLIN.bits(), EVENT_DATA),
            )
            .unwrap();

        // Make the thing send a notification, wait for it
        let add_val = 1u64;
        assert_eq!(
            event.write(&current_task, &mut VecInputBuffer::new(&add_val.to_ne_bytes())).unwrap(),
            std::mem::size_of::<u64>()
        );

        assert_eq!(epoll_file.wait(&current_task, 10, zx::Time::ZERO).unwrap().len(), 1);

        // Remove the thing
        epoll_file.delete(&event).unwrap();

        // Wait for new notifications
        assert_eq!(epoll_file.wait(&current_task, 10, zx::Time::ZERO).unwrap().len(), 0);
        // That shouldn't crash
    }

    #[::fuchsia::test]
    async fn test_add_then_modify() {
        let (_kernel, current_task) = create_kernel_and_task();
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
                &current_task,
                &socket1,
                &epoll_file_handle,
                epoll_event::new(FdEvents::POLLIN.bits(), EVENT_DATA),
            )
            .unwrap();
        assert_eq!(epoll_file.wait(&current_task, 10, zx::Time::ZERO).unwrap().len(), 0);

        let read_write_event = FdEvents::POLLIN | FdEvents::POLLOUT;
        epoll_file
            .modify(&current_task, &socket1, epoll_event::new(read_write_event.bits(), EVENT_DATA))
            .unwrap();
        let triggered_events = epoll_file.wait(&current_task, 10, zx::Time::ZERO).unwrap();
        assert_eq!(1, triggered_events.len());
        let event = &triggered_events[0];
        let events = event.events;
        assert_eq!(events, FdEvents::POLLOUT.bits());
        let data = event.data;
        assert_eq!(EVENT_DATA, data);
    }
}
