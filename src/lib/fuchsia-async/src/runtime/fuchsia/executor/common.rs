// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::super::timer::{TimeWaker, TimerHandle, TimerHeap};
use super::{
    instrumentation::{Collector, LocalCollector, WakeupReason},
    packets::{PacketReceiver, PacketReceiverMap, ReceiverRegistration},
    time::Time,
    ScopeRef,
};
use crate::atomic_future::{AtomicFuture, AttemptPollResult};
use crossbeam::queue::SegQueue;
use fuchsia_sync::Mutex;
use fuchsia_zircon::{self as zx};
use std::{
    any::Any,
    cell::RefCell,
    fmt,
    future::Future,
    mem::ManuallyDrop,
    panic::Location,
    sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU64, AtomicUsize, Ordering},
    sync::{Arc, Weak},
    task::{Context, RawWaker, RawWakerVTable, Waker},
    u64, usize,
};

pub(crate) const TASK_READY_WAKEUP_ID: u64 = u64::MAX - 1;

/// The id of the main task, which is a virtual task that lives from construction
/// to destruction of the executor. The main task may correspond to multiple
/// main futures, in cases where the executor runs multiple times during its lifetime.
pub(crate) const MAIN_TASK_ID: usize = 0;

thread_local!(
    static EXECUTOR: RefCell<Option<(ScopeRef, TimerHeap)>> = RefCell::new(None)
);

pub(crate) fn with_local_timer_heap<F, R>(f: F) -> R
where
    F: FnOnce(&mut TimerHeap) -> R,
{
    EXECUTOR.with(|e| {
        (f)(&mut e
            .borrow_mut()
            .as_mut()
            .expect("can't get timer heap before fuchsia_async::Executor is initialized")
            .1)
    })
}

pub enum ExecutorTime {
    RealTime,
    FakeTime(AtomicI64),
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

pub(super) struct Executor {
    pub(super) port: zx::Port,
    pub(super) done: AtomicBool,
    is_local: bool,
    receivers: Mutex<PacketReceiverMap<Arc<dyn PacketReceiver>>>,
    task_count: AtomicUsize,
    pub(super) ready_tasks: SegQueue<Arc<Task>>,
    time: ExecutorTime,
    pub(super) collector: Collector,
    pub(super) source: Option<&'static Location<'static>>,
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
    #[cfg_attr(trace_level_logging, track_caller)]
    pub fn new(time: ExecutorTime, is_local: bool, num_threads: u8) -> Self {
        #[cfg(trace_level_logging)]
        let source = Some(Location::caller());
        #[cfg(not(trace_level_logging))]
        let source = None;

        let collector = Collector::new();
        Executor {
            port: zx::Port::create(),
            done: AtomicBool::new(false),
            is_local,
            receivers: Mutex::new(PacketReceiverMap::new()),
            task_count: AtomicUsize::new(MAIN_TASK_ID + 1),
            ready_tasks: SegQueue::new(),
            time,
            collector,
            source,
            threads_state: AtomicU16::new(0),
            num_threads,
            polled: AtomicU64::new(0),
            owner_data: Mutex::new(None),
        }
    }

    pub fn set_local(root_scope: ScopeRef, timers: TimerHeap) {
        EXECUTOR.with(|e| {
            let mut e = e.borrow_mut();
            assert!(e.is_none(), "Cannot create multiple Fuchsia Executors");
            *e = Some((root_scope, timers));
        });
    }

    fn poll_ready_tasks(&self, local_collector: &mut LocalCollector<'_>) -> PollReadyTasksResult {
        loop {
            for _ in 0..16 {
                let Some(task) = self.ready_tasks.pop() else {
                    return PollReadyTasksResult::NoneReady;
                };
                let complete = self.try_poll(&task);
                local_collector.task_polled(
                    task.id,
                    task.source(),
                    complete,
                    self.ready_tasks.len(),
                );
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

    #[cfg_attr(trace_level_logging, track_caller)]
    pub fn spawn(self: &Arc<Self>, scope: &ScopeRef, future: AtomicFuture<'static>) -> usize {
        let next_id = self.task_count.fetch_add(1, Ordering::Relaxed);
        let task = {
            let mut scope_state = scope.lock();
            if scope_state.cancelled {
                return usize::MAX;
            }
            let task = Task::new(next_id, scope.clone(), future);
            scope_state.all_tasks.insert(next_id, task.clone());
            task
        };
        self.collector.task_created(next_id, task.source());
        task.wake();
        next_id
    }

    #[cfg_attr(trace_level_logging, track_caller)]
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
        self.collector.task_created(MAIN_TASK_ID, task.source());
        assert!(
            root_scope.lock().all_tasks.insert(MAIN_TASK_ID, task.clone()).is_none(),
            "Existing main task"
        );
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

    pub fn deliver_packet(&self, key: usize, packet: zx::Packet) {
        let receiver = match self.receivers.lock().get(key) {
            // Clone the `Arc` so that we don't hold the lock
            // any longer than absolutely necessary.
            // The `receive_packet` impl may be arbitrarily complex.
            Some(receiver) => receiver.clone(),
            None => return,
        };
        receiver.receive_packet(packet);
    }

    pub fn now(&self) -> Time {
        match &self.time {
            ExecutorTime::RealTime => Time::from_zx(zx::Time::get_monotonic()),
            ExecutorTime::FakeTime(t) => Time::from_nanos(t.load(Ordering::Relaxed)),
        }
    }

    pub fn set_fake_time(&self, new: Time) {
        match &self.time {
            ExecutorTime::RealTime => {
                panic!("Error: called `set_fake_time` on an executor using actual time.")
            }
            ExecutorTime::FakeTime(t) => t.store(new.into_nanos(), Ordering::Relaxed),
        }
    }

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

        // Synthetic main task marked completed
        self.collector.task_completed(MAIN_TASK_ID, self.source);

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
        let mut local_collector = self.collector.create_local_collector();

        loop {
            // Keep track of whether we are considered asleep.
            let mut sleeping = false;

            match self.poll_ready_tasks(&mut local_collector) {
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
                Timer(TimeWaker),
                Stalled,
            }

            let mut notified = false;
            let work = with_local_timer_heap(|timer_heap| {
                // If we're considered awake choose INFINITE_PAST which will make the wait call
                // return immediately. Otherwise choose a deadline from the timers.
                let deadline = if !sleeping || UNTIL_STALLED {
                    Time::INFINITE_PAST
                } else {
                    timer_heap.next_deadline().map(|t| t.time()).unwrap_or(Time::INFINITE)
                };

                local_collector.will_wait();

                // into_zx: we are using real time, so the time is a monotonic time.
                match self.port.wait(deadline.into_zx()) {
                    Ok(packet) => {
                        if packet.key() == TASK_READY_WAKEUP_ID {
                            local_collector.woke_up(WakeupReason::Notification);
                            notified = true;
                            Work::None
                        } else {
                            Work::Packet(packet)
                        }
                    }
                    Err(zx::Status::TIMED_OUT) => {
                        if !sleeping {
                            Work::None
                        } else if UNTIL_STALLED {
                            // Fire timers if using fake time.
                            if !self.is_real_time() {
                                if let Some(deadline) = timer_heap.next_deadline().map(|t| t.time())
                                {
                                    if deadline <= self.now() {
                                        return Work::Timer(timer_heap.pop().unwrap());
                                    }
                                }
                            }
                            Work::Stalled
                        } else {
                            Work::Timer(timer_heap.pop().unwrap())
                        }
                    }
                    Err(status) => {
                        panic!("Error calling port wait: {:?}", status);
                    }
                }
            });

            let threads_state_sub = make_threads_state(sleeping as u8, notified as u8);
            if threads_state_sub > 0 {
                self.threads_state.fetch_sub(threads_state_sub, Ordering::Relaxed);
            }

            match work {
                Work::Packet(packet) => {
                    local_collector.woke_up(WakeupReason::Io);
                    self.deliver_packet(packet.key() as usize, packet);
                }
                Work::Timer(timer) => {
                    local_collector.woke_up(WakeupReason::Deadline);
                    timer.wake();
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
        if let Some(task) = root_scope.lock().all_tasks.remove(&MAIN_TASK_ID) {
            // Even though we've removed the task from active tasks, it could still be in
            // pending_tasks, so we have to drop the future here. At time of writing, this is only
            // used by the local executor and there could only be something in ready_tasks if
            // there's a panic.
            task.future.drop_future_unchecked();
        }
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
            AttemptPollResult::IFinished => {
                let mut waker = None;
                {
                    let mut tasks = task.scope.lock();
                    if !task.future.is_detached_or_cancelled() {
                        waker = tasks.join_wakers.remove(&task.id);
                    } else if task.id != MAIN_TASK_ID {
                        tasks.all_tasks.remove(&task.id);
                    }
                }
                if let Some(waker) = waker {
                    waker.wake();
                }
                true
            }
            AttemptPollResult::Cancelled => {
                task.scope.lock().all_tasks.remove(&task.id);
                true
            }
            _ => false,
        }
    }
}

/// A handle to an executor.
#[derive(Clone)]
pub struct EHandle {
    pub(super) root_scope: ScopeRef,
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
            .with(|e| e.borrow().as_ref().map(|x| x.0.clone()))
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
        let key = self.inner().receivers.lock().insert(receiver.clone()) as u64;

        ReceiverRegistration { ehandle: self.clone(), key, receiver }
    }

    #[inline(always)]
    pub(super) fn inner(&self) -> &Arc<Executor> {
        &self.root_scope.executor()
    }

    pub(crate) fn deregister_receiver(&self, key: u64) {
        let key = key as usize;
        let mut lock = self.inner().receivers.lock();
        if lock.contains(key) {
            lock.remove(key);
        } else {
            // The executor is shutting down and already removed the entry.
            assert!(self.inner().done.load(Ordering::SeqCst), "Missing receiver to deregister");
        }
    }

    pub(crate) fn register_timer(time: Time, handle: TimerHandle) {
        with_local_timer_heap(|timer_heap| {
            timer_heap.add_timer(time, handle);
        });
    }

    /// See `Inner::spawn`.
    #[cfg_attr(trace_level_logging, track_caller)]
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
    #[cfg_attr(trace_level_logging, track_caller)]
    pub fn spawn_detached(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.inner().spawn(self.root_scope(), AtomicFuture::new(future, true));
    }

    /// See `Inner::spawn_local`.
    #[cfg_attr(trace_level_logging, track_caller)]
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
    #[cfg_attr(trace_level_logging, track_caller)]
    pub fn spawn_local_detached(&self, future: impl Future<Output = ()> + 'static) {
        self.inner().spawn_local(self.root_scope(), future, true);
    }
}

pub(super) struct Task {
    id: usize,
    pub(super) future: AtomicFuture<'static>,
    pub(super) scope: ScopeRef,
    #[cfg(trace_level_logging)]
    source: &'static Location<'static>,
}

impl Task {
    #[cfg_attr(trace_level_logging, track_caller)]
    fn new(id: usize, scope: ScopeRef, future: AtomicFuture<'static>) -> Arc<Self> {
        let this = Arc::new(Self {
            id,
            future,
            scope,
            #[cfg(trace_level_logging)]
            source: Location::caller(),
        });

        // Take a weak reference now to be used as a waker.
        let _ = Arc::downgrade(&this).into_raw();

        this
    }

    fn wake(self: &Arc<Self>) {
        if self.future.mark_ready() {
            self.scope.executor().ready_tasks.push(self.clone());
            self.scope.executor().notify_task_ready();
        }
    }

    fn source(&self) -> Option<&'static Location<'static>> {
        #[cfg(trace_level_logging)]
        {
            Some(self.source)
        }
        #[cfg(not(trace_level_logging))]
        None
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
    use {
        crate::{LocalExecutor, Task},
        std::{
            future::poll_fn,
            sync::{
                atomic::{AtomicU32, Ordering},
                Arc,
            },
            task::Poll,
        },
    };

    async fn yield_to_executor() {
        let mut done = false;
        poll_fn(|cx| {
            if done {
                Poll::Ready(())
            } else {
                done = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    #[test]
    fn test_detach() {
        let mut e = LocalExecutor::new();
        e.run_singlethreaded(async {
            let counter = Arc::new(AtomicU32::new(0));

            {
                let counter = counter.clone();
                Task::spawn(async move {
                    for _ in 0..5 {
                        yield_to_executor().await;
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .detach();
            }

            while counter.load(Ordering::Relaxed) != 5 {
                yield_to_executor().await;
            }
        });

        assert!(e.ehandle.root_scope.lock().join_wakers.is_empty());
    }

    #[test]
    fn test_cancel() {
        let mut e = LocalExecutor::new();
        e.run_singlethreaded(async {
            let ref_count = Arc::new(());
            // First, just drop the task.
            {
                let ref_count = ref_count.clone();
                let _ = Task::spawn(async move {
                    let _ref_count = ref_count;
                    let _: () = std::future::pending().await;
                });
            }

            while Arc::strong_count(&ref_count) != 1 {
                yield_to_executor().await;
            }

            // Now try explicitly cancelling.
            let task = {
                let ref_count = ref_count.clone();
                Task::spawn(async move {
                    let _ref_count = ref_count;
                    let _: () = std::future::pending().await;
                })
            };

            assert_eq!(task.cancel(), None);
            while Arc::strong_count(&ref_count) != 1 {
                yield_to_executor().await;
            }

            // Now cancel a task that has already finished.
            let task = {
                let ref_count = ref_count.clone();
                Task::spawn(async move {
                    let _ref_count = ref_count;
                })
            };

            // Wait for it to finish.
            while Arc::strong_count(&ref_count) != 1 {
                yield_to_executor().await;
            }

            assert_eq!(task.cancel(), Some(()));
        });

        assert!(e.ehandle.root_scope.lock().join_wakers.is_empty());
    }
}
