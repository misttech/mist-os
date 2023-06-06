// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use std::{
    collections::HashMap,
    ops::DerefMut,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
};

use crate::{
    fs::FdEvents,
    lock::Mutex,
    logging::*,
    task::*,
    types::{Errno, *},
};

pub type SignalHandler = Box<dyn FnOnce(zx::Signals) + Send + Sync>;
pub type EventHandler = Box<dyn FnOnce(FdEvents) + Send + Sync>;

pub enum WaitCallback {
    SignalHandler(SignalHandler),
    EventHandler(EventHandler),
}

/// Return values for wait_async methods. Calling `cancel` will cancel any running wait.
pub struct WaitCanceler {
    canceler: Box<dyn Fn() -> bool + Send + Sync>,
}

impl WaitCanceler {
    pub fn new<F>(canceler: F) -> Self
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        Self { canceler: Box::new(canceler) }
    }

    /// Cancel the pending wait. Returns `true` if a wait has been cancelled. It is valid to call
    /// this function multiple times.
    pub fn cancel(&self) -> bool {
        (self.canceler)()
    }
}

/// Return values for wait_async methods that monitor the state of a handle. Calling `cancel` will
/// cancel any running wait.
pub struct HandleWaitCanceler {
    canceler: Box<dyn Fn(zx::HandleRef<'_>) -> bool + Send + Sync>,
}

impl HandleWaitCanceler {
    pub fn new<F>(canceler: F) -> Self
    where
        F: Fn(zx::HandleRef<'_>) -> bool + Send + Sync + 'static,
    {
        Self { canceler: Box::new(canceler) }
    }

    /// Cancel the pending wait. Returns `true` if a wait has been cancelled. It is valid to call
    /// this function multiple times.
    pub fn cancel(&self, handle: zx::HandleRef<'_>) -> bool {
        (self.canceler)(handle)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct WaitKey {
    raw: u64,
}

impl WaitKey {
    /// an empty key means no associated handler
    fn empty() -> WaitKey {
        WaitKey { raw: 0 }
    }
}

/// The different type of event that can be waited on / triggered.
#[derive(Clone, Copy, Debug)]
enum WaitEvents {
    /// All event: a wait on `All` will be woken up by all event, and a trigger on `All` will wake
    /// every waiter.
    All,
    /// Wait on the set of FdEvents.
    Fd(FdEvents),
    /// Wait on the given mask.
    Mask(u64),
    /// Wait for the specified value.
    Value(u64),
}

impl WaitEvents {
    /// Build a WaitEvents from the given `ordinal` and `value`.
    fn from_data(ordinal: u8, value: u64) -> Self {
        match ordinal {
            0 => Self::All,
            1 => Self::Fd(FdEvents::from_u64(value)),
            2 => Self::Mask(value),
            3 => Self::Value(value),
            _ => panic!("Unknown ordinal"),
        }
    }

    /// Returns whether a wait on `self` should be woken up by `other`.
    fn intercept(self: &WaitEvents, other: &WaitEvents) -> bool {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => true,
            (Self::Fd(m1), Self::Fd(m2)) => m1.bits() & m2.bits() != 0,
            (Self::Mask(m1), Self::Mask(m2)) => m1 & m2 != 0,
            (Self::Value(v1), Self::Value(v2)) => v1 == v2,
            _ => false,
        }
    }

    /// Returns the data to serialize `self`.
    fn data(&self) -> (u8, u64) {
        match self {
            Self::All => (0, 0),
            Self::Fd(events) => (1, events.bits() as u64),
            Self::Mask(value) => (2, *value),
            Self::Value(value) => (3, *value),
        }
    }
}

impl From<WaitEvents> for zx::UserPacket {
    fn from(events: WaitEvents) -> Self {
        let (ordinal, value) = events.data();
        let mut packet_data = [0u8; 32];
        packet_data[0] = ordinal;
        packet_data[8..16].copy_from_slice(&value.to_ne_bytes());
        zx::UserPacket::from_u8_array(packet_data)
    }
}

impl From<zx::UserPacket> for WaitEvents {
    fn from(packet: zx::UserPacket) -> Self {
        let mut value_bytes = [0u8; 8];
        value_bytes[..8].copy_from_slice(&packet.as_u8_array()[8..16]);
        Self::from_data(packet.as_u8_array()[0], u64::from_ne_bytes(value_bytes))
    }
}

impl WaitCallback {
    pub fn none() -> EventHandler {
        Box::new(|_| {})
    }
}

/// A type that can put a thread to sleep waiting for a condition.
pub struct Waiter(Arc<WaiterImpl>);

/// Implementation of Waiter. We put the Waiter data in an Arc so that WaitQueue can tell when the
/// Waiter has been destroyed by keeping a Weak reference. But this is an implementation detail and
/// a Waiter should have a single owner. So the Arc is hidden inside Waiter.
struct WaiterImpl {
    /// The underlying Zircon port that the thread sleeps in.
    port: zx::Port,
    key_map: Mutex<HashMap<WaitKey, WaitCallback>>, // the key 0 is reserved for 'no handler'
    next_key: AtomicU64,
    ignore_signals: bool,
}

impl Waiter {
    /// Internal constructor.
    fn new_internal(ignore_signals: bool) -> Self {
        Self(Arc::new(WaiterImpl {
            port: zx::Port::create(),
            key_map: Mutex::new(HashMap::new()),
            next_key: AtomicU64::new(1),
            ignore_signals,
        }))
    }

    /// Create a new waiter.
    pub fn new() -> Self {
        Self::new_internal(false)
    }

    /// Create a new waiter that doesn't wake up when a signal is received.
    pub fn new_ignoring_signals() -> Self {
        Self::new_internal(true)
    }

    /// Create a weak reference to this waiter.
    pub fn weak(&self) -> WaiterRef {
        WaiterRef(Arc::downgrade(&self.0))
    }

    /// Wait until the waiter is woken up.
    ///
    /// If the wait is interrupted (see [`Waiter::interrupt`]), this function returns EINTR.
    pub fn wait(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.wait_until(current_task, zx::Time::INFINITE)
    }

    /// Wait until the given deadline has passed or the waiter is woken up.
    ///
    /// If the wait deadline is nonzero and is interrupted (see [`Waiter::interrupt`]), this
    /// function returns EINTR. Callers must take special care not to lose any accumulated data or
    /// local state when EINTR is received as this is a normal and recoverable situation.
    ///
    /// Using a 0 deadline (no waiting, useful for draining pending events) will not wait and is
    /// guaranteed not to issue EINTR.
    ///
    /// It the timeout elapses with no events, this function returns ETIMEDOUT.
    ///
    /// Processes at most one event. If the caller is interested in draining the events, it should
    /// repeatedly call this function with a 0 deadline until it reports ETIMEDOUT. (This case is
    /// why a 0 deadline must not return EINTR, as previous calls to wait_until() may have
    /// accumulated state that would be lost when returning EINTR to userspace.)
    pub fn wait_until(&self, current_task: &CurrentTask, deadline: zx::Time) -> Result<(), Errno> {
        let is_waiting = deadline.into_nanos() > 0;

        if is_waiting {
            let mut state = current_task.write();
            assert!(!state.signals.waiter.is_valid());
            if state.signals.is_any_pending() {
                return error!(EINTR);
            }
            state.signals.waiter = self.weak();
        }

        scopeguard::defer! {
            if is_waiting {
                let mut state = current_task.write();
                assert!(
                    state.signals.waiter.access(|waiter| Arc::ptr_eq(&waiter.unwrap().0, &self.0)),
                    "SignalState waiter changed while waiting!"
                );
                state.signals.waiter = WaiterRef::empty();
            }
        };

        // We are susceptible to spurious wakeups because interrupt() posts a message to the port
        // queue. In addition to more subtle races, there could already be valid messages in the
        // port queue that will immediately wake us up, leaving the interrupt message in the queue
        // for subsequent waits (which by then may not have any signals pending) to read.
        //
        // It's impossible to non-racily guarantee that a signal is pending so there might always
        // be an EINTR result here with no signal. But any signal we get when !is_waiting we know is
        // leftover from before: the top of this function only sets ourself as the
        // current_task.signals.waiter when there's a nonzero timeout, and that waiter reference is
        // what is used to signal the interrupt().
        loop {
            let wait_result = self.wait_internal(deadline);
            if let Err(errno) = &wait_result {
                if errno.code == EINTR && !is_waiting {
                    continue; // Spurious wakeup.
                }
            }
            return wait_result;
        }
    }

    /// Waits until the given deadline has passed or the waiter is woken up. See wait_until().
    fn wait_internal(&self, deadline: zx::Time) -> Result<(), Errno> {
        match self.0.port.wait(deadline) {
            Ok(packet) => match packet.status() {
                zx::sys::ZX_OK => {
                    let contents = packet.contents();
                    let key = WaitKey { raw: packet.key() };
                    match contents {
                        zx::PacketContents::SignalOne(sigpkt) => {
                            let handler = self.0.key_map.lock().remove(&key);
                            if let Some(callback) = handler {
                                match callback {
                                    WaitCallback::SignalHandler(handler) => {
                                        handler(sigpkt.observed())
                                    }
                                    WaitCallback::EventHandler(_) => {
                                        panic!("wrong type of handler called")
                                    }
                                }
                            }
                        }
                        zx::PacketContents::User(usrpkt) => {
                            let events: WaitEvents = usrpkt.into();
                            let handler = self.0.key_map.lock().remove(&key);
                            if let Some(callback) = handler {
                                match callback {
                                    WaitCallback::EventHandler(handler) => {
                                        let fd_events = match events {
                                            // If the event is All, signal on all possible fd
                                            // events.
                                            WaitEvents::All => FdEvents::all(),
                                            WaitEvents::Fd(events) => events,
                                            _ => panic!("wrong type of handler called: {events:?}"),
                                        };
                                        handler(fd_events)
                                    }
                                    WaitCallback::SignalHandler(_) => {
                                        panic!("wrong type of handler called")
                                    }
                                }
                            }
                        }
                        _ => return error!(EBADMSG),
                    }
                    Ok(())
                }
                zx::sys::ZX_ERR_CANCELED => error!(EINTR),
                _ => {
                    debug_assert!(false, "Unexpected status in port wait {}", packet.status());
                    error!(EBADMSG)
                }
            },
            Err(zx::Status::TIMED_OUT) => error!(ETIMEDOUT),
            Err(errno) => Err(impossible_error(errno)),
        }
    }

    fn next_key(&self) -> WaitKey {
        let key = self.0.next_key.fetch_add(1, Ordering::Relaxed);
        // TODO - find a better reaction to wraparound
        assert!(key != 0, "bad key from u64 wraparound");
        WaitKey { raw: key }
    }

    fn register_callback(&self, callback: WaitCallback) -> WaitKey {
        let key = self.next_key();
        assert!(
            self.0.key_map.lock().insert(key, callback).is_none(),
            "unexpected callback already present for key {key:?}"
        );
        key
    }

    pub fn wake_immediately(&self, events: FdEvents, handler: EventHandler) {
        let callback = WaitCallback::EventHandler(handler);
        let key = self.register_callback(callback);
        self.queue_events(&key, WaitEvents::Fd(events));
    }

    /// Establish an asynchronous wait for the signals on the given Zircon handle (not to be
    /// confused with POSIX signals), optionally running a FnOnce.
    ///
    /// Returns a `HandleWaitCanceler` that can be used to cancel the wait.
    pub fn wake_on_zircon_signals(
        &self,
        handle: &dyn zx::AsHandleRef,
        zx_signals: zx::Signals,
        handler: SignalHandler,
    ) -> Result<HandleWaitCanceler, zx::Status> {
        let callback = WaitCallback::SignalHandler(handler);
        let key = self.register_callback(callback);
        handle.wait_async_handle(
            &self.0.port,
            key.raw,
            zx_signals,
            zx::WaitAsyncOpts::EDGE_TRIGGERED,
        )?;
        let waiter_impl = Arc::downgrade(&self.0);
        Ok(HandleWaitCanceler::new(move |handle_ref| {
            if let Some(waiter_impl) = waiter_impl.upgrade() {
                waiter_impl.port.cancel(&handle_ref, key.raw).is_ok()
            } else {
                false
            }
        }))
    }

    /// Return a WaitCanceler representing a wait that will never complete. Useful for stub
    /// implementations that should block forever even though a real implementation would wake up
    /// eventually.
    pub fn fake_wait(&self) -> WaitCanceler {
        let has_run = Mutex::new(false);
        WaitCanceler::new(move || {
            let mut has_run = has_run.lock();
            if !*has_run {
                *has_run = true;
                true
            } else {
                false
            }
        })
    }

    fn wake_on_events(&self, handler: EventHandler) -> WaitKey {
        let callback = WaitCallback::EventHandler(handler);
        self.register_callback(callback)
    }

    fn queue_events(&self, key: &WaitKey, event: WaitEvents) {
        self.queue_user_packet_data(key, zx::sys::ZX_OK, event)
    }

    /// Interrupt the waiter to deliver a signal. The wait operation will return EINTR, and a
    /// typical caller should then unwind to the syscall dispatch loop to let the signal be
    /// processed. See wait_until() for more details.
    ///
    /// Ignored if the waiter was created with new_ignoring_signals().
    pub fn interrupt(&self) {
        if self.0.ignore_signals {
            return;
        }
        self.queue_user_packet_data(&WaitKey::empty(), zx::sys::ZX_ERR_CANCELED, WaitEvents::All);
    }

    /// Queue a packet to the underlying Zircon port, which will cause the
    /// waiter to wake up.
    fn queue_user_packet_data(&self, key: &WaitKey, status: i32, events: WaitEvents) {
        let packet = zx::Packet::from_user_packet(key.raw, status, events.into());
        self.0.port.queue(&packet).map_err(impossible_error).unwrap();
    }
}

impl std::fmt::Debug for Waiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waiter").field("port", &self.0.port).finish_non_exhaustive()
    }
}

impl Default for Waiter {
    fn default() -> Self {
        Self::new()
    }
}

/// A weak reference to a Waiter. Intended for holding in wait queues or stashing elsewhere for
/// calling queue_events later.
#[derive(Default)]
pub struct WaiterRef(Weak<WaiterImpl>);

impl WaiterRef {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_valid(&self) -> bool {
        self.0.strong_count() != 0
    }

    /// Call the closure with a reference to the waiter if this weak ref is valid, or None if it
    /// isn't.
    pub fn access<R>(&self, f: impl FnOnce(Option<&Waiter>) -> R) -> R {
        f(self.0.upgrade().map(Waiter).as_ref())
    }
}

impl std::fmt::Debug for WaiterRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.access(|waiter| f.debug_tuple("WaiterRef").field(&waiter).finish())
    }
}

/// A list of waiters waiting for some event.
///
/// For events that are generated inside Starnix, we walk the wait queue
/// on the thread that triggered the event to notify the waiters that the event
/// has occurred. The waiters will then wake up on their own thread to handle
/// the event.
#[derive(Default, Debug)]
pub struct WaitQueue {
    /// The list of waiters.
    waiters: Arc<Mutex<Vec<WaitEntry>>>,
}

/// An entry in a WaitQueue.
#[derive(Debug)]
struct WaitEntry {
    /// The waiter that is waking for the FdEvent.
    waiter: WaiterRef,

    /// The events that the waiter is waiting for.
    filter: WaitEvents,

    /// Whether the waiter wishes to remain in the WaitQueue after one of
    /// the events that the waiter is waiting for occurs.
    persistent: bool,

    /// key for cancelling and queueing events
    key: WaitKey,
}

impl WaitQueue {
    /// Establish a wait for the given entry.
    ///
    /// The waiter will be notified when an event matching the entry occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    fn wait_async_on_entry(&self, waiter: &Waiter, entry: WaitEntry) -> WaitCanceler {
        let key = entry.key;
        self.waiters.lock().push(entry);
        let waiter = waiter.weak();
        let waiters = Arc::downgrade(&self.waiters);
        WaitCanceler::new(move || {
            if let Some(waiters) = waiters.upgrade() {
                let mut cancelled = false;
                // TODO(steveaustin) Maybe make waiters a map to avoid linear search
                waiters.lock().retain(|entry| {
                    if entry.waiter.0.as_ptr() == waiter.0.as_ptr() && entry.key == key {
                        cancelled = true;
                        false
                    } else {
                        true
                    }
                });
                cancelled
            } else {
                false
            }
        })
    }

    /// Establish a wait for the given event mask.
    ///
    /// The waiter will be notified when an event matching the events mask
    /// occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    pub fn wait_async_mask(&self, waiter: &Waiter, mask: u64) -> WaitCanceler {
        let key = waiter.next_key();
        self.wait_async_on_entry(
            waiter,
            WaitEntry {
                waiter: waiter.weak(),
                filter: WaitEvents::Mask(mask),
                persistent: false,
                key,
            },
        )
    }

    /// Establish a wait for the given value event.
    ///
    /// The waiter will be notified when an event with the same value occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    pub fn wait_async_value(&self, waiter: &Waiter, value: u64) -> WaitCanceler {
        let key = waiter.next_key();
        self.wait_async_on_entry(
            waiter,
            WaitEntry {
                waiter: waiter.weak(),
                filter: WaitEvents::Value(value),
                persistent: false,
                key,
            },
        )
    }

    /// Establish a wait for any event.
    ///
    /// The waiter will be notified when any event occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    pub fn wait_async(&self, waiter: &Waiter) -> WaitCanceler {
        let key = waiter.next_key();
        self.wait_async_on_entry(
            waiter,
            WaitEntry { waiter: waiter.weak(), filter: WaitEvents::All, persistent: false, key },
        )
    }

    /// Establish a wait for the given events.
    ///
    /// The waiter will be notified when an event matching the `events` occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    pub fn wait_async_events(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        let key = waiter.wake_on_events(handler);
        self.wait_async_on_entry(
            waiter,
            WaitEntry {
                waiter: waiter.weak(),
                filter: WaitEvents::Fd(events),
                persistent: false,
                key,
            },
        )
    }

    fn notify_events_count(&self, events: WaitEvents, mut limit: usize) -> usize {
        let mut woken = 0;
        self.waiters.lock().retain(|entry| {
            entry.waiter.access(|waiter| {
                // Drop entries whose waiter no longer exists.
                let waiter = if let Some(waiter) = waiter {
                    waiter
                } else {
                    return false;
                };

                if limit > 0 && entry.filter.intercept(&events) {
                    waiter.queue_events(&entry.key, events);
                    limit -= 1;
                    woken += 1;
                    return entry.persistent;
                }

                true
            })
        });
        woken
    }

    /// Notify any waiters that the given events have occurred.
    ///
    /// Walks the wait queue and wakes each waiter that is waiting on an
    /// event that matches the given mask. Persistent waiters remain in the
    /// list. Non-persistent waiters are removed.
    ///
    /// The waiters will wake up on their own threads to handle these events.
    /// They are not called synchronously by this function.
    ///
    /// Returns the number of waiters woken.
    pub fn notify_mask_count(&self, mask: u64, limit: usize) -> usize {
        self.notify_events_count(WaitEvents::Mask(mask), limit)
    }

    pub fn notify_mask(&self, mask: u64) {
        self.notify_mask_count(mask, usize::MAX);
    }

    pub fn notify_fd_events(&self, events: FdEvents) {
        self.notify_events_count(WaitEvents::Fd(events), usize::MAX);
    }

    pub fn notify_value_event(&self, value: u64) {
        self.notify_events_count(WaitEvents::Value(value), usize::MAX);
    }

    pub fn notify_count(&self, limit: usize) {
        self.notify_events_count(WaitEvents::All, limit);
    }

    pub fn notify_all(&self) {
        self.notify_count(usize::MAX);
    }

    pub fn transfer(&self, other: &WaitQueue) {
        let mut other_entries = std::mem::take(other.waiters.lock().deref_mut());
        self.waiters.lock().append(&mut other_entries);
    }

    /// Returns whether there is no active waiters waiting on this `WaitQueue`.
    pub fn is_empty(&self) -> bool {
        let mut waiters = self.waiters.lock();
        waiters.retain(|entry| entry.waiter.is_valid());
        waiters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fs::{
            buffers::{VecInputBuffer, VecOutputBuffer},
            fuchsia::*,
            new_eventfd, EventFdType, FdEvents,
        },
        testing::*,
    };
    use std::sync::atomic::AtomicU64;

    static INIT_VAL: u64 = 0;
    static FINAL_VAL: u64 = 42;

    #[::fuchsia::test]
    async fn test_async_wait_exec() {
        static COUNTER: AtomicU64 = AtomicU64::new(INIT_VAL);
        static WRITE_COUNT: AtomicU64 = AtomicU64::new(0);

        let (_kernel, current_task) = create_kernel_and_task();
        let (local_socket, remote_socket) = zx::Socket::create_stream();
        let pipe = create_fuchsia_pipe(&current_task, remote_socket, OpenFlags::RDWR).unwrap();

        const MEM_SIZE: usize = 1024;
        let mut output_buffer = VecOutputBuffer::new(MEM_SIZE);

        let test_string = "hello startnix".to_string();
        let report_packet: EventHandler = Box::new(|observed: FdEvents| {
            assert!(observed.contains(FdEvents::POLLIN));
            COUNTER.store(FINAL_VAL, Ordering::Relaxed);
        });
        let waiter = Waiter::new();
        pipe.wait_async(&current_task, &waiter, FdEvents::POLLIN, report_packet)
            .expect("wait_async");
        let test_string_clone = test_string.clone();

        let thread = std::thread::spawn(move || {
            let test_data = test_string_clone.as_bytes();
            let no_written = local_socket.write(test_data).unwrap();
            assert_eq!(0, WRITE_COUNT.fetch_add(no_written as u64, Ordering::Relaxed));
            assert_eq!(no_written, test_data.len());
        });

        // this code would block on failure
        assert_eq!(INIT_VAL, COUNTER.load(Ordering::Relaxed));
        waiter.wait(&current_task).unwrap();
        let _ = thread.join();
        assert_eq!(FINAL_VAL, COUNTER.load(Ordering::Relaxed));

        let read_size = pipe.read(&current_task, &mut output_buffer).unwrap();

        let no_written = WRITE_COUNT.load(Ordering::Relaxed);
        assert_eq!(no_written, read_size as u64);

        assert_eq!(output_buffer.data(), test_string.as_bytes());
    }

    #[::fuchsia::test]
    async fn test_async_wait_cancel() {
        for do_cancel in [true, false] {
            let (_kernel, current_task) = create_kernel_and_task();
            let event = new_eventfd(&current_task, 0, EventFdType::Counter, true);
            let waiter = Waiter::new();
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

            let wait_result = waiter.wait_until(&current_task, zx::Time::ZERO);
            let final_count = callback_count.load(Ordering::Relaxed);
            if do_cancel {
                assert_eq!(wait_result, error!(ETIMEDOUT));
                assert_eq!(0, final_count);
            } else {
                assert_eq!(wait_result, Ok(()));
                assert_eq!(1, final_count);
            }
        }
    }

    #[::fuchsia::test]
    async fn single_waiter_multiple_waits_cancel_one_waiter_still_notified() {
        let (_kernel, current_task) = create_kernel_and_task();
        let wait_queue = WaitQueue::default();
        let waiter = Waiter::new();
        let wk1 = wait_queue.wait_async(&waiter);
        let _wk2 = wait_queue.wait_async(&waiter);
        assert!(wk1.cancel());
        wait_queue.notify_all();
        assert!(waiter.wait_until(&current_task, zx::Time::ZERO).is_ok());
    }

    #[::fuchsia::test]
    async fn multiple_waiters_cancel_one_other_still_notified() {
        let (_kernel, current_task) = create_kernel_and_task();
        let wait_queue = WaitQueue::default();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let wk1 = wait_queue.wait_async(&waiter1);
        let _wk2 = wait_queue.wait_async(&waiter2);
        assert!(wk1.cancel());
        wait_queue.notify_all();
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_ok());
    }

    #[::fuchsia::test]
    async fn test_wait_queue() {
        let (_kernel, current_task) = create_kernel_and_task();
        let queue = WaitQueue::default();

        let waiter0 = Waiter::new();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        queue.wait_async(&waiter0);
        queue.wait_async(&waiter1);
        queue.wait_async(&waiter2);

        queue.notify_count(2);
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_ok());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_ok());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_err());

        queue.notify_all();
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_ok());

        queue.notify_count(3);
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_err());
    }

    #[::fuchsia::test]
    async fn test_wait_queue_mask() {
        let (_kernel, current_task) = create_kernel_and_task();
        let queue = WaitQueue::default();

        let waiter0 = Waiter::new();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        queue.wait_async_mask(&waiter0, 0x13);
        queue.wait_async_mask(&waiter1, 0x11);
        queue.wait_async_mask(&waiter2, 0x12);

        queue.notify_mask_count(0x2, 2);
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_ok());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_ok());

        queue.notify_mask_count(0x1, usize::MAX);
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_ok());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_err());
    }
}
