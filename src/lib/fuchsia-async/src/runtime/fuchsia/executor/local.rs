// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::super::timer::TimerHeap;
use super::{
    common::{with_local_timer_heap, EHandle, ExecutorTime, Inner},
    time::Time,
};
use futures::{
    future::{self, Either},
    task::{AtomicWaker, FutureObj, LocalFutureObj},
    FutureExt,
};
use pin_utils::pin_mut;
use std::{
    fmt,
    future::{poll_fn, Future},
    marker::Unpin,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

/// A single-threaded port-based executor for Fuchsia OS.
///
/// Having a `LocalExecutor` in scope allows the creation and polling of zircon objects, such as
/// [`fuchsia_async::Channel`].
///
/// # Panics
///
/// `LocalExecutor` will panic on drop if any zircon objects attached to it are still alive. In
/// other words, zircon objects backed by a `LocalExecutor` must be dropped before it.
pub struct LocalExecutor {
    /// The inner executor state.
    inner: Arc<Inner>,
}

impl fmt::Debug for LocalExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalExecutor").field("port", &self.inner.port).finish()
    }
}

impl LocalExecutor {
    /// Create a new single-threaded executor running with actual time.
    pub fn new() -> Self {
        let inner = Arc::new(Inner::new(
            ExecutorTime::RealTime,
            /* is_local */ true,
            /* num_threads */ 1,
        ));
        inner.clone().set_local(TimerHeap::default());
        Self { inner }
    }

    /// Run a single future to completion on a single thread, also polling other active tasks.
    pub fn run_singlethreaded<F>(&mut self, main_future: F) -> F::Output
    where
        F: Future,
    {
        assert!(
            self.inner.is_real_time(),
            "Error: called `run_singlethreaded` on an executor using fake time"
        );

        let mut result = None;
        let main_future = main_future.map(|r| result = Some(r));
        pin_mut!(main_future);

        self.run::</* UNTIL_STALLED: */ false>(LocalFutureObj::new(main_future));

        // The main future must have completed.
        result.unwrap()
    }

    fn run<const UNTIL_STALLED: bool>(&mut self, main_future: LocalFutureObj<'_, ()>) {
        /// # Safety
        ///
        /// See the comment below.
        unsafe fn remove_lifetime(obj: FutureObj<'_, ()>) -> FutureObj<'static, ()> {
            std::mem::transmute(obj)
        }

        // SAFETY: This is a single-threaded executor, so the future here will never be used
        // across multiple threads, so we can safely convert from a non-`Send`able future to a
        // `Send`able one.
        let obj = unsafe { main_future.into_future_obj() };

        // SAFETY: Erasing the lifetime is safe because we make sure to drop the main task within
        // the required lifetime.
        self.inner.spawn_main(unsafe { remove_lifetime(obj) });

        struct DropMainTask<'a>(&'a Inner);
        impl Drop for DropMainTask<'_> {
            fn drop(&mut self) {
                // SAFETY: drop_main_tasks requires that the executor isn't running
                // i.e. worker_lifecycle isn't running, which will be the case when this runs.
                unsafe { self.0.drop_main_task() };
            }
        }
        let _drop_main_task = DropMainTask(&self.inner);

        self.inner.worker_lifecycle::<UNTIL_STALLED>();
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> super::instrumentation::Snapshot {
        self.inner.collector.snapshot()
    }
}

impl Drop for LocalExecutor {
    fn drop(&mut self) {
        self.inner.mark_done();
        self.inner.on_parent_drop();
    }
}

/// A single-threaded executor for testing. Exposes additional APIs for manipulating executor state
/// and validating behavior of executed tasks.
pub struct TestExecutor {
    /// LocalExecutor used under the hood, since most of the logic is shared.
    local: LocalExecutor,
}

impl TestExecutor {
    /// Create a new executor for testing.
    pub fn new() -> Self {
        Self { local: LocalExecutor::new() }
    }

    /// Create a new single-threaded executor running with fake time.
    pub fn new_with_fake_time() -> Self {
        let inner = Arc::new(Inner::new(
            ExecutorTime::FakeTime(AtomicI64::new(Time::INFINITE_PAST.into_nanos())),
            /* is_local */ true,
            /* num_threads */ 1,
        ));
        inner.clone().set_local(TimerHeap::default());
        Self { local: LocalExecutor { inner } }
    }

    /// Return the current time according to the executor.
    pub fn now(&self) -> Time {
        self.local.inner.now()
    }

    /// Set the fake time to a given value.
    ///
    /// # Panics
    ///
    /// If the executor was not created with fake time
    pub fn set_fake_time(&self, t: Time) {
        self.local.inner.set_fake_time(t)
    }

    /// Run a single future to completion on a single thread, also polling other active tasks.
    pub fn run_singlethreaded<F>(&mut self, main_future: F) -> F::Output
    where
        F: Future,
    {
        self.local.run_singlethreaded(main_future)
    }

    /// PollResult the future. If it is not ready, dispatch available packets and possibly try
    /// again. Timers will only fire if this executor uses fake time. Never blocks.
    ///
    /// This function is for testing. DO NOT use this function in tests or applications that
    /// involve any interaction with other threads or processes, as those interactions
    /// may become stalled waiting for signals from "the outside world" which is beyond
    /// the knowledge of the executor.
    ///
    /// Unpin: this function requires all futures to be `Unpin`able, so any `!Unpin`
    /// futures must first be pinned using the `pin_mut!` macro from the `pin-utils` crate.
    pub fn run_until_stalled<F>(&mut self, main_future: &mut F) -> Poll<F::Output>
    where
        F: Future + Unpin,
    {
        let mut result = None;
        let main_future = main_future.map(|r| result = Some(r));
        pin_mut!(main_future);

        // Set up an instance of UntilStalledData that works with `poll_until_stalled`.
        struct Cleanup(Arc<Inner>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                *self.0.owner_data.lock() = None;
            }
        }
        let _cleanup = Cleanup(self.local.inner.clone());
        *self.local.inner.owner_data.lock() = Some(Box::new(UntilStalledData { watcher: None }));

        loop {
            self.local.run::</* UNTIL_STALLED: */ true>(LocalFutureObj::new(main_future.as_mut()));
            // If a waker was set by `poll_until_stalled`, disarm, wake, and loop.
            if let Some(watcher) = with_data(|data| data.watcher.take()) {
                watcher.waker.wake();
                // Relaxed ordering is fine here because this atomic is only ever access from the
                // main thread.
                watcher.done.store(true, Ordering::Relaxed);
            } else {
                break;
            }
        }

        if let Some(result) = result {
            Poll::Ready(result)
        } else {
            Poll::Pending
        }
    }

    /// Wake all tasks waiting for expired timers, and return `true` if any task was woken.
    ///
    /// This is intended for use in test code in conjunction with fake time.
    pub fn wake_expired_timers(&mut self) -> bool {
        let now = self.now();
        with_local_timer_heap(|timer_heap| {
            let mut ret = false;
            while let Some(waker) = timer_heap.next_deadline().filter(|waker| waker.time() <= now) {
                waker.wake();
                timer_heap.pop();
                ret = true;
            }
            ret
        })
    }

    /// Wake up the next task waiting for a timer, if any, and return the time for which the
    /// timer was scheduled.
    ///
    /// This is intended for use in test code in conjunction with `run_until_stalled`.
    /// For example, here is how one could test that the Timer future fires after the given
    /// timeout:
    ///
    ///     let deadline = 5.seconds().after_now();
    ///     let mut future = Timer::<Never>::new(deadline);
    ///     assert_eq!(Poll::Pending, exec.run_until_stalled(&mut future));
    ///     assert_eq!(Some(deadline), exec.wake_next_timer());
    ///     assert_eq!(Poll::Ready(()), exec.run_until_stalled(&mut future));
    pub fn wake_next_timer(&mut self) -> Option<Time> {
        with_local_timer_heap(|timer_heap| {
            let deadline = timer_heap.next_deadline().map(|waker| {
                waker.wake();
                waker.time()
            });
            if deadline.is_some() {
                timer_heap.pop();
            }
            deadline
        })
    }

    /// Returns the deadline for the next timer due to expire.
    pub fn next_timer() -> Option<Time> {
        with_local_timer_heap(|timer_heap| timer_heap.next_deadline().map(|t| t.time()))
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> super::instrumentation::Snapshot {
        self.local.inner.collector.snapshot()
    }

    /// Advances fake time to the specified time.  This will only work if the executor is being run
    /// via `TestExecutor::run_until_stalled` and can only be called by one task at a time.  This
    /// will make sure that repeating timers fire as expected.
    ///
    /// # Panics
    ///
    /// Panics if the executor was not created with fake time, and for the same reasons
    /// `poll_until_stalled` can below.
    pub async fn advance_to(time: Time) {
        let ehandle = EHandle::local();
        loop {
            let _: Poll<_> = Self::poll_until_stalled(future::pending::<()>()).await;
            if let Some(next_timer) = Self::next_timer() {
                if next_timer <= time {
                    ehandle.inner.set_fake_time(next_timer);
                    continue;
                }
            }
            ehandle.inner.set_fake_time(time);
            break;
        }
    }

    /// Runs the future until it is ready or the executor is stalled. Returns the state of the
    /// future.
    ///
    /// This will only work if the executor is being run via `TestExecutor::run_until_stalled` and
    /// can only be called by one task at a time.
    ///
    /// This can be used in tests to assert that a future should be pending:
    /// ```
    /// assert!(
    ///     TestExecutor::poll_until_stalled(my_fut).await.is_pending(),
    ///     "my_fut should not be ready!"
    /// );
    /// ```
    ///
    /// If you just want to know when the executor is stalled, you can do:
    /// ```
    /// let _: Poll<()> = TestExecutor::poll_until_stalled(future::pending::<()>()).await;
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if another task is currently trying to use `run_until_stalled`, or the executor is
    /// not using `TestExecutor::run_until_stalled`.
    pub async fn poll_until_stalled<T>(fut: impl Future<Output = T> + Unpin) -> Poll<T> {
        let watcher =
            Arc::new(StalledWatcher { waker: AtomicWaker::new(), done: AtomicBool::new(false) });

        assert!(
            with_data(|data| data.watcher.replace(watcher.clone())).is_none(),
            "Error: Another task has called `poll_until_stalled`."
        );

        struct Watcher(Arc<StalledWatcher>);

        // Make sure we clean up if we're dropped.
        impl Drop for Watcher {
            fn drop(&mut self) {
                if !self.0.done.swap(true, Ordering::Relaxed) {
                    with_data(|data| data.watcher = None);
                }
            }
        }

        let watcher = Watcher(watcher);

        let poll_fn = poll_fn(|cx: &mut Context<'_>| {
            if watcher.0.done.load(Ordering::Relaxed) {
                Poll::Ready(())
            } else {
                watcher.0.waker.register(cx.waker());
                Poll::Pending
            }
        });
        match future::select(poll_fn, fut).await {
            Either::Left(_) => Poll::Pending,
            Either::Right((value, _)) => Poll::Ready(value),
        }
    }
}

struct StalledWatcher {
    waker: AtomicWaker,
    done: AtomicBool,
}

struct UntilStalledData {
    watcher: Option<Arc<StalledWatcher>>,
}

/// Calls `f` with `&mut UntilStalledData` that is stored in `owner_data`.
///
/// # Panics
///
/// Panics if `owner_data` isn't an instance of `UntilStalledData`.
fn with_data<R>(f: impl Fn(&mut UntilStalledData) -> R) -> R {
    const MESSAGE: &str = "poll_until_stalled only works if the executor is being run \
                           with TestExecutor::run_until_stalled";
    f(&mut EHandle::local()
        .inner
        .owner_data
        .lock()
        .as_mut()
        .expect(MESSAGE)
        .downcast_mut::<UntilStalledData>()
        .expect(MESSAGE))
}

#[cfg(test)]
mod tests {
    use super::{super::spawn, *};
    use crate::{handle::on_signals::OnSignals, Interval, Timer};
    use assert_matches::assert_matches;
    use fuchsia_zircon::{self as zx, AsHandleRef, DurationNum};
    use futures::{future, task::LocalFutureObj, StreamExt};
    use pin_utils::pin_mut;
    use std::{
        cell::{Cell, RefCell},
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Poll, Waker},
    };

    // Runs a future that suspends and returns after being resumed.
    #[test]
    fn stepwise_two_steps() {
        let fut_step = Arc::new(Cell::new(0));
        let fut_waker: Arc<RefCell<Option<Waker>>> = Arc::new(RefCell::new(None));
        let fut_waker_clone = fut_waker.clone();
        let fut_step_clone = fut_step.clone();
        let fut_fn = move |cx: &mut Context<'_>| {
            fut_waker_clone.borrow_mut().replace(cx.waker().clone());
            match fut_step_clone.get() {
                0 => {
                    fut_step_clone.set(1);
                    Poll::Pending
                }
                1 => {
                    fut_step_clone.set(2);
                    Poll::Ready(())
                }
                _ => panic!("future called after done"),
            }
        };
        let fut = Box::new(future::poll_fn(fut_fn));
        let mut executor = TestExecutor::new_with_fake_time();
        // Spawn the future rather than waking it the main task because run_until_stalled will wake
        // the main future on every call, and we want to wake it ourselves using the waker.
        executor.local.inner.spawn_local(LocalFutureObj::new(fut));
        assert_eq!(fut_step.get(), 0);
        assert_eq!(executor.run_until_stalled(&mut future::pending::<()>()), Poll::Pending);
        assert_eq!(fut_step.get(), 1);

        fut_waker.borrow_mut().take().unwrap().wake();
        assert_eq!(executor.run_until_stalled(&mut future::pending::<()>()), Poll::Pending);
        assert_eq!(fut_step.get(), 2);
    }

    #[test]
    // Runs a future that waits on a timer.
    fn stepwise_timer() {
        let mut executor = TestExecutor::new_with_fake_time();
        executor.set_fake_time(Time::from_nanos(0));
        let fut = Timer::new(Time::after(1000.nanos()));
        pin_mut!(fut);

        let _ = executor.run_until_stalled(&mut fut);
        assert_eq!(Time::now(), Time::from_nanos(0));

        executor.set_fake_time(Time::from_nanos(1000));
        assert_eq!(Time::now(), Time::from_nanos(1000));
        assert!(executor.run_until_stalled(&mut fut).is_ready());
    }

    // Runs a future that waits on an event.
    #[test]
    fn stepwise_event() {
        let mut executor = TestExecutor::new_with_fake_time();
        let event = zx::Event::create();
        let fut = OnSignals::new(&event, zx::Signals::USER_0);
        pin_mut!(fut);

        let _ = executor.run_until_stalled(&mut fut);

        event.signal_handle(zx::Signals::NONE, zx::Signals::USER_0).unwrap();
        assert_matches!(executor.run_until_stalled(&mut fut), Poll::Ready(Ok(zx::Signals::USER_0)));
    }

    // Using `run_until_stalled` does not modify the order of events
    // compared to normal execution.
    #[test]
    fn run_until_stalled_preserves_order() {
        let mut executor = TestExecutor::new_with_fake_time();
        let spawned_fut_completed = Arc::new(AtomicBool::new(false));
        let spawned_fut_completed_writer = spawned_fut_completed.clone();
        let spawned_fut = Box::pin(async move {
            Timer::new(Time::after(5.seconds())).await;
            spawned_fut_completed_writer.store(true, Ordering::SeqCst);
        });
        let main_fut = async {
            Timer::new(Time::after(10.seconds())).await;
        };
        pin_mut!(main_fut);
        spawn(spawned_fut);
        assert_eq!(executor.run_until_stalled(&mut main_fut), Poll::Pending);
        executor.set_fake_time(Time::after(15.seconds()));
        // The timer in `spawned_fut` should fire first, then the
        // timer in `main_fut`.
        assert_eq!(executor.run_until_stalled(&mut main_fut), Poll::Ready(()));
        assert_eq!(spawned_fut_completed.load(Ordering::SeqCst), true);
    }

    #[test]
    fn task_destruction() {
        struct DropSpawner {
            dropped: Arc<AtomicBool>,
        }
        impl Drop for DropSpawner {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::SeqCst);
                let dropped_clone = self.dropped.clone();
                spawn(async {
                    // Hold on to a reference here to verify that it, too, is destroyed later
                    let _dropped_clone = dropped_clone;
                    panic!("task spawned in drop shouldn't be polled");
                });
            }
        }
        let mut dropped = Arc::new(AtomicBool::new(false));
        let drop_spawner = DropSpawner { dropped: dropped.clone() };
        let mut executor = TestExecutor::new();
        let main_fut = async move {
            spawn(async move {
                // Take ownership of the drop spawner
                let _drop_spawner = drop_spawner;
                future::pending::<()>().await;
            });
        };
        pin_mut!(main_fut);
        assert!(executor.run_until_stalled(&mut main_fut).is_ready());
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            false,
            "executor dropped pending task before destruction"
        );

        // Should drop the pending task and it's owned drop spawner,
        // as well as gracefully drop the future spawned from the drop spawner.
        drop(executor);
        let dropped = Arc::get_mut(&mut dropped)
            .expect("someone else is unexpectedly still holding on to a reference");
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            true,
            "executor did not drop pending task during destruction"
        );
    }

    #[test]
    fn time_now_real_time() {
        let _executor = LocalExecutor::new();
        let t1 = zx::Time::after(0.seconds());
        let t2 = Time::now().into_zx();
        let t3 = zx::Time::after(0.seconds());
        assert!(t1 <= t2);
        assert!(t2 <= t3);
    }

    #[test]
    fn time_now_fake_time() {
        let executor = TestExecutor::new_with_fake_time();
        let t1 = Time::from_zx(zx::Time::from_nanos(0));
        executor.set_fake_time(t1);
        assert_eq!(Time::now(), t1);

        let t2 = Time::from_zx(zx::Time::from_nanos(1000));
        executor.set_fake_time(t2);
        assert_eq!(Time::now(), t2);
    }

    #[test]
    fn time_after_overflow() {
        let executor = TestExecutor::new_with_fake_time();

        executor.set_fake_time(Time::INFINITE - 100.nanos());
        assert_eq!(Time::after(200.seconds()), Time::INFINITE);

        executor.set_fake_time(Time::INFINITE_PAST + 100.nanos());
        assert_eq!(Time::after((-200).seconds()), Time::INFINITE_PAST);
    }

    // This future wakes itself up a number of times during the same cycle
    async fn multi_wake(n: usize) {
        let mut done = false;
        futures::future::poll_fn(|cx| {
            if done {
                return Poll::Ready(());
            }
            for _ in 1..n {
                cx.waker().wake_by_ref()
            }
            done = true;
            Poll::Pending
        })
        .await;
    }

    #[test]
    fn dedup_wakeups() {
        let run = |n| {
            let mut executor = LocalExecutor::new();
            executor.run_singlethreaded(multi_wake(n));
            let snapshot = executor.inner.collector.snapshot();
            snapshot.wakeups_notification
        };
        assert_eq!(run(5), run(10)); // Same number of notifications independent of wakeup calls
    }

    // Ensure that a large amount of wakeups does not exhaust kernel resources,
    // such as the zx port queue limit.
    #[test]
    fn many_wakeups() {
        let mut executor = LocalExecutor::new();
        executor.run_singlethreaded(multi_wake(4096 * 2));
    }

    #[test]
    fn test_advance_to() {
        let mut executor = TestExecutor::new_with_fake_time();
        executor.set_fake_time(Time::from_nanos(0));

        let fut = async {
            let timer_fired = Arc::new(AtomicBool::new(false));
            futures::join!(
                async {
                    Timer::new(1.seconds()).await;
                    timer_fired.store(true, Ordering::SeqCst);
                },
                async {
                    let mut fired = 0;
                    let mut interval = Interval::new(1.seconds());
                    while let Some(_) = interval.next().await {
                        fired += 1;
                        if fired == 3 {
                            break;
                        }
                    }
                    assert_eq!(fired, 3);
                },
                async {
                    assert!(!timer_fired.load(Ordering::SeqCst));
                    TestExecutor::advance_to(Time::after(500.millis())).await;
                    // Timer still shouldn't be fired.
                    assert!(!timer_fired.load(Ordering::SeqCst));
                    TestExecutor::advance_to(Time::after(500.millis())).await;

                    // The timer should have fired.
                    assert!(timer_fired.load(Ordering::SeqCst));

                    // The interval timer should have fired once.  Make it fire twice more.
                    TestExecutor::advance_to(Time::after(2.seconds())).await;
                }
            )
        };
        pin_mut!(fut);
        assert!(executor.run_until_stalled(&mut fut).is_ready());
    }
}
