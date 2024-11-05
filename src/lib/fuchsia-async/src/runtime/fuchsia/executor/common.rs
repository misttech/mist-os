// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::super::timer::Timers;
use super::packets::{PacketReceiver, PacketReceiverMap, ReceiverRegistration};
use super::scope::ScopeRef;
use super::time::{BootInstant, MonotonicInstant};
use crate::atomic_future::{AtomicFuture, AttemptPollResult};
use crossbeam::queue::SegQueue;
use fuchsia_sync::Mutex;
use zx::BootDuration;

use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, RawWaker, RawWakerVTable, Waker};
use std::{fmt, u64, usize};

pub(crate) const TASK_READY_WAKEUP_ID: u64 = u64::MAX - 1;

/// The id of the main task, which is a virtual task that lives from construction
/// to destruction of the executor. The main task may correspond to multiple
/// main futures, in cases where the executor runs multiple times during its lifetime.
pub(crate) const MAIN_TASK_ID: usize = 0;

thread_local!(
    static EXECUTOR: RefCell<Option<ScopeRef>> = RefCell::new(None)
);

pub enum ExecutorTime {
    RealTime,
    /// Fake readings used in tests.
    FakeTime {
        // The fake monotonic clock reading.
        mono_reading_ns: AtomicI64,
        // An offset to add to mono_reading_ns to get the reading of the boot
        // clock, disregarding the difference in timelines.
        //
        // We disregard the fact that the reading and offset can not be
        // read atomically, this is usually not relevant in tests.
        mono_to_boot_offset_ns: AtomicI64,
    },
}

enum PollReadyTasksResult {
    NoneReady,
    MoreReady,
    MainTaskCompleted,
}

// -- Helpers for threads_state below --

fn threads_sleeping(state: u16) -> u8 {
    state as u8
}

fn threads_notified(state: u16) -> u8 {
    (state >> 8) as u8
}

fn make_threads_state(sleeping: u8, notified: u8) -> u16 {
    sleeping as u16 | ((notified as u16) << 8)
}

#[cfg(test)]
static ACTIVE_EXECUTORS: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Executor {
    pub(super) port: zx::Port,
    monotonic_timers: Arc<Timers<MonotonicInstant>>,
    boot_timers: Arc<Timers<BootInstant>>,
    pub(super) done: AtomicBool,
    is_local: bool,
    receivers: Mutex<PacketReceiverMap<Arc<dyn PacketReceiver>>>,
    task_count: AtomicUsize,
    pub(super) ready_tasks: SegQueue<Arc<Task>>,
    time: ExecutorTime,
    // The low byte is the number of threads currently sleeping. The high byte is the number of
    // of wake-up notifications pending.
    pub(super) threads_state: AtomicU16,
    pub(super) num_threads: u8,
    pub(super) polled: AtomicU64,
    // Data that belongs to the user that can be accessed via EHandle::local(). See
    // `TestExecutor::poll_until_stalled`.
    pub(super) owner_data: Mutex<Option<Box<dyn Any + Send>>>,
}

impl Executor {
    pub fn new(time: ExecutorTime, is_local: bool, num_threads: u8) -> Self {
        #[cfg(test)]
        ACTIVE_EXECUTORS.fetch_add(1, Ordering::Relaxed);

        let mut receivers: PacketReceiverMap<Arc<dyn PacketReceiver>> = PacketReceiverMap::new();

        // Is this a fake-time executor?
        let is_fake = matches!(
            time,
            ExecutorTime::FakeTime { mono_reading_ns: _, mono_to_boot_offset_ns: _ }
        );
        let monotonic_timers = receivers.insert(|key| {
            let timers = Arc::new(Timers::<MonotonicInstant>::new(key, is_fake));
            (timers.clone(), timers)
        });
        let boot_timers = receivers.insert(|key| {
            let timers = Arc::new(Timers::<BootInstant>::new(key, is_fake));
            (timers.clone(), timers)
        });

        Executor {
            port: zx::Port::create(),
            monotonic_timers,
            boot_timers,
            done: AtomicBool::new(false),
            is_local,
            receivers: Mutex::new(receivers),
            task_count: AtomicUsize::new(MAIN_TASK_ID + 1),
            ready_tasks: SegQueue::new(),
            time,
            threads_state: AtomicU16::new(0),
            num_threads,
            polled: AtomicU64::new(0),
            owner_data: Mutex::new(None),
        }
    }

    pub fn set_local(root_scope: ScopeRef) {
        EXECUTOR.with(|e| {
            let mut e = e.borrow_mut();
            assert!(e.is_none(), "Cannot create multiple Fuchsia Executors");
            *e = Some(root_scope);
        });
    }

    fn poll_ready_tasks(&self) -> PollReadyTasksResult {
        loop {
            for _ in 0..16 {
                let Some(task) = self.ready_tasks.pop() else {
                    return PollReadyTasksResult::NoneReady;
                };
                let complete = self.try_poll(&task);
                if complete && task.id == MAIN_TASK_ID {
                    return PollReadyTasksResult::MainTaskCompleted;
                }
                self.polled.fetch_add(1, Ordering::Relaxed);
            }
            // We didn't finish all the ready tasks. If there are sleeping threads, post a
            // notification to wake one up.
            let mut threads_state = self.threads_state.load(Ordering::Relaxed);
            loop {
                if threads_sleeping(threads_state) == 0 {
                    // All threads are awake now. Prevent starvation.
                    return PollReadyTasksResult::MoreReady;
                }
                if threads_notified(threads_state) >= threads_sleeping(threads_state) {
                    // All sleeping threads have been notified. Keep going and poll more tasks.
                    break;
                }
                match self.try_notify(threads_state) {
                    Ok(()) => break,
                    Err(s) => threads_state = s,
                }
            }
        }
    }

    pub fn spawn(self: &Arc<Self>, scope: &ScopeRef, future: AtomicFuture<'static>) -> usize {
        let next_id = self.task_count.fetch_add(1, Ordering::Relaxed);
        let task = {
            let task = Task::new(next_id, scope.clone(), future);
            if !scope.insert_task(next_id, task.clone()) {
                return usize::MAX;
            }
            task
        };
        task.wake();
        next_id
    }

    pub fn spawn_local<F: Future<Output = R> + 'static, R: 'static>(
        self: &Arc<Self>,
        scope: &ScopeRef,
        future: F,
        detached: bool,
    ) -> usize {
        if !self.is_local {
            panic!(
                "Error: called `spawn_local` on multithreaded executor. \
                 Use `spawn` or a `LocalExecutor` instead."
            );
        }

        // SAFETY: We've confirmed that the futures here will never be used across multiple threads,
        // so the Send requirements that `new_local` requires should be met.
        self.spawn(scope, unsafe { AtomicFuture::new_local(future, detached) })
    }

    /// Spawns the main future.
    pub fn spawn_main(self: &Arc<Self>, root_scope: &ScopeRef, future: AtomicFuture<'static>) {
        let task = Task::new(MAIN_TASK_ID, root_scope.clone(), future);
        if !root_scope.insert_task(MAIN_TASK_ID, task.clone()) {
            panic!("Could not spawn main task");
        }
        task.wake();
    }

    pub fn notify_task_ready(&self) {
        // Only post if there's no thread running (or soon to be running). If we happen to be
        // running on a thread for this executor, then threads_state won't be equal to num_threads,
        // which means notifications only get fired if this is from a non-async thread, or a thread
        // that belongs to a different executor. We use SeqCst ordering here to make sure this load
        // happens *after* the change to ready_tasks and to synchronize with worker_lifecycle.
        let mut threads_state = self.threads_state.load(Ordering::SeqCst);

        // We compare threads_state directly against self.num_threads (which means that
        // notifications must be zero) because we only want to notify if there are no pending
        // notifications and *all* threads are currently asleep.
        while threads_state == self.num_threads as u16 {
            match self.try_notify(threads_state) {
                Ok(()) => break,
                Err(s) => threads_state = s,
            }
        }
    }

    /// Tries to notify a thread to wake up. Returns threads_state if it fails.
    fn try_notify(&self, old_threads_state: u16) -> Result<(), u16> {
        self.threads_state
            .compare_exchange_weak(
                old_threads_state,
                old_threads_state + 0x100, // <- Add one to notifications.
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .map(|_| self.notify_id(TASK_READY_WAKEUP_ID))
    }

    pub fn wake_one_thread(&self) {
        let mut threads_state = self.threads_state.load(Ordering::Relaxed);
        let current_sleeping = threads_sleeping(threads_state);
        if current_sleeping == 0 {
            return;
        }
        while threads_notified(threads_state) == 0
            && threads_sleeping(threads_state) >= current_sleeping
        {
            match self.try_notify(threads_state) {
                Ok(()) => break,
                Err(s) => threads_state = s,
            }
        }
    }

    pub fn notify_id(&self, id: u64) {
        let up = zx::UserPacket::from_u8_array([0; 32]);
        let packet = zx::Packet::from_user_packet(id, 0 /* status??? */, up);
        if let Err(e) = self.port.queue(&packet) {
            // TODO: logging
            eprintln!("Failed to queue notify in port: {:?}", e);
        }
    }

    pub fn deliver_packet(&self, key: u64, packet: zx::Packet) {
        let receiver = match self.receivers.lock().get(key) {
            // Clone the `Arc` so that we don't hold the lock
            // any longer than absolutely necessary.
            // The `receive_packet` impl may be arbitrarily complex.
            Some(receiver) => receiver.clone(),
            None => return,
        };
        receiver.receive_packet(packet);
    }

    /// Returns the current reading of the monotonic clock.
    ///
    /// For test executors running in fake time, returns the reading of the
    /// fake monotonic clock.
    pub fn now(&self) -> MonotonicInstant {
        match &self.time {
            ExecutorTime::RealTime => MonotonicInstant::from_zx(zx::MonotonicInstant::get()),
            ExecutorTime::FakeTime { mono_reading_ns: t, .. } => {
                MonotonicInstant::from_nanos(t.load(Ordering::Relaxed))
            }
        }
    }

    /// Returns the current reading of the boot clock.
    ///
    /// For test executors running in fake time, returns the reading of the
    /// fake boot clock.
    pub fn boot_now(&self) -> BootInstant {
        match &self.time {
            ExecutorTime::RealTime => BootInstant::from_zx(zx::BootInstant::get()),

            ExecutorTime::FakeTime { mono_reading_ns: t, mono_to_boot_offset_ns } => {
                // The two atomic values are loaded one after the other. This should
                // not normally be an issue in tests.
                let fake_mono_now = MonotonicInstant::from_nanos(t.load(Ordering::Relaxed));
                let boot_offset_ns = mono_to_boot_offset_ns.load(Ordering::Relaxed);
                BootInstant::from_nanos(fake_mono_now.into_nanos() + boot_offset_ns)
            }
        }
    }

    /// Sets the reading of the fake monotonic clock.
    ///
    /// # Panics
    ///
    /// If called on an executor that runs in real time.
    pub fn set_fake_time(&self, new: MonotonicInstant) {
        let boot_offset_ns = match &self.time {
            ExecutorTime::RealTime => {
                panic!("Error: called `set_fake_time` on an executor using actual time.")
            }
            ExecutorTime::FakeTime { mono_reading_ns: t, mono_to_boot_offset_ns } => {
                t.store(new.into_nanos(), Ordering::Relaxed);
                mono_to_boot_offset_ns.load(Ordering::Relaxed)
            }
        };
        self.monotonic_timers.maybe_notify(new);

        // Changing fake time also affects boot time.  Notify boot clocks as well.
        let new_boot_time = BootInstant::from_nanos(new.into_nanos() + boot_offset_ns);
        self.boot_timers.maybe_notify(new_boot_time);
    }

    // Sets a new offset between boot and monotonic time.
    //
    // Only works for executors operating in fake time.
    // The change in the fake offset will wake expired boot timers.
    pub fn set_fake_boot_to_mono_offset(&self, offset: BootDuration) {
        let mono_now_ns = match &self.time {
            ExecutorTime::RealTime => {
                panic!("Error: called `set_fake_boot_to_mono_offset` on an executor using actual time.")
            }
            ExecutorTime::FakeTime { mono_reading_ns: t, mono_to_boot_offset_ns: b } => {
                // We ignore the non-atomic update between b and t, it is likely
                // not relevant in tests.
                b.store(offset.into_nanos(), Ordering::Relaxed);
                t.load(Ordering::Relaxed)
            }
        };
        let new_boot_now = BootInstant::from_nanos(mono_now_ns) + offset;
        self.boot_timers.maybe_notify(new_boot_now);
    }

    /// Returns `true` if this executor is running in real time.  Returns
    /// `false` if this executor si running in fake time.
    pub fn is_real_time(&self) -> bool {
        matches!(self.time, ExecutorTime::RealTime)
    }

    /// Must be called before `on_parent_drop`.
    ///
    /// Done flag must be set before dropping packet receivers
    /// so that future receivers that attempt to deregister themselves
    /// know that it's okay if their entries are already missing.
    pub fn mark_done(&self) {
        self.done.store(true, Ordering::SeqCst);

        // Make sure there's at least one notification outstanding per thread to wake up all
        // workers. This might be more notifications than required, but this way we don't have to
        // worry about races where tasks are just about to sleep; when a task receives the
        // notification, it will check done and terminate.
        let mut threads_state = self.threads_state.load(Ordering::Relaxed);
        let num_threads = self.num_threads;
        loop {
            let notified = threads_notified(threads_state);
            if notified >= num_threads {
                break;
            }
            match self.threads_state.compare_exchange_weak(
                threads_state,
                make_threads_state(threads_sleeping(threads_state), num_threads),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    for _ in notified..num_threads {
                        self.notify_id(TASK_READY_WAKEUP_ID);
                    }
                    return;
                }
                Err(old) => threads_state = old,
            }
        }
    }

    /// Notes about the lifecycle of an Executor.
    ///
    /// a) The Executor stands as the only way to run a reactor based on a Fuchsia port, but the
    /// lifecycle of the port itself is not currently tied to it. Executor vends clones of its
    /// inner Arc structure to all receivers, so we don't have a type-safe way of ensuring that
    /// the port is dropped alongside the Executor as it should.
    /// TODO(https://fxbug.dev/42154828): Ensure the port goes away with the executor.
    ///
    /// b) The Executor's lifetime is also tied to the thread-local variable pointing to the
    /// "current" executor being set, and that's unset when the executor is dropped.
    ///
    /// Point (a) is related to "what happens if I use a receiver after the executor is dropped",
    /// and point (b) is related to "what happens when I try to create a new receiver when there
    /// is no executor".
    ///
    /// Tokio, for example, encodes the lifetime of the reactor separately from the thread-local
    /// storage [1]. And the reactor discourages usage of strong references to it by vending weak
    /// references to it [2] instead of strong.
    ///
    /// There are pros and cons to both strategies. For (a), tokio encourages (but doesn't
    /// enforce [3]) type-safety by vending weak pointers, but those add runtime overhead when
    /// upgrading pointers. For (b) the difference mostly stand for "when is it safe to use IO
    /// objects/receivers". Tokio says it's only safe to use them whenever a guard is in scope.
    /// Fuchsia-async says it's safe to use them when a fuchsia_async::Executor is still in scope
    /// in that thread.
    ///
    /// This acts as a prelude to the panic encoded in Executor::drop when receivers haven't
    /// unregistered themselves when the executor drops. The choice to panic was made based on
    /// patterns in fuchsia-async that may come to change:
    ///
    /// - Executor vends strong references to itself and those references are *stored* by most
    /// receiver implementations (as opposed to reached out on TLS every time).
    /// - Fuchsia-async objects return zx::Status on wait calls, there isn't an appropriate and
    /// easy to understand error to return when polling on an extinct executor.
    /// - All receivers are implemented in this crate and well-known.
    ///
    /// [1]: https://docs.rs/tokio/1.5.0/tokio/runtime/struct.Runtime.html#method.enter
    /// [2]: https://github.com/tokio-rs/tokio/blob/b42f21ec3e212ace25331d0c13889a45769e6006/tokio/src/signal/unix/driver.rs#L35
    /// [3]: by returning an upgraded Arc, tokio trusts callers to not "use it for too long", an
    /// opaque non-clone-copy-or-send guard would be stronger than this. See:
    /// https://github.com/tokio-rs/tokio/blob/b42f21ec3e212ace25331d0c13889a45769e6006/tokio/src/io/driver/mod.rs#L297
    pub fn on_parent_drop(&self, root_scope: &ScopeRef) {
        // Drop all tasks.
        // Any use of fasync::unblock can involve a waker. Wakers hold weak references to tasks, but
        // as part of waking, there's an upgrade to a strong reference, so for a small amount of
        // time `fasync::unblock` can hold a strong reference to a task which in turn holds the
        // future for the task which in turn could hold references to receivers, which, if we did
        // nothing about it, would trip the assertion below. For that reason, we forcibly drop the
        // task futures here.
        root_scope.drop_all_tasks();

        // Drop all of the uncompleted tasks
        while let Some(_) = self.ready_tasks.pop() {}

        // Unregister the timer receivers so that we can perform the check below.
        self.receivers.lock().remove(self.monotonic_timers.port_key());
        self.receivers.lock().remove(self.boot_timers.port_key());

        // Do not allow any receivers to outlive the executor. That's very likely a bug waiting to
        // happen. See discussion above.
        //
        // If you're here because you hit this panic check your code for:
        //
        // - A struct that contains a fuchsia_async::Executor NOT in the last position (last
        // position gets dropped last: https://doc.rust-lang.org/reference/destructors.html).
        //
        // - A function scope that contains a fuchsia_async::Executor NOT in the first position
        // (first position in function scope gets dropped last:
        // https://doc.rust-lang.org/reference/destructors.html?highlight=scope#drop-scopes).
        //
        // - A function that holds a `fuchsia_async::Executor` in scope and whose last statement
        // contains a temporary (temporaries are dropped after the function scope:
        // https://doc.rust-lang.org/reference/destructors.html#temporary-scopes). This usually
        // looks like a `match` statement at the end of the function without a semicolon.
        //
        // - Storing channel and FIDL objects in static variables.
        //
        // - fuchsia_async::unblock calls that move channels or FIDL objects to another thread.
        assert!(
            self.receivers.lock().mapping.is_empty(),
            "receivers must not outlive their executor"
        );

        // Remove the thread-local executor set in `new`.
        EHandle::rm_local();
    }

    // The debugger looks for this function on the stack, so if its (fully-qualified) name changes,
    // the debugger needs to be updated.
    // LINT.IfChange
    pub fn worker_lifecycle<const UNTIL_STALLED: bool>(self: &Arc<Executor>) {
        // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
        loop {
            // Keep track of whether we are considered asleep.
            let mut sleeping = false;

            match self.poll_ready_tasks() {
                PollReadyTasksResult::NoneReady => {
                    // No more tasks, indicate we are sleeping. We use SeqCst ordering because we
                    // want this change here to happen *before* we check ready_tasks below. This
                    // synchronizes with notify_task_ready which is called *after* a task is added
                    // to ready_tasks.
                    self.threads_state.fetch_add(1, Ordering::SeqCst);
                    // Check ready tasks again. If a task got posted, wake up. This has to be done
                    // because a notification won't get sent if there is at least one active thread
                    // so there's a window between the preceding two lines where a task could be
                    // made ready and a notification is not sent because it looks like there is at
                    // least one thread running.
                    if self.ready_tasks.is_empty() {
                        sleeping = true;
                    } else {
                        // We lost a race, we're no longer sleeping.
                        self.threads_state.fetch_sub(1, Ordering::Relaxed);
                    }
                }
                PollReadyTasksResult::MoreReady => {}
                PollReadyTasksResult::MainTaskCompleted => return,
            }

            // Check done here after updating threads_state to avoid shutdown races.
            if self.done.load(Ordering::SeqCst) {
                return;
            }

            enum Work {
                None,
                Packet(zx::Packet),
                Stalled,
            }

            let mut notified = false;
            let work = {
                // If we're considered awake choose INFINITE_PAST which will make the wait call
                // return immediately.  Otherwise, wait until a packet arrives.
                let deadline = if !sleeping || UNTIL_STALLED {
                    zx::Instant::INFINITE_PAST
                } else {
                    zx::Instant::INFINITE
                };

                match self.port.wait(deadline) {
                    Ok(packet) => {
                        if packet.key() == TASK_READY_WAKEUP_ID {
                            notified = true;
                            Work::None
                        } else {
                            Work::Packet(packet)
                        }
                    }
                    Err(zx::Status::TIMED_OUT) => {
                        if !UNTIL_STALLED || !sleeping {
                            Work::None
                        } else {
                            Work::Stalled
                        }
                    }
                    Err(status) => {
                        panic!("Error calling port wait: {:?}", status);
                    }
                }
            };

            let threads_state_sub = make_threads_state(sleeping as u8, notified as u8);
            if threads_state_sub > 0 {
                self.threads_state.fetch_sub(threads_state_sub, Ordering::Relaxed);
            }

            match work {
                Work::Packet(packet) => {
                    self.deliver_packet(packet.key(), packet);
                }
                Work::None => {}
                Work::Stalled => return,
            }
        }
    }

    /// Drops the main task.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the executor isn't running.
    pub(super) unsafe fn drop_main_task(&self, root_scope: &ScopeRef) {
        root_scope.drop_task_unchecked(MAIN_TASK_ID);
    }

    fn try_poll(&self, task: &Arc<Task>) -> bool {
        // SAFETY: We meet the contract for RawWaker/RawWakerVtable.
        let task_waker = unsafe {
            Waker::from_raw(RawWaker::new(Arc::as_ptr(task) as *const (), &BORROWED_VTABLE))
        };
        match task.future.try_poll(&mut Context::from_waker(&task_waker)) {
            AttemptPollResult::Yield => {
                self.ready_tasks.push(task.clone());
                false
            }
            AttemptPollResult::IFinished | AttemptPollResult::Cancelled => {
                task.scope.task_did_finish(task.id);
                true
            }
            _ => false,
        }
    }

    /// Returns the monotonic timers.
    pub fn monotonic_timers(&self) -> &Timers<MonotonicInstant> {
        &self.monotonic_timers
    }

    /// Returns the boot timers.
    pub fn boot_timers(&self) -> &Timers<BootInstant> {
        &self.boot_timers
    }
}

#[cfg(test)]
impl Drop for Executor {
    fn drop(&mut self) {
        ACTIVE_EXECUTORS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A handle to an executor.
#[derive(Clone)]
pub struct EHandle {
    // LINT.IfChange
    pub(super) root_scope: ScopeRef,
    // LINT.ThenChange(//src/developer/debug/zxdb/console/commands/verb_async_backtrace.cc)
}

impl fmt::Debug for EHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EHandle").field("port", &self.inner().port).finish()
    }
}

impl EHandle {
    /// Returns the thread-local executor.
    ///
    /// # Panics
    ///
    /// If called outside the context of an active async executor.
    pub fn local() -> Self {
        let root_scope = EXECUTOR
            .with(|e| e.borrow().as_ref().map(|x| x.clone()))
            .expect("Fuchsia Executor must be created first");

        EHandle { root_scope }
    }

    pub(super) fn rm_local() {
        EXECUTOR.with(|e| *e.borrow_mut() = None);
    }

    /// The root scope of the executor.
    ///
    /// This can be used to spawn tasks that live as long as the executor, and
    /// to create shorter-lived child scopes.
    ///
    /// Most users should create an owned scope with
    /// [`Scope::new`][crate::Scope::new] instead of using this method.
    pub fn root_scope(&self) -> &ScopeRef {
        &self.root_scope
    }

    /// Get a reference to the Fuchsia `zx::Port` being used to listen for events.
    pub fn port(&self) -> &zx::Port {
        &self.inner().port
    }

    /// Registers a `PacketReceiver` with the executor and returns a registration.
    /// The `PacketReceiver` will be deregistered when the `Registration` is dropped.
    pub fn register_receiver<T>(&self, receiver: Arc<T>) -> ReceiverRegistration<T>
    where
        T: PacketReceiver,
    {
        self.inner().receivers.lock().insert(|key| {
            (receiver.clone(), ReceiverRegistration { ehandle: self.clone(), key, receiver })
        })
    }

    #[inline(always)]
    pub(crate) fn inner(&self) -> &Arc<Executor> {
        &self.root_scope.executor()
    }

    pub(crate) fn deregister_receiver(&self, key: u64) {
        let mut lock = self.inner().receivers.lock();
        if lock.contains(key) {
            lock.remove(key);
        } else {
            // The executor is shutting down and already removed the entry.
            assert!(self.inner().done.load(Ordering::SeqCst), "Missing receiver to deregister");
        }
    }

    /// See `Inner::spawn`.
    pub(crate) fn spawn<R: Send + 'static>(
        &self,
        scope: &ScopeRef,
        future: impl Future<Output = R> + Send + 'static,
    ) -> usize {
        self.inner().spawn(scope, AtomicFuture::new(future, false))
    }

    /// Spawn a new task to be run on this executor.
    ///
    /// Tasks spawned using this method must be thread-safe (implement the `Send` trait), as they
    /// may be run on either a singlethreaded or multithreaded executor.
    pub fn spawn_detached(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.inner().spawn(self.root_scope(), AtomicFuture::new(future, true));
    }

    /// See `Inner::spawn_local`.
    pub(crate) fn spawn_local<R: 'static>(
        &self,
        scope: &ScopeRef,
        future: impl Future<Output = R> + 'static,
    ) -> usize {
        self.inner().spawn_local(scope, future, false)
    }

    /// Spawn a new task to be run on this executor.
    ///
    /// This is similar to the `spawn_detached` method, but tasks spawned using this method do not
    /// have to be threads-safe (implement the `Send` trait). In return, this method requires that
    /// this executor is a LocalExecutor.
    pub fn spawn_local_detached(&self, future: impl Future<Output = ()> + 'static) {
        self.inner().spawn_local(self.root_scope(), future, true);
    }

    pub(crate) fn mono_timers(&self) -> &Arc<Timers<MonotonicInstant>> {
        &self.inner().monotonic_timers
    }

    pub(crate) fn boot_timers(&self) -> &Arc<Timers<BootInstant>> {
        &self.inner().boot_timers
    }
}

pub(super) struct Task {
    id: usize,
    pub(super) future: AtomicFuture<'static>,
    pub(super) scope: ScopeRef,
}

impl Task {
    fn new(id: usize, scope: ScopeRef, future: AtomicFuture<'static>) -> Arc<Self> {
        let this = Arc::new(Self { id, future, scope });

        // Take a weak reference now to be used as a waker.
        let _ = Arc::downgrade(&this).into_raw();

        this
    }

    pub(super) fn wake(self: &Arc<Self>) {
        if self.future.mark_ready() {
            self.scope.executor().ready_tasks.push(self.clone());
            self.scope.executor().notify_task_ready();
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        // SAFETY: This balances the `into_raw` in `new`.
        unsafe {
            // TODO(https://fxbug.dev/328126836): We might need to revisit this when pointer
            // provenance lands.
            Weak::from_raw(self);
        }
    }
}

// This vtable is used for the waker that exists for the lifetime of the task, which gets dropped
// above, so these functions never drop.
static BORROWED_VTABLE: RawWakerVTable =
    RawWakerVTable::new(waker_clone, waker_wake_by_ref, waker_wake_by_ref, waker_noop);

static VTABLE: RawWakerVTable =
    RawWakerVTable::new(waker_clone, waker_wake, waker_wake_by_ref, waker_drop);

fn waker_clone(weak_raw: *const ()) -> RawWaker {
    // SAFETY: `weak_raw` comes from a previous call to `into_raw`.
    let weak = ManuallyDrop::new(unsafe { Weak::from_raw(weak_raw as *const Task) });
    RawWaker::new((*weak).clone().into_raw() as *const _, &VTABLE)
}

fn waker_wake(weak_raw: *const ()) {
    // SAFETY: `weak_raw` comes from a previous call to `into_raw`.
    if let Some(task) = unsafe { Weak::from_raw(weak_raw as *const Task) }.upgrade() {
        task.wake();
    }
}

fn waker_wake_by_ref(weak_raw: *const ()) {
    // SAFETY: `weak_raw` comes from a previous call to `into_raw`.
    if let Some(task) =
        ManuallyDrop::new(unsafe { Weak::from_raw(weak_raw as *const Task) }).upgrade()
    {
        task.wake();
    }
}

fn waker_noop(_weak_raw: *const ()) {}

fn waker_drop(weak_raw: *const ()) {
    // SAFETY: `weak_raw` comes from a previous call to `into_raw`.
    unsafe {
        Weak::from_raw(weak_raw as *const Task);
    }
}

#[cfg(test)]
mod tests {
    use super::ACTIVE_EXECUTORS;
    use crate::SendExecutor;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_no_leaks() {
        std::thread::spawn(|| SendExecutor::new(1).run(async {})).join().unwrap();

        assert_eq!(ACTIVE_EXECUTORS.load(Ordering::Relaxed), 0);
    }
}
